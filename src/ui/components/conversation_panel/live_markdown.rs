// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Background rendering for the in-flight assistant Markdown message.
//!
//! The conversation widget is rendered on the application's event-loop thread.
//! Parsing a growing Markdown document (and especially syntax-highlighting a
//! growing fenced block) on that thread makes a long response visibly block
//! input and redraws.  This module is deliberately small: a single worker
//! keeps only the newest request per item, renders it off-thread, and publishes
//! only the newest completed result for each live item. The panel owns the front results
//! and may continue painting them while the worker prepares the next ones.

use crate::ui::components::messages::assistant::text::{
    markdown_source as message_markdown_source, markdown_width as message_markdown_width,
};
use crate::ui::components::messages::assistant_message::{
    AssistantMessage, render_reasoning, scan_copy_buttons_from_lines,
};
use crate::ui::markdown_code_block::{CodeBlockHooks, CodeCopyButton};
use crate::ui::markdown_theme::AppTheme;
use async_openai::types::responses::OutputItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::Widget;
use ratatui_markdown::markdown::MarkdownRenderer;
use ratatui_widgets::paragraph::Paragraph;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// An owned request sent to the background renderer.
#[derive(Debug)]
pub(crate) struct LiveMarkdownJob {
    pub(crate) key: String,
    /// Monotonic submission id, used to order same-revision jobs when the
    /// user toggles expansion while an older render is still running.
    pub(crate) request_id: u64,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) width: u16,
    pub(crate) item: OutputItem,
    pub(crate) in_progress: bool,
    pub(crate) expanded: bool,
    pub(crate) frame_count: u64,
}

/// A fully materialized front-buffer candidate.
#[derive(Debug, Clone)]
pub(crate) struct LiveMarkdownSnapshot {
    pub(crate) key: String,
    pub(crate) request_id: u64,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) width: u16,
    pub(crate) paragraph: Arc<Paragraph<'static>>,
    /// Incremental live messages render directly from persistent Markdown
    /// chunks. Final snapshots still use `paragraph` for the history cache.
    pub(crate) incremental: Option<Arc<IncrementalMarkdownParagraph>>,
    pub(crate) height: u16,
    pub(crate) copy_buttons: Arc<Vec<CodeCopyButton>>,
    /// Unwrapped reasoning lines, when this snapshot represents a reasoning
    /// item embedded inside a live tool group.
    pub(crate) reasoning_text: Option<Arc<Text<'static>>>,
    pub(crate) in_progress: bool,
    pub(crate) expanded: bool,
}

#[derive(Debug, Default)]
struct WorkerState {
    /// One pending job per output item. A new delta replaces only that item's
    /// pending snapshot, so a later reasoning block cannot starve an older one.
    pending_jobs: HashMap<String, (u64, LiveMarkdownJob)>,
    /// Jobs currently being parsed. Keeping their metadata here prevents the
    /// UI from cloning a newer full item every frame while the old parse is
    /// still running; the next revision is queued after this one publishes.
    in_flight: HashMap<String, (u64, u64, u16, bool)>,
    /// Completed snapshots are retained per item until the UI takes them.
    /// Keeping a map avoids losing a fast result when several items finish
    /// before the next terminal frame.
    latest_results: HashMap<String, LiveMarkdownSnapshot>,
    next_order: u64,
    shutdown: bool,
}

#[derive(Debug, Default)]
struct IncrementalMarkdownState {
    generation: u64,
    width: u16,
    source_len: usize,
    scan_offset: usize,
    in_fence: bool,
    stable_end: usize,
    stable: Option<Arc<StableMarkdownChunk>>,
}

#[derive(Debug, Default)]
struct CachedIncrementalMarkdown {
    last_used: u64,
    state: IncrementalMarkdownState,
}

