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

use crate::commands::CompletionState;
use async_openai::types::responses::InputImageContent;
use ratatui::style::{Color, Modifier, Style};
use ratatui_textarea::{CursorMove, Input, TextArea};

#[derive(Debug, Clone)]
pub struct InputPanel<'a> {
    pub text_area: TextArea<'a>,
    /// Tab-completion state, set when the user types a slash command.
    pub completion: Option<CompletionState>,
    /// Command history (most recent at the end).
    pub history: Vec<String>,
    /// Current position in the history when navigating (-1 means "below the last entry", i.e. empty).
    pub history_index: i64,
    /// Large pastes collapsed into placeholders: `(placeholder, full content)`.
    /// Expanded back into the text when the message is sent.
    pub pastes: Vec<(String, String)>,
    /// Clipboard images associated with placeholders still present in the draft.
    images: Vec<(String, InputImageContent)>,
    next_image_id: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct InputDraft {
    content: String,
    pastes: Vec<(String, String)>,
    images: Vec<(String, InputImageContent)>,
    next_image_id: usize,
}

impl InputPanel<'_> {
    pub fn new() -> Self {
        let mut text_area = TextArea::default();

        text_area.set_style(Style::default().fg(Color::White));
        text_area.set_cursor_line_style(Style::default());
        text_area.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        text_area.set_placeholder_text("Talk with the programmer…");
        text_area.set_placeholder_style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        );

        InputPanel {
            text_area,
            completion: None,
            history: Vec::new(),
            history_index: -1,
            pastes: Vec::new(),
            images: Vec::new(),
            next_image_id: 1,
        }
    }

    pub fn get_content(&self) -> String {
        self.text_area.lines().join("\n")
    }

    /// Maximum number of text rows the input grows to before it stops
    /// expanding and scrolls internally instead.
    pub const MAX_VISIBLE_LINES: usize = 10;

    /// Total height (including the top + bottom border) the panel needs to show
    /// its current content. Grows with multi-line input up to
    /// [`Self::MAX_VISIBLE_LINES`] so long messages get room without letting the
    /// input take over the whole screen.
    pub fn needed_height(&self) -> u16 {
        let lines = self
            .text_area
            .lines()
            .len()
            .clamp(1, Self::MAX_VISIBLE_LINES);
        lines as u16 + 2 // top + bottom border rows
    }

    /// The input content with paste placeholders expanded to their full text.
    pub fn expanded_content(&self) -> String {
        let mut text = self.get_content();
        for (placeholder, content) in &self.pastes {
            text = text.replace(placeholder.as_str(), content.as_str());
        }
        text
    }

    pub(crate) fn draft_snapshot(&self) -> InputDraft {
        InputDraft {
            content: self.get_content(),
            pastes: self.pastes.clone(),
            images: self.images.clone(),
            next_image_id: self.next_image_id,
        }
    }

    pub(crate) fn restore_draft(&mut self, draft: InputDraft) {
        self.clear();
        self.text_area.insert_str(&draft.content);
        self.pastes = draft.pastes;
        self.images = draft.images;
        self.next_image_id = draft.next_image_id;
        self.history_index = -1;
    }

    pub(crate) fn remove_last_history_if(&mut self, value: &str) {
        if self.history.last().is_some_and(|entry| entry == value) {
            self.history.pop();
        }
        self.history_index = -1;
    }

    /// Insert pasted text at the cursor as-is.
    pub fn insert_str(&mut self, text: &str) {
        self.text_area.insert_str(text);
        // Editing the text leaves history-navigation mode.
        self.history_index = -1;
    }

    /// Collapse a large paste into a `[Pasted text #N +M lines]` placeholder at
    /// the cursor. The real content is restored by [`Self::expanded_content`].
    pub fn add_paste(&mut self, content: String) {
        let lines = content.lines().count().max(1);
        let placeholder = format!("[Pasted text #{} +{} lines]", self.pastes.len() + 1, lines);
        self.insert_str(&placeholder);
        self.pastes.push((placeholder, content));
    }

    /// Add a clipboard image at the cursor. Removing its placeholder before
    /// sending also removes the associated attachment.
    pub fn add_image(&mut self, image: InputImageContent, width: usize, height: usize) -> bool {
        let content = self.get_content();
        self.images
            .retain(|(placeholder, _)| content.contains(placeholder));
        if self.images.len() >= crate::commands::MAX_IMAGES_PER_MESSAGE {
            return false;
        }

        let placeholder = format!("[Pasted image #{} {width}x{height}]", self.next_image_id);
        self.next_image_id += 1;
        self.insert_str(&placeholder);
        self.images.push((placeholder, image));
        true
    }

    /// Drain attachments whose placeholders are still present in the draft.
    pub fn take_images(&mut self) -> Vec<InputImageContent> {
        let content = self.get_content();
        std::mem::take(&mut self.images)
            .into_iter()
            .filter_map(|(placeholder, image)| content.contains(&placeholder).then_some(image))
            .collect()
    }

    pub(crate) fn placeholders(&self) -> impl Iterator<Item = &str> {
        self.pastes
            .iter()
            .map(|(placeholder, _)| placeholder.as_str())
            .chain(
                self.images
                    .iter()
                    .map(|(placeholder, _)| placeholder.as_str()),
            )
    }

    pub fn delete_placeholder_backward(&mut self) -> bool {
        self.delete_placeholder_at_cursor(true)
    }

    pub fn delete_placeholder_forward(&mut self) -> bool {
        self.delete_placeholder_at_cursor(false)
    }

    fn delete_placeholder_at_cursor(&mut self, backward: bool) -> bool {
        if self.text_area.selection_range().is_some() {
            return false;
        }
        let cursor = self.text_area.cursor();
        let (row, cursor) = (cursor.0, cursor.1);
        let Some(line) = self.text_area.lines().get(row) else {
            return false;
        };

        let mut matched = None;
        'placeholders: for placeholder in self.placeholders() {
            for (byte_start, _) in line.match_indices(placeholder) {
                let start = line[..byte_start].chars().count();
                let end = start + placeholder.chars().count();
                let touches_placeholder = if backward {
                    cursor > start && cursor <= end
                } else {
                    cursor >= start && cursor < end
                };
                if touches_placeholder {
                    matched = Some((placeholder.to_string(), start, end - start));
                    break 'placeholders;
                }
            }
        }

        let Some((placeholder, start, len)) = matched else {
            return false;
        };
        let (Ok(row), Ok(start)) = (u16::try_from(row), u16::try_from(start)) else {
            return false;
        };
        self.text_area.move_cursor(CursorMove::Jump(row, start));
        self.text_area.delete_str(len);
        self.pastes.retain(|(value, _)| value != &placeholder);
        self.images.retain(|(value, _)| value != &placeholder);
        self.history_index = -1;
        true
    }

    pub fn input(&mut self, input: impl Into<Input>) -> bool {
        let modified = self.text_area.input(input);
        if modified {
            // Editing the text leaves history-navigation mode.
            self.history_index = -1;
        }
        modified
    }

    /// Insert a newline at the cursor.
    pub fn insert_newline(&mut self) {
        self.text_area.insert_newline();
        // Editing the text leaves history-navigation mode.
        self.history_index = -1;
    }

    /// True when the cursor is on the first line of the text area.
    pub fn cursor_on_first_line(&self) -> bool {
        self.text_area.cursor().0 == 0
    }

    /// True when the cursor is on the last line of the text area.
    pub fn cursor_on_last_line(&self) -> bool {
        self.text_area.cursor().0 + 1 == self.text_area.lines().len()
    }

    pub fn clear(&mut self) -> bool {
        self.pastes.clear();
        self.images.clear();
        self.next_image_id = 1;
        self.completion = None;
        self.text_area.clear()
    }

    /// Replace the entire content of the text area with `text`.
    pub fn set_content(&mut self, text: &str) {
        self.text_area.clear();
        self.text_area.insert_str(text);
    }

    /// Push a message to the history (after sending).
    pub fn push_history(&mut self, text: String) {
        // Don't push duplicates of the last entry.
        if self.history.last() != Some(&text) {
            self.history.push(text);
        }
        self.history_index = -1;
    }

    /// True while the input shows the history entry at `history_index` unmodified,
    /// i.e. the user is currently navigating history. Editing or clearing the
    /// recalled text leaves navigation mode.
    pub fn is_navigating_history(&self) -> bool {
        self.history_index >= 0
            && self
                .history
                .get(self.history_index as usize)
                .map(String::as_str)
                == Some(self.get_content().as_str())
    }

    /// Navigate history: up = older, down = newer.
    /// Returns true if the input was updated.
    pub fn history_up(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        if self.is_navigating_history() {
            if self.history_index == 0 {
                return false; // Already at the oldest entry.
            }
            self.history_index -= 1;
        } else {
            // Start (or restart) navigation from the most recent entry.
            self.history_index = self.history.len() as i64 - 1;
        }
        let text = self.history[self.history_index as usize].clone();
        self.set_content(&text);
        true
    }

    /// Navigate history forward. Returns true if the input was updated.
    pub fn history_down(&mut self) -> bool {
        if !self.is_navigating_history() {
            return false;
        }
        let len = self.history.len() as i64;
        if self.history_index < len - 1 {
            self.history_index += 1;
            let text = self.history[self.history_index as usize].clone();
            self.set_content(&text);
        } else {
            // Past the most recent entry — clear input.
            self.history_index = -1;
            self.clear();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CompletionCandidate;
    use async_openai::types::responses::ImageDetail;

    fn image() -> InputImageContent {
        InputImageContent {
            detail: ImageDetail::Auto,
            file_id: None,
            image_url: Some("data:image/png;base64,AAAA".to_string()),
        }
    }

    #[test]
    fn clear_dismisses_completion_state() {
        let mut panel = InputPanel::new();
        panel.insert_str("@");
        panel.completion = Some(CompletionState {
            prefix: "@".to_string(),
            candidates: vec![CompletionCandidate {
                value: "diagnostics".to_string(),
                label: "All diagnostics".to_string(),
            }],
            selected: 0,
            visible: true,
            scroll_offset: 0,
        });

        panel.clear();

        assert!(panel.get_content().is_empty());
        assert!(panel.completion.is_none());
    }

    #[test]
    fn image_placeholder_controls_attachment() {
        let mut panel = InputPanel::new();
        assert!(panel.add_image(image(), 640, 480));
        assert_eq!(panel.get_content(), "[Pasted image #1 640x480]");
        assert_eq!(panel.take_images().len(), 1);

        assert!(panel.add_image(image(), 320, 200));
        panel.set_content("placeholder removed");
        assert!(panel.take_images().is_empty());
    }

    #[test]
    fn draft_snapshot_restores_pastes_and_images_after_send_drains_them() {
        let mut panel = InputPanel::new();
        panel.add_paste("first\nsecond".to_string());
        assert!(panel.add_image(image(), 640, 480));
        let expected_content = panel.get_content();
        let expected_expanded = panel.expanded_content();
        let draft = panel.draft_snapshot();

        assert_eq!(panel.take_images().len(), 1);
        panel.clear();
        panel.restore_draft(draft);

        assert_eq!(panel.get_content(), expected_content);
        assert_eq!(panel.expanded_content(), expected_expanded);
        assert_eq!(panel.take_images().len(), 1);
    }

    #[test]
    fn backspace_removes_an_entire_paste_placeholder() {
        let mut panel = InputPanel::new();
        panel.add_paste("first\nsecond".to_string());

        assert!(panel.delete_placeholder_backward());
        assert!(panel.get_content().is_empty());
        assert!(panel.pastes.is_empty());
    }

    #[test]
    fn delete_removes_an_entire_image_placeholder() {
        let mut panel = InputPanel::new();
        assert!(panel.add_image(image(), 640, 480));
        panel.text_area.move_cursor(CursorMove::Jump(0, 0));

        assert!(panel.delete_placeholder_forward());
        assert!(panel.get_content().is_empty());
        assert!(panel.take_images().is_empty());
    }
}
