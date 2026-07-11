use std::sync::{Arc, OnceLock};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui_markdown::highlight::{segments_to_lines, CodeHighlighter, TreeSitterHighlighter};
use ratatui_markdown::markdown::RenderHooks;

use crate::ui::markdown_theme::palette;

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
}

impl CodeBlockHooks {
    pub fn new(width: usize) -> Self {
        Self { width }
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
        let content = content.replace('\t', "    ");
        let base = Style::default().bg(CODE_BG);
        let inner = self.width.saturating_sub(INDENT.len() + RIGHT_PAD);

        let segments = highlighter().highlight(lang, &content);
        let code_lines = segments_to_lines(&content, &segments, INDENT, base, inner);

        let mut lines = Vec::with_capacity(code_lines.len() + 2);

        // Top padding row, doubling as a dim language label.
        if lang.is_empty() {
            lines.push(self.blank());
        } else {
            let mut spans = vec![
                Span::styled(INDENT, base),
                Span::styled(lang.to_string(), Style::default().fg(LABEL_FG).bg(CODE_BG)),
            ];
            let used: usize = spans.iter().map(|s| s.width()).sum();
            if used < self.width {
                spans.push(Span::styled(" ".repeat(self.width - used), base));
            }
            lines.push(Line::from(spans));
        }

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
