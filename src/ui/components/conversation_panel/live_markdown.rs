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

use crate::ui::components::messages::assistant_message::{AssistantMessage, render_reasoning};
use crate::ui::markdown_code_block::CodeCopyButton;
use async_openai::types::responses::OutputItem;
use ratatui::text::Text;
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
        let result = render_job(job);
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

fn render_job(job: LiveMarkdownJob) -> LiveMarkdownSnapshot {
    let (paragraph, copy_buttons, reasoning_text) = match &job.item {
        OutputItem::Message(_) => {
            let (paragraph, copy_buttons) = AssistantMessage::new(&job.item, job.width)
                .in_progress(job.in_progress)
                .expanded(job.expanded)
                .frame_count(job.frame_count)
                .into_paragraph();
            (paragraph, copy_buttons, None)
        }
        OutputItem::Reasoning(item) => {
            let (paragraph, copy_buttons, text) = render_reasoning(
                item,
                job.width,
                job.in_progress,
                job.expanded,
                job.frame_count,
            );
            (paragraph, copy_buttons, Some(Arc::new(text)))
        }
        // Keep a harmless empty result if a future caller accidentally submits
        // another item kind rather than parsing it on the UI thread.
        _ => (Paragraph::new(""), Vec::new(), None),
    };
    let height = paragraph.line_count(job.width) as u16;
    LiveMarkdownSnapshot {
        key: job.key,
        request_id: job.request_id,
        generation: job.generation,
        revision: job.revision,
        width: job.width,
        paragraph: Arc::new(paragraph),
        height,
        copy_buttons: Arc::new(copy_buttons),
        reasoning_text,
        in_progress: job.in_progress,
        expanded: job.expanded,
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveMarkdownJob, LiveMarkdownWorker};
    use async_openai::types::responses::{
        AssistantRole, OutputItem, OutputMessage, OutputMessageContent, OutputStatus,
        OutputTextContent, ReasoningItem, SummaryPart, SummaryTextContent,
    };

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
}