struct IncrementalMarkdownRender {
    stable: Option<Arc<StableMarkdownChunk>>,
    tail_lines: Arc<[Line<'static>]>,
    #[cfg(test)]
    tail_codes: Arc<[String]>,
    tail_buttons: Arc<[CodeCopyButton]>,
    line_count: u16,
    #[cfg(test)]
    reparsed_bytes: usize,
}

#[derive(Debug)]
struct StableMarkdownChunk {
    previous: Option<Arc<StableMarkdownChunk>>,
    lines: Arc<[Line<'static>]>,
    #[cfg(test)]
    codes: Arc<[String]>,
    buttons: Arc<[CodeCopyButton]>,
    total_lines: u16,
    source_end: usize,
}

#[cfg(test)]
impl IncrementalMarkdownRender {
    fn lines(&self) -> Vec<Line<'static>> {
        let mut chunks = Vec::new();
        let mut current = self.stable.as_deref();
        while let Some(chunk) = current {
            chunks.push(chunk);
            current = chunk.previous.as_deref();
        }
        chunks.reverse();
        chunks
            .into_iter()
            .flat_map(|chunk| chunk.lines.iter().cloned())
            .chain(self.tail_lines.iter().cloned())
            .collect()
    }

    fn codes(&self) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = self.stable.as_deref();
        while let Some(chunk) = current {
            chunks.push(chunk);
            current = chunk.previous.as_deref();
        }
        chunks.reverse();
        chunks
            .into_iter()
            .flat_map(|chunk| chunk.codes.iter().cloned())
            .chain(self.tail_codes.iter().cloned())
            .collect()
    }
}

/// A live Markdown paragraph backed by persistent stable chunks plus one
/// replaceable tail. Publishing a new token snapshot only clones the head
/// `Arc`; it never clones the already-rendered lines in the prefix.
#[derive(Debug)]
pub(crate) struct IncrementalMarkdownParagraph {
    stable: Option<Arc<StableMarkdownChunk>>,
    tail_lines: Arc<[Line<'static>]>,
    tail_buttons: Arc<[CodeCopyButton]>,
    height: u16,
}

impl IncrementalMarkdownParagraph {
    fn new(rendered: IncrementalMarkdownRender) -> Self {
        Self {
            stable: rendered.stable,
            tail_lines: rendered.tail_lines,
            tail_buttons: rendered.tail_buttons,
            height: rendered.line_count,
        }
    }

    pub(crate) fn height(&self) -> u16 {
        self.height
    }

    /// Paint only the requested logical rows. Markdown output is already
    /// wrapped for the content width, so individual borrowed `Line`s can be
    /// rendered without materializing a conversation-sized `Text`.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, source_offset: u16) {
        if area.width <= 2 || area.height == 0 {
            return;
        }
        let start = source_offset;
        let end = start.saturating_add(area.height).min(self.height);
        self.for_each_line(start, end, |row, line| {
            let destination_y = area.y.saturating_add(row.saturating_sub(start));
            line.render(Rect::new(area.x + 1, destination_y, area.width - 2, 1), buf);
        });
    }

    pub(crate) fn copy_button(&self, row: u16, x: u16) -> Option<String> {
        let mut row_offset = 0u16;
        for chunk in self.stable_chunks() {
            if let Some(content) = find_chunk_button(&chunk.buttons, row, x, row_offset) {
                return Some(content);
            }
            row_offset = row_offset.saturating_add(chunk.lines.len() as u16);
        }
        find_chunk_button(&self.tail_buttons, row, x, row_offset)
    }

    fn stable_chunks(&self) -> Vec<&StableMarkdownChunk> {
        let mut chunks = Vec::new();
        let mut current = self.stable.as_deref();
        while let Some(chunk) = current {
            chunks.push(chunk);
            current = chunk.previous.as_deref();
        }
        chunks.reverse();
        chunks
    }

