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

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::markdown_theme::palette;

/// Renders a collapsible token-usage summary after each response.
pub struct UsageMessage {
    input_tokens: u32,
    output_tokens: u32,
    cached_input_tokens: u32,
    recalled_memories: Option<usize>,
    expanded: bool,
    memories: Vec<String>,
}

impl UsageMessage {
    pub fn new(
        input_tokens: u32,
        output_tokens: u32,
        cached_input_tokens: u32,
        recalled_memories: Option<usize>,
    ) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            recalled_memories,
            expanded: false,
            memories: Vec::new(),
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Attaches recalled-memory snapshots without changing the recorded count.
    pub fn memories(mut self, memories: Vec<String>) -> Self {
        self.memories = memories;
        self
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let total = u64::from(self.input_tokens) + u64::from(self.output_tokens);
        let cached_percent = (u64::from(self.cached_input_tokens) * 100)
            .checked_div(u64::from(self.input_tokens))
            .unwrap_or(0);
        let memory_count = self
            .recalled_memories
            .map_or_else(|| "n/a".to_string(), |n| n.to_string());
        let memory_label = if self.recalled_memories == Some(1) {
            "memory"
        } else {
            "memories"
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(
                if self.expanded { "▾ " } else { "▸ " },
                Style::new().fg(palette::FAINT),
            ),
            Span::styled(total.to_string(), Style::new().fg(palette::CYAN)),
            Span::styled(" tokens(", Style::new().fg(palette::MUTED)),
            Span::styled(
                format!("{cached_percent}%"),
                Style::new().fg(palette::GREEN),
            ),
            Span::styled(" cached)", Style::new().fg(palette::MUTED)),
            Span::styled(" · ", Style::new().fg(palette::FAINT)),
            Span::styled(memory_count.clone(), Style::new().fg(palette::YELLOW)),
            Span::styled(format!(" {memory_label}"), Style::new().fg(palette::MUTED)),
        ])];
        if self.expanded {
            for (label, value, color) in [
                ("Input", u64::from(self.input_tokens), palette::BLUE),
                (
                    "Cached",
                    u64::from(self.cached_input_tokens),
                    palette::GREEN,
                ),
                ("Output", u64::from(self.output_tokens), palette::PURPLE),
                ("Total", total, palette::CYAN),
            ] {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {label:<8}"), Style::new().fg(palette::MUTED)),
                    Span::styled(value.to_string(), Style::new().fg(color)),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled("  Memories recalled: ", Style::new().fg(palette::MUTED)),
                Span::styled(memory_count, Style::new().fg(palette::YELLOW)),
            ]));
            if self.recalled_memories.is_some_and(|count| count > 0) && self.memories.is_empty() {
                lines.push(Line::from(Span::styled(
                    "    Memory details unavailable",
                    Style::new().fg(palette::MUTED),
                )));
            }
            for memory in self.memories {
                for line in memory.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("    {line}"),
                        Style::new().fg(palette::MUTED),
                    )));
                }
            }
        }
        Paragraph::new(lines).wrap(Wrap { trim: false })
    }
}

#[cfg(test)]
mod tests {
    use super::UsageMessage;
    use crate::ui::markdown_theme::palette;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    fn render(message: UsageMessage) -> (Buffer, Vec<String>) {
        let area = Rect::new(0, 0, 120, 12);
        let mut buffer = Buffer::empty(area);
        message.into_paragraph().render(area, &mut buffer);
        let lines = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        (buffer, lines)
    }

    #[test]
    fn usage_is_a_compact_borderless_summary() {
        let (buffer, lines) = render(UsageMessage::new(180000, 9135, 60000, Some(1)));
        assert_eq!(lines[0], "▸ 189135 tokens(33% cached) · 1 memory");
        assert!(lines[1..].iter().all(String::is_empty));
        for (text, color) in [
            ("189135", palette::CYAN),
            ("33%", palette::GREEN),
            ("1 memory", palette::YELLOW),
        ] {
            let x = lines[0][..lines[0].find(text).unwrap()].chars().count() as u16;
            assert_eq!(buffer.cell((x, 0)).unwrap().fg, color);
        }
        assert!(!lines[0].contains('│'));
    }

    #[test]
    fn zero_input_and_unknown_memory_counts_are_safe() {
        for (count, label) in [
            (Some(0), "0 memories"),
            (Some(2), "2 memories"),
            (None, "n/a memories"),
        ] {
            let (_, lines) = render(UsageMessage::new(0, 0, 5, count));
            assert_eq!(lines[0], format!("▸ 0 tokens(0% cached) · {label}"));
        }
        let (_, lines) = render(UsageMessage::new(0, 0, 0, None).expanded(true));
        assert_eq!(lines[5], "  Memories recalled: n/a");
        assert!(lines[6].is_empty());
    }

    #[test]
    fn totals_and_cached_percent_do_not_overflow() {
        let (_, lines) =
            render(UsageMessage::new(u32::MAX, u32::MAX, u32::MAX, Some(0)).expanded(true));
        assert_eq!(lines[0], "▾ 8589934590 tokens(100% cached) · 0 memories");
        assert_eq!(lines[4], "  Total   8589934590");
        assert!(lines[6].is_empty());
    }

    #[test]
    fn expanded_rows_and_snapshots_have_semantic_colors() {
        let (buffer, lines) = render(
            UsageMessage::new(13, 7, 5, Some(2))
                .expanded(true)
                .memories(vec![
                    "First snapshot".into(),
                    "Second snapshot\ncontinued".into(),
                ]),
        );
        assert_eq!(
            &lines[..9],
            &[
                "▾ 20 tokens(38% cached) · 2 memories",
                "  Input   13",
                "  Cached  5",
                "  Output  7",
                "  Total   20",
                "  Memories recalled: 2",
                "    First snapshot",
                "    Second snapshot",
                "    continued",
            ]
        );
        for (y, color) in [
            (1, palette::BLUE),
            (2, palette::GREEN),
            (3, palette::PURPLE),
            (4, palette::CYAN),
        ] {
            assert_eq!(buffer.cell((10, y)).unwrap().fg, color);
        }
    }

    #[test]
    fn missing_details_are_explicit_and_snapshots_stay_collapsed() {
        let (_, lines) = render(UsageMessage::new(1, 2, 0, Some(1)).expanded(true));
        assert_eq!(lines[6], "    Memory details unavailable");
        let (_, lines) = render(
            UsageMessage::new(1, 2, 0, Some(1))
                .memories(vec!["hidden snapshot".into()])
                .expanded(true)
                .expanded(false),
        );
        assert_eq!(lines[0], "▸ 3 tokens(0% cached) · 1 memory");
        assert!(lines[1..].iter().all(String::is_empty));
    }
}
