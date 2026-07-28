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

use crate::classifier::WorkMode;
use crate::thinking::ThinkingLevel;
use crate::ui::components::status_bar::status_bar::StatusBar;

/// Bottom bar: status indicator on the left, work mode, model and thinking
/// level in the middle, copyright on the right.
#[derive(Debug)]
pub struct Footer {
    pub status: StatusBar,
    pub current_model: String,
    pub(crate) thinking_level: ThinkingLevel,
    pub work_mode: WorkMode,
    /// Whether the project has an LSP checker configured, so the LSP block shows
    /// even before a server has started.
    pub lsp_configured: bool,
    /// Comma-separated names of active skills for display.
    pub active_skills: String,
}

impl Footer {
    pub fn new() -> Self {
        Self {
            status: StatusBar::new(),
            current_model: String::new(),
            thinking_level: ThinkingLevel::default(),
            work_mode: WorkMode::default(),
            lsp_configured: false,
            active_skills: String::new(),
        }
    }

    pub(crate) fn model_and_thinking_text(&self) -> String {
        if self.current_model.is_empty() {
            String::new()
        } else if self.thinking_level == ThinkingLevel::None {
            format!(" {} ", self.current_model)
        } else {
            format!(" {} · {} ", self.current_model, self.thinking_level.label())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_and_thinking_text_omits_none() {
        let mut footer = Footer::new();
        footer.current_model = "openai/gpt-5".to_string();
        footer.thinking_level = ThinkingLevel::None;

        assert_eq!(footer.model_and_thinking_text(), " openai/gpt-5 ");
    }

    #[test]
    fn model_and_thinking_text_includes_active_level() {
        let mut footer = Footer::new();
        footer.current_model = "openai/gpt-5".to_string();
        footer.thinking_level = ThinkingLevel::High;

        assert_eq!(footer.model_and_thinking_text(), " openai/gpt-5 · high ");
    }
}
