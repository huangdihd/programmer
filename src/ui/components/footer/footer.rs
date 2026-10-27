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
use crate::security::SandboxMode;
use crate::thinking::ThinkingLevel;
use crate::ui::components::status_bar::status_bar::StatusBar;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use unicode_width::UnicodeWidthStr;

/// Mouse-actionable footer segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FooterTarget {
    WorkMode,
    Security,
    Diagnostics,
    Model,
    Thinking,
}

/// Bottom bar: work/sandbox modes and live status on the left, diagnostics and
/// the active model anchored on the right.
#[derive(Debug)]
pub struct Footer {
    pub status: StatusBar,
    pub current_model: String,
    pub(crate) thinking_level: ThinkingLevel,
    pub work_mode: WorkMode,
    pub(crate) sandbox_mode: SandboxMode,
    pub(crate) sandbox_profile: String,
    /// Whether the project has an LSP checker configured, so the LSP block shows
    /// even before a server has started.
    pub lsp_configured: bool,
}

impl Footer {
    pub fn new() -> Self {
        Self {
            status: StatusBar::new(),
            current_model: String::new(),
            thinking_level: ThinkingLevel::default(),
            work_mode: WorkMode::default(),
            sandbox_mode: SandboxMode::default(),
            sandbox_profile: String::new(),
            lsp_configured: false,
        }
    }

    pub(crate) fn model_text(&self) -> String {
        if self.current_model.is_empty() {
            String::new()
        } else {
            format!(" {}", self.current_model)
        }
    }

    pub(crate) fn thinking_text(&self) -> String {
        if self.current_model.is_empty() {
            String::new()
        } else {
            format!(" · {} ", self.thinking_level.label())
        }
    }

    pub(crate) fn model_and_thinking_text(&self) -> String {
        format!("{}{}", self.model_text(), self.thinking_text())
    }

    pub(crate) fn model_and_thinking_widths(&self, area_width: u16) -> (u16, u16) {
        let right_len = (self.model_and_thinking_text().width() as u16).min(area_width / 2);
        let max_thinking_len = if self.current_model.is_empty() {
            right_len
        } else {
            right_len.saturating_sub(1)
        };
        let thinking_len = (self.thinking_text().width() as u16).min(max_thinking_len);
        (right_len.saturating_sub(thinking_len), thinking_len)
    }

    pub(crate) fn sandbox_text(&self) -> String {
        if self.sandbox_profile.is_empty() {
            format!(" \u{1f512} {} ", self.sandbox_mode.label())
        } else {
            format!(
                " \u{1f512} {}/{} ",
                self.sandbox_profile,
                self.sandbox_mode.label()
            )
        }
    }

