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
use ratatui::style::{Color, Modifier, Style};
use ratatui_textarea::{Input, TextArea};

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
        if self.history.last().map_or(true, |last| last != &text) {
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