    fn for_each_line(&self, start: u16, end: u16, mut visit: impl FnMut(u16, &Line<'static>)) {
        let mut row = 0u16;
        for chunk in self.stable_chunks() {
            let chunk_end = row.saturating_add(chunk.lines.len() as u16);
            if chunk_end <= start {
                row = chunk_end;
                continue;
            }
            let local_start = start.saturating_sub(row) as usize;
            for (local_row, line) in chunk.lines.iter().enumerate().skip(local_start) {
                let absolute_row = row.saturating_add(local_row as u16);
                if absolute_row >= end {
                    return;
                }
                visit(absolute_row, line);
            }
            row = chunk_end;
        }
        let local_start = start.saturating_sub(row) as usize;
        for (local_row, line) in self.tail_lines.iter().enumerate().skip(local_start) {
            let absolute_row = row.saturating_add(local_row as u16);
            if absolute_row >= end {
                return;
            }
            visit(absolute_row, line);
        }
    }
}

fn find_chunk_button(
    buttons: &[CodeCopyButton],
    row: u16,
    x: u16,
    row_offset: u16,
) -> Option<String> {
    buttons
        .iter()
        .find(|button| {
            row == row_offset.saturating_add(button.row) && x >= button.x_start && x < button.x_end
        })
        .map(|button| button.content.clone())
}

impl IncrementalMarkdownState {
    fn render(&mut self, source: &str, generation: u64, width: u16) -> IncrementalMarkdownRender {
        let append_only =
            self.generation == generation && self.width == width && source.len() >= self.source_len;
        if !append_only {
            self.generation = generation;
            self.width = width;
            self.source_len = 0;
            self.scan_offset = 0;
            self.in_fence = false;
            self.stable_end = 0;
            self.stable = None;
        }

        self.scan_appended_lines(source);
        let next_stable_end = self.stable_end;
        let rendered_stable_end = self.stable.as_ref().map_or(0, |chunk| chunk.source_end);
        #[cfg(test)]
        let old_stable_end = rendered_stable_end;
        if next_stable_end > rendered_stable_end {
            let stable_delta = &source[rendered_stable_end..next_stable_end];
            let (lines, codes) = render_markdown(stable_delta, width);
            let buttons = scan_copy_buttons_from_lines(&lines, &codes);
            let new_lines = lines.len() as u16;
            let previous_lines = self.stable.as_ref().map_or(0, |chunk| chunk.total_lines);
            self.stable = Some(Arc::new(StableMarkdownChunk {
                previous: self.stable.take(),
                lines: lines.into(),
                #[cfg(test)]
                codes: codes.into(),
                buttons: buttons.into(),
                total_lines: previous_lines.saturating_add(new_lines),
                source_end: next_stable_end,
            }));
        }

        let tail = &source[self.stable_end..];
        let (tail_lines, tail_codes) = render_markdown(tail, width);
        let tail_buttons = scan_copy_buttons_from_lines(&tail_lines, &tail_codes);
        let stable_lines = self.stable.as_ref().map_or(0, |chunk| chunk.total_lines);
        let line_count = stable_lines.saturating_add(tail_lines.len() as u16);
        self.source_len = source.len();

        IncrementalMarkdownRender {
            stable: self.stable.clone(),
            tail_lines: tail_lines.into(),
            #[cfg(test)]
            tail_codes: tail_codes.into(),
            tail_buttons: tail_buttons.into(),
            line_count,
            #[cfg(test)]
            reparsed_bytes: next_stable_end.saturating_sub(old_stable_end) + tail.len(),
        }
    }

