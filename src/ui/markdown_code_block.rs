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

use std::sync::{Arc, Mutex, OnceLock};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui_markdown::highlight::{CodeHighlighter, TreeSitterHighlighter, segments_to_lines};
use ratatui_markdown::markdown::RenderHooks;

use crate::ui::markdown_theme::palette;

/// The clickable copy label on a code block's top row.
pub const COPY_LABEL: &str = "⧉ copy";

/// A clickable "copy this code block" hotspot, positioned relative to the top
/// left of the rendered message paragraph.
#[derive(Debug, Clone)]
pub struct CodeCopyButton {
    /// Row within the paragraph.
    pub row: u16,
    /// Inclusive start column within the paragraph.
    pub x_start: u16,
    /// Exclusive end column.
    pub x_end: u16,
    /// The code block's raw content.
    pub content: String,
}

/// Subtle shaded panel behind code blocks (a touch lighter than the terminal
/// background), so the block reads as distinct without any border characters.
const CODE_BG: Color = palette::CODE_BG;
/// Muted foreground for the language label on the top padding row.
const LABEL_FG: Color = palette::FAINT;
/// Left padding inside the block.
const INDENT: &str = "  ";
/// Extra columns kept clear on the right so text doesn't touch the edge.
const RIGHT_PAD: usize = 2;

/// The syntax highlighter is expensive to build, so share a single lazily
/// initialized instance across every code block and frame.
fn highlighter() -> Arc<dyn CodeHighlighter> {
    static HL: OnceLock<Arc<dyn CodeHighlighter>> = OnceLock::new();
    HL.get_or_init(|| Arc::new(TreeSitterHighlighter::new()))
        .clone()
}

/// Renders fenced code blocks as a borderless, shaded, syntax-highlighted panel
/// instead of the library default (a left `│` gutter with corner brackets).
pub struct CodeBlockHooks {
    width: usize,
    /// Raw content of every rendered code block, in render order. Shared with
    /// the caller so copy buttons can be wired up after rendering.
    codes: Arc<Mutex<Vec<String>>>,
}

impl CodeBlockHooks {
    pub fn new(width: usize) -> Self {
        Self {
            width,
            codes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Handle to the collected code block contents.
    pub fn codes(&self) -> Arc<Mutex<Vec<String>>> {
        self.codes.clone()
    }

    /// The block's top padding row: a dim language label on the left and the
    /// clickable copy label on the right.
    fn label_line(&self, lang: &str) -> Line<'static> {
        let base = Style::default().bg(CODE_BG);
        let label_style = Style::default().fg(LABEL_FG).bg(CODE_BG);

        let mut spans = vec![Span::styled(INDENT, base)];
        if !lang.is_empty() {
            spans.push(Span::styled(lang.to_string(), label_style));
        }
        let used: usize = spans.iter().map(|s| s.width()).sum();

        let button = Span::styled(COPY_LABEL, label_style);
        let button_width = button.width();
        if self.width > used + button_width + RIGHT_PAD {
            spans.push(Span::styled(
                " ".repeat(self.width - used - button_width - RIGHT_PAD),
                base,
            ));
            spans.push(button);
            spans.push(Span::styled(" ".repeat(RIGHT_PAD), base));
        } else if used < self.width {
            // Too narrow for the button; just pad the row out.
            spans.push(Span::styled(" ".repeat(self.width - used), base));
        }
        Line::from(spans)
    }

    /// A full-width row filled with the panel background.
    fn blank(&self) -> Line<'static> {
        Line::from(Span::styled(
            " ".repeat(self.width),
            Style::default().bg(CODE_BG),
        ))
    }

    /// Applies the panel background to every span of a highlighted line and pads
    /// it out to the full block width so the shading is a clean rectangle.
    fn shade(&self, line: Line<'static>) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|span| Span::styled(span.content, span.style.bg(CODE_BG)))
            .collect();

        let used: usize = spans.iter().map(|s| s.width()).sum();
        if used < self.width {
            spans.push(Span::styled(
                " ".repeat(self.width - used),
                Style::default().bg(CODE_BG),
            ));
        }
        Line::from(spans)
    }
}

impl RenderHooks for CodeBlockHooks {
    fn render_code_block(&self, lang: &str, content: &str) -> Option<Vec<Line<'static>>> {
        if let Ok(mut codes) = self.codes.lock() {
            codes.push(content.to_string());
        }

        let content = content.replace('\t', "    ");
        let base = Style::default().bg(CODE_BG);
        let inner = self.width.saturating_sub(INDENT.len() + RIGHT_PAD);

        let segments = highlighter().highlight(lang, &content);
        let code_lines = segments_to_lines(&content, &segments, INDENT, base, inner);

        let mut lines = Vec::with_capacity(code_lines.len() + 2);

        // Top padding row: dim language label plus the clickable copy label.
        lines.push(self.label_line(lang));
        lines.extend(code_lines.into_iter().map(|line| self.shade(line)));
        lines.push(self.blank());

        Some(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_borderless_shaded_block() {
        let hooks = CodeBlockHooks::new(20);
        let lines = hooks
            .render_code_block("rust", "fn main() {}\nlet x = 1;")
            .expect("hooks always render a block");

        // No border/gutter characters anywhere.
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains(['│', '╭', '╰', '─']),
                "unexpected border char in {text:?}"
            );
        }

        // Every row is filled to the full width with the panel background.
        for line in &lines {
            assert_eq!(line.width(), 20, "row not padded to full width");
            for span in &line.spans {
                assert_eq!(span.style.bg, Some(CODE_BG), "span missing panel bg");
            }
        }

        // A top label row, the two code rows, and a bottom padding row.
        assert_eq!(lines.len(), 4);
    }
}
