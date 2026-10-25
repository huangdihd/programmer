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

use super::status_bar::{StatusBar, StatusState};
use crate::ui::{markdown_theme::palette, theme::role};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::time::Duration;

const DIM: Color = palette::MUTED;
const ACCENT: Color = palette::BLUE;
const OUTPUT: Color = palette::GREEN;
const WARN: Color = palette::YELLOW;
const BREATH_STEP: Duration = Duration::from_millis(400);
const THINKING_DEEP_AFTER: Duration = Duration::from_secs(10);
const THINKING_DEEPEST_AFTER: Duration = Duration::from_secs(30);

fn breath_modifier(elapsed: Duration) -> Modifier {
    match (elapsed.as_millis() / BREATH_STEP.as_millis()) % 4 {
        0 => Modifier::DIM,
        2 => Modifier::BOLD,
        _ => Modifier::empty(),
    }
}

fn active_color(status: StatusState, elapsed: Duration) -> Option<Color> {
    match status {
        StatusState::Connecting => Some(role::STATUS_CONNECTING),
        StatusState::Thinking if elapsed >= THINKING_DEEPEST_AFTER => {
            Some(role::STATUS_THINKING_DEEPEST)
        }
        StatusState::Thinking if elapsed >= THINKING_DEEP_AFTER => Some(role::STATUS_THINKING_DEEP),
        StatusState::Thinking => Some(role::STATUS_THINKING),
        StatusState::ToolRunning => Some(role::STATUS_TOOL_RUNNING),
        _ => None,
    }
}

impl Widget for &StatusBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (icon, label, color) = match self.status {
            StatusState::Idle => ("●", "Ready", DIM),
            StatusState::Connecting => ("◌", "Connecting", ACCENT),
            StatusState::Retrying => ("↻", "Retrying", WARN),
            StatusState::Thinking => ("●", "Thinking", ACCENT),
            StatusState::Outputting => ("▸", "Outputting", OUTPUT),
            StatusState::CreatingToolCall => ("⚒", "Creating tool call", WARN),
            StatusState::ToolRunning => ("⚡", "Running tools", WARN),
            StatusState::Classifying => ("◍", "Evaluating", ACCENT),
            StatusState::Associating => ("∿", "Associating", ACCENT),
            StatusState::Checking => ("◇", "Checking diagnostics", ACCENT),
            StatusState::Compacting => ("⧉", "Compacting context", ACCENT),
            StatusState::Cancelling => ("✖", "Cancelling", WARN),
            StatusState::WaitingAnswer => ("?", "Waiting for answer", ACCENT),
            StatusState::WaitingApproval => ("🛡", "Waiting for approval", WARN),
            StatusState::WaitingSubagents => ("⏳", "Waiting for subagents", ACCENT),
        };

        let busy = self.status.is_busy();
        let elapsed = self.elapsed();
        let mut text = format!(" {} {} ", icon, label);
        if busy && let Some(dur) = elapsed {
            let secs = dur.as_secs_f64();
            if secs < 60.0 {
                text.push_str(&format!("({:.1}s)", secs));
            } else {
                let m = (secs / 60.0) as u64;
                let s = secs % 60.0;
                text.push_str(&format!("({}m {:.0}s)", m, s));
            }
        }

        let style = if let Some(elapsed) = elapsed
            && let Some(color) = active_color(self.status, elapsed)
        {
            Style::default()
                .fg(color)
                .add_modifier(breath_modifier(elapsed))
        } else {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        };
        let mut spans = vec![Span::styled(text, style)];
        if let Some(detail) = &self.detail {
            spans.push(Span::styled(
                format!("· {detail} "),
                Style::default().fg(DIM),
            ));
        }
        ratatui::widgets::Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_status_breath_is_deterministic_and_repeats() {
        let phases = [
            (0, Modifier::DIM),
            (399, Modifier::DIM),
            (400, Modifier::empty()),
            (800, Modifier::BOLD),
            (1_200, Modifier::empty()),
            (1_600, Modifier::DIM),
        ];
        for (millis, expected) in phases {
            assert_eq!(breath_modifier(Duration::from_millis(millis)), expected);
        }
    }

    #[test]
    fn only_requested_active_statuses_use_animation_roles() {
        let elapsed = Duration::from_secs(1);
        assert_eq!(
            active_color(StatusState::Connecting, elapsed),
            Some(role::STATUS_CONNECTING)
        );
        assert_eq!(
            active_color(StatusState::Thinking, elapsed),
            Some(role::STATUS_THINKING)
        );
        assert_eq!(
            active_color(StatusState::ToolRunning, elapsed),
            Some(role::STATUS_TOOL_RUNNING)
        );
        assert_eq!(active_color(StatusState::Outputting, elapsed), None);
    }

    #[test]
    fn thinking_color_deepens_at_duration_boundaries() {
        assert_eq!(
            active_color(
                StatusState::Thinking,
                THINKING_DEEP_AFTER - Duration::from_millis(1)
            ),
            Some(role::STATUS_THINKING)
        );
        assert_eq!(
            active_color(StatusState::Thinking, THINKING_DEEP_AFTER),
            Some(role::STATUS_THINKING_DEEP)
        );
        assert_eq!(
            active_color(
                StatusState::Thinking,
                THINKING_DEEPEST_AFTER - Duration::from_millis(1)
            ),
            Some(role::STATUS_THINKING_DEEP)
        );
        assert_eq!(
            active_color(StatusState::Thinking, THINKING_DEEPEST_AFTER),
            Some(role::STATUS_THINKING_DEEPEST)
        );
    }
}