    fn scan_appended_lines(&mut self, source: &str) {
        let mut offset = self.scan_offset;
        for line in source[self.scan_offset..].split_inclusive('\n') {
            if !line.ends_with('\n') {
                break;
            }
            offset += line.len();
            let trimmed = line.trim();
            if let Some(after_fence) = trimmed.strip_prefix("```") {
                if !self.in_fence || after_fence.trim().is_empty() {
                    self.in_fence = !self.in_fence;
                }
            } else if !self.in_fence && trimmed.is_empty() {
                self.stable_end = offset;
            }
            self.scan_offset = offset;
        }
    }
}

/// Byte boundary after the last completed block separator. A blank line can
/// seal everything before it only while it is outside a fenced code block;
/// the unfinished tail remains free to change type as more tokens arrive.
#[cfg(test)]
fn stable_markdown_prefix_end(source: &str) -> usize {
    let mut state = IncrementalMarkdownState::default();
    state.scan_appended_lines(source);
    state.stable_end
}

fn render_markdown(source: &str, width: u16) -> (Vec<Line<'static>>, Vec<String>) {
    if source.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let hooks = CodeBlockHooks::new(width as usize);
    let codes_handle = hooks.codes();
    let renderer = MarkdownRenderer::new(width as usize).with_render_hooks(Box::new(hooks));
    let blocks = renderer.parse(source);
    let lines = renderer.render(&blocks, &AppTheme);
    let codes = codes_handle
        .lock()
        .map(|codes| codes.clone())
        .unwrap_or_default();
    (lines, codes)
}

/// A one-worker latest-wins renderer.
///
/// A condition-variable worker is used instead of spawning one blocking task
/// per token.  When tokens arrive faster than Markdown can be rendered, a new
/// request replaces the pending request for that item. The queue is still
/// bounded by the number of live Markdown items, and completed snapshots are
/// retained once per item.
pub(crate) struct LiveMarkdownWorker {
    shared: Arc<(Mutex<WorkerState>, Condvar)>,
    /// Dropping a `JoinHandle` detaches the worker. `Drop` below still signals
    /// shutdown, so a worker that is waiting exits promptly; a render already in
    /// progress is allowed to finish without blocking application teardown.
    thread: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for LiveMarkdownWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveMarkdownWorker")
            .field("thread_running", &self.thread.is_some())
            .finish()
    }
}