    pub(crate) fn regions(&self, area: Rect) -> [(FooterTarget, Rect); 5] {
        if self.status.status
            == crate::ui::components::status_bar::status_bar::StatusState::Cancelling
        {
            let hidden = Rect::new(area.x, area.y, 0, area.height);
            return [
                (FooterTarget::WorkMode, hidden),
                (FooterTarget::Security, hidden),
                (FooterTarget::Diagnostics, hidden),
                (FooterTarget::Model, hidden),
                (FooterTarget::Thinking, hidden),
            ];
        }
        let mode_len =
            format!("  {} {} ", self.work_mode.icon(), self.work_mode.label()).width() as u16;
        let sandbox_len = self.sandbox_text().width() as u16;
        let (model_len, thinking_len) = self.model_and_thinking_widths(area.width);
        let lsp = crate::diagnostics::lsp_status();
        let lsp_len = if lsp.checking || lsp.failed || lsp.servers > 0 || self.lsp_configured {
            if lsp.servers > 0 && !lsp.checking && !lsp.failed {
                format!(" \u{25c8} LSP {} ", lsp.servers).width() as u16
            } else if lsp.checking || lsp.failed {
                " ◈ LSP ⟳ ".width() as u16
            } else {
                " ◈ LSP ".width() as u16
            }
        } else {
            0
        };
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(mode_len),
                Constraint::Length(sandbox_len),
                Constraint::Min(1),
                Constraint::Length(lsp_len),
                Constraint::Length(model_len),
                Constraint::Length(thinking_len),
            ])
            .split(area);
        [
            (FooterTarget::WorkMode, chunks[0]),
            (FooterTarget::Security, chunks[1]),
            (FooterTarget::Diagnostics, chunks[3]),
            (FooterTarget::Model, chunks[4]),
            (FooterTarget::Thinking, chunks[5]),
        ]
    }

    pub(crate) fn target_at(&self, area: Rect, column: u16, row: u16) -> Option<FooterTarget> {
        self.regions(area)
            .into_iter()
            .find(|(_, region)| {
                region.width > 0
                    && column >= region.x
                    && column < region.right()
                    && row >= region.y
                    && row < region.bottom()
            })
            .map(|(target, _)| target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_and_thinking_text_keeps_none_clickable() {
        let mut footer = Footer::new();
        footer.current_model = "openai/gpt-5".to_string();
        footer.thinking_level = ThinkingLevel::None;

        assert_eq!(footer.model_and_thinking_text(), " openai/gpt-5 · none ");
    }

    #[test]
    fn model_and_thinking_text_includes_active_level() {
        let mut footer = Footer::new();
        footer.current_model = "openai/gpt-5".to_string();
        footer.thinking_level = ThinkingLevel::High;

        assert_eq!(footer.model_and_thinking_text(), " openai/gpt-5 · high ");
    }

    #[test]
    fn sandbox_text_shows_the_effective_mode() {
        let mut footer = Footer::new();
        footer.sandbox_mode = SandboxMode::Off;
        footer.sandbox_profile = "local".to_string();

        assert_eq!(footer.sandbox_text(), " 🔒 local/off ");
    }

    #[test]
    fn pointer_targets_match_visible_footer_segments() {
        let mut footer = Footer::new();
        footer.current_model = "provider/model".into();
        footer.lsp_configured = true;
        let area = Rect::new(4, 7, 100, 1);

        for (target, region) in footer.regions(area) {
            assert!(region.width > 0);
            assert_eq!(footer.target_at(area, region.x, region.y), Some(target));
            assert_eq!(
                footer.target_at(area, region.right() - 1, region.y),
                Some(target)
            );
        }
        let regions = footer.regions(area);
        let model = regions
            .iter()
            .find(|(target, _)| *target == FooterTarget::Model)
            .unwrap()
            .1;
        let thinking = regions
            .iter()
            .find(|(target, _)| *target == FooterTarget::Thinking)
            .unwrap()
            .1;
        assert_eq!(model.right(), thinking.x);
        assert_eq!(
            footer.target_at(area, model.right() - 1, area.y),
            Some(FooterTarget::Model)
        );
        assert_eq!(
            footer.target_at(area, thinking.x, area.y),
            Some(FooterTarget::Thinking)
        );
        assert_eq!(footer.target_at(area, area.x, area.y + 1), None);
        assert_eq!(
            footer.regions(area)[0].1.width,
            format!(
                "  {} {} ",
                footer.work_mode.icon(),
                footer.work_mode.label()
            )
            .width() as u16
        );
    }

    #[test]
    fn cancelling_footer_has_no_hidden_pointer_targets() {
        let mut footer = Footer::new();
        footer.current_model = "provider/model".into();
        footer.status.status =
            crate::ui::components::status_bar::status_bar::StatusState::Cancelling;
        let area = Rect::new(4, 7, 100, 1);

        assert!(
            footer
                .regions(area)
                .iter()
                .all(|(_, region)| region.width == 0)
        );
        assert_eq!(footer.target_at(area, area.x, area.y), None);
        assert_eq!(footer.target_at(area, area.right() - 1, area.y), None);
    }
}