impl LiveMarkdownWorker {
    pub(crate) fn new() -> Self {
        let shared = Arc::new((Mutex::new(WorkerState::default()), Condvar::new()));
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("programmer-live-markdown".to_string())
            .spawn(move || worker_loop(worker_shared))
            .expect("live Markdown worker thread should start");
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Replace the pending snapshot for this item with the newest request.
    pub(crate) fn submit(&self, mut job: LiveMarkdownJob) {
        let (state, wake) = &*self.shared;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.shutdown {
            return;
        }
        state.next_order = state.next_order.wrapping_add(1);
        let order = state.next_order;
        job.request_id = order;
        let old = state.pending_jobs.insert(job.key.clone(), (order, job));
        wake.notify_one();
        // Release the worker mutex before dropping a superseded large
        // OutputItem. Deallocating a growing response can itself take enough
        // time to make the render thread wait on this lock.
        drop(state);
        drop(old);
    }

    /// Take one completed result, if the worker has published one.
    pub(crate) fn take_result(&self) -> Option<LiveMarkdownSnapshot> {
        let (state, _) = &*self.shared;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = state.latest_results.keys().next()?.clone();
        state.latest_results.remove(&key)
    }

    /// Whether this item already has a request for the same view state being
    /// rendered. While that request is in flight, the panel can keep painting
    /// its front snapshot instead of cloning another full growing item that
    /// would immediately replace the pending job.
    pub(crate) fn has_pending_view(
        &self,
        key: &str,
        generation: u64,
        width: u16,
        expanded: bool,
    ) -> bool {
        let (state, _) = &*self.shared;
        let state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let same_view = |job_generation, _job_revision, job_width, job_expanded| {
            job_generation == generation && job_width == width && job_expanded == expanded
        };
        state.pending_jobs.get(key).is_some_and(|(_, job)| {
            same_view(job.generation, job.revision, job.width, job.expanded)
        }) || state.in_flight.get(key).is_some_and(
            |&(job_generation, job_revision, job_width, job_expanded)| {
                same_view(job_generation, job_revision, job_width, job_expanded)
            },
        )
    }
}

impl Drop for LiveMarkdownWorker {
    fn drop(&mut self) {
        let (state, wake) = &*self.shared;
        if let Ok(mut state) = state.lock() {
            state.shutdown = true;
            state.pending_jobs.clear();
            state.latest_results.clear();
            wake.notify_one();
        }
        // Deliberately detach. Joining here could make quitting wait for a
        // large syntax-highlight operation that was already in progress.
        let _ = self.thread.take();
    }
}

fn worker_loop(shared: Arc<(Mutex<WorkerState>, Condvar)>) {
    const MAX_RENDER_STATES: usize = 64;
    let mut render_states: HashMap<String, CachedIncrementalMarkdown> = HashMap::new();
    let mut render_order = 0u64;
    loop {
        let job = {
            let (state, wake) = &*shared;
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while state.pending_jobs.is_empty() && !state.shutdown {
                state = wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if state.shutdown {
                return;
            }
            // Process completed/final snapshots first so a response hand-off
            // can settle its status promptly, then process the oldest pending
            // key. A continuously growing item may replace its pending job on
            // every frame; FIFO selection prevents it from starving an older
            // reasoning block.
            let key = state
                .pending_jobs
                .iter()
                .min_by_key(|(_, (order, job))| (job.in_progress, *order))
                .map(|(key, _)| key.clone());
            key.and_then(|key| {
                state.pending_jobs.remove(&key).map(|(_, job)| {
                    state
                        .in_flight
                        .insert(key, (job.generation, job.revision, job.width, job.expanded));
                    job
                })
            })
        };

        let Some(job) = job else { continue };
        render_order = render_order.wrapping_add(1);
        let key = job.key.clone();
        let finished = !job.in_progress;
        let result = {
            let cached = render_states.entry(key.clone()).or_default();
            cached.last_used = render_order;
            render_job(job, &mut cached.state)
        };
        if finished {
            // A finalized item cannot receive more token deltas. Its immutable
            // snapshot remains available to the panel; the extra raw source
            // and stable-prefix cache no longer need to stay in the worker.
            render_states.remove(&key);
        } else if render_states.len() > MAX_RENDER_STATES
            && let Some(oldest) = render_states
                .iter()
                .filter(|(candidate, _)| *candidate != &key)
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key.clone())
        {
            render_states.remove(&oldest);
        }
        let (state, _) = &*shared;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.in_flight.remove(&result.key);
        if state.shutdown {
            return;
        }
        // Keep the newest finished result for each item. The UI validates
        // generation, revision, and width before swapping it into the front
        // buffer.
        let replace = state
            .latest_results
            .get(&result.key)
            .is_none_or(|old| result.request_id >= old.request_id);
        let old = replace.then(|| state.latest_results.insert(result.key.clone(), result));
        drop(state);
        // Paragraph/Text deallocation belongs to the worker, outside the
        // shared mutex, so `take_result` never waits behind a large drop.
        drop(old.flatten());
    }
}

fn render_job(
    job: LiveMarkdownJob,
    markdown_state: &mut IncrementalMarkdownState,
) -> LiveMarkdownSnapshot {
    let (paragraph, incremental, copy_buttons, reasoning_text) = match &job.item {
        OutputItem::Message(message) if job.in_progress => {
            let source = message_markdown_source(message);
            let rendered =
                markdown_state.render(&source, job.generation, message_markdown_width(job.width));
            (
                Paragraph::new(""),
                Some(Arc::new(IncrementalMarkdownParagraph::new(rendered))),
                Vec::new(),
                None,
            )
        }
        OutputItem::Message(_) => {
            let (paragraph, copy_buttons) = AssistantMessage::new(&job.item, job.width)
                .in_progress(job.in_progress)
                .expanded(job.expanded)
                .frame_count(job.frame_count)
                .into_paragraph();
            (paragraph, None, copy_buttons, None)
        }
        OutputItem::Reasoning(item) => {
            let (paragraph, copy_buttons, text) = render_reasoning(
                item,
                job.width,
                job.in_progress,
                job.expanded,
                job.frame_count,
            );
            (paragraph, None, copy_buttons, Some(Arc::new(text)))
        }
        // Keep a harmless empty result if a future caller accidentally submits
        // another item kind rather than parsing it on the UI thread.
        _ => (Paragraph::new(""), None, Vec::new(), None),
    };
    let height = incremental.as_ref().map_or_else(
        || paragraph.line_count(job.width) as u16,
        |view| view.height(),
    );
    LiveMarkdownSnapshot {
        key: job.key,
        request_id: job.request_id,
        generation: job.generation,
        revision: job.revision,
        width: job.width,
        paragraph: Arc::new(paragraph),
        incremental,
        height,
        copy_buttons: Arc::new(copy_buttons),
        reasoning_text,
        in_progress: job.in_progress,
        expanded: job.expanded,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AssistantMessage, IncrementalMarkdownParagraph, IncrementalMarkdownState, LiveMarkdownJob,
        LiveMarkdownWorker, message_markdown_width, render_markdown, stable_markdown_prefix_end,
    };
    use async_openai::types::responses::{
        AssistantRole, OutputItem, OutputMessage, OutputMessageContent, OutputStatus,
        OutputTextContent, ReasoningItem, SummaryPart, SummaryTextContent,
    };
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;
    use std::sync::Arc;

    fn message(text: &str) -> async_openai::types::responses::OutputItem {
        async_openai::types::responses::OutputItem::Message(OutputMessage {
            content: vec![OutputMessageContent::OutputText(OutputTextContent {
                annotations: Vec::new(),
                logprobs: None,
                text: text.to_string(),
            })],
            id: "message-test".to_string(),
            role: AssistantRole::Assistant,
            phase: None,
            status: OutputStatus::InProgress,
        })
    }

    fn reasoning(text: &str) -> OutputItem {
        OutputItem::Reasoning(ReasoningItem {
            id: Some("reasoning-test".to_string()),
            summary: vec![SummaryPart::SummaryText(SummaryTextContent {
                text: text.to_string(),
            })],
            content: None,
            encrypted_content: None,
            status: None,
        })
    }

    #[test]
    fn worker_replaces_pending_jobs_with_latest_revision() {
        let worker = LiveMarkdownWorker::new();
        worker.submit(LiveMarkdownJob {
            key: "message-test".into(),
            request_id: 0,
            generation: 1,
            revision: 1,
            width: 40,
            item: message("old"),
            in_progress: true,
            expanded: false,
            frame_count: 0,
        });
        worker.submit(LiveMarkdownJob {
            key: "message-test".into(),
            request_id: 0,
            generation: 1,
            revision: 2,
            width: 40,
            item: message("new"),
            in_progress: true,
            expanded: false,
            frame_count: 0,
        });

        let mut result = None;
        for _ in 0..100 {
            if let Some(candidate) = worker.take_result() {
                result = Some(candidate);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let result = result.expect("worker should publish a result");
        assert_eq!(result.revision, 2);
        assert_eq!(result.width, 40);
        assert!(result.height > 0);
    }

    #[test]
    fn worker_keeps_results_for_multiple_live_items() {
        let worker = LiveMarkdownWorker::new();
        for (key, text) in [("message-a", "first"), ("message-b", "second")] {
            worker.submit(LiveMarkdownJob {
                key: key.into(),
                request_id: 0,
                generation: 1,
                revision: 1,
                width: 40,
                item: message(text),
                in_progress: true,
                expanded: false,
                frame_count: 0,
            });
        }

        let mut keys = Vec::new();
        for _ in 0..200 {
            while let Some(result) = worker.take_result() {
                keys.push(result.key);
            }
            if keys.len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        keys.sort();
        assert_eq!(keys, ["message-a", "message-b"]);
    }

    #[test]
    fn worker_renders_expanded_reasoning_off_thread() {
        let worker = LiveMarkdownWorker::new();
        worker.submit(LiveMarkdownJob {
            key: "reasoning-test".into(),
            request_id: 0,
            generation: 4,
            revision: 7,
            width: 40,
            item: reasoning("# Heading\n\nlong reasoning body"),
            in_progress: true,
            expanded: true,
            frame_count: 0,
        });

        let mut result = None;
        for _ in 0..100 {
            if let Some(candidate) = worker.take_result() {
                result = Some(candidate);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let result = result.expect("worker should publish reasoning");
        assert!(result.expanded);
        assert!(
            result
                .reasoning_text
                .as_ref()
                .is_some_and(|text| text.to_string().contains("Heading"))
        );
    }

    #[test]
    fn incremental_renderer_reparses_only_the_unsealed_markdown_tail() {
        let mut state = IncrementalMarkdownState::default();
        let first = "# Stable heading\n\nfirst streaming paragraph";
        let rendered = state.render(first, 1, 40);
        assert_eq!(rendered.reparsed_bytes, first.len());
        assert_eq!(state.stable_end, "# Stable heading\n\n".len());
        let stable_prefix = rendered.stable.as_ref().expect("sealed prefix").clone();

        let appended = format!("{first} gets one more token");
        let rendered = state.render(&appended, 1, 40);
        assert_eq!(
            rendered.reparsed_bytes,
            "first streaming paragraph gets one more token".len()
        );
        assert!(rendered.reparsed_bytes < appended.len());
        assert!(Arc::ptr_eq(
            &stable_prefix,
            rendered.stable.as_ref().expect("shared sealed prefix")
        ));
        assert!(
            rendered
                .lines()
                .iter()
                .any(|line| line.to_string().contains("Stable heading"))
        );
    }

    #[test]
    fn fenced_blank_lines_do_not_seal_an_incomplete_code_block() {
        let source = "intro\n\n```rust\nfn main() {\n\n";
        assert_eq!(stable_markdown_prefix_end(source), "intro\n\n".len());

        let closed = format!("{source}}}\n```\n\nafter");
        assert_eq!(
            stable_markdown_prefix_end(&closed),
            closed.len() - "after".len()
        );
    }

    #[test]
    fn incremental_blocks_match_a_full_document_render() {
        let source = concat!(
            "# Heading\n\n",
            "A paragraph with **bold** text.\n\n",
            "- first\n- second\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "| a | b |\n| - | - |\n| 1 | 2 |\n\n",
            "tail"
        );
        let mut state = IncrementalMarkdownState::default();
        let mut last = None;
        for boundary in source
            .match_indices("\n\n")
            .map(|(index, separator)| index + separator.len())
            .chain(std::iter::once(source.len()))
        {
            last = Some(state.render(&source[..boundary], 3, 60));
        }
        let incremental = last.unwrap();
        let (full_lines, full_codes) = render_markdown(source, 60);
        assert_eq!(incremental.lines(), full_lines);
        assert_eq!(incremental.codes(), full_codes);
    }

    #[test]
    fn incremental_view_matches_the_full_message_pixels_without_flattening_prefix() {
        let source = concat!(
            "# Heading\n\n",
            "A paragraph with **bold** text.\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "streaming tail"
        );
        let item = message(source);
        let width = 60;
        let mut state = IncrementalMarkdownState::default();
        let rendered = state.render(source, 9, message_markdown_width(width));
        let incremental = IncrementalMarkdownParagraph::new(rendered);
        let (full, _) = AssistantMessage::new(&item, width).into_paragraph();
        let height = full.line_count(width) as u16;
        assert_eq!(incremental.height(), height);

        let area = Rect::new(0, 0, width, height);
        let mut incremental_buffer = Buffer::empty(area);
        incremental.render(area, &mut incremental_buffer, 0);
        let mut full_buffer = Buffer::empty(area);
        full.render(area, &mut full_buffer);

        assert_eq!(incremental_buffer, full_buffer);
    }
}
