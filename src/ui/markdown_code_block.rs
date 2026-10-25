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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui_markdown::highlight::{CodeHighlighter, TreeSitterHighlighter, segments_to_lines};
use ratatui_markdown::markdown::RenderHooks;
use ratatui_markdown::mermaid::render_mermaid;

use crate::ui::markdown_theme::{AppTheme, CODE_COLORS, palette};

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
    HL.get_or_init(|| Arc::new(TreeSitterHighlighter::new().with_code_colors(CODE_COLORS)))
        .clone()
}

/// Renders fenced code blocks as a borderless, shaded, syntax-highlighted panel
/// instead of the library default (a left `│` gutter with corner brackets).
pub struct CodeBlockHooks {
    width: usize,
    show_copy_label: bool,
    /// Raw content of every rendered code block, in render order. Shared with
    /// the caller so copy buttons can be wired up after rendering.
    codes: Arc<Mutex<Vec<String>>>,
}

impl CodeBlockHooks {
    pub fn new(width: usize) -> Self {
        Self {
            width,
            show_copy_label: true,
            codes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Read-only surfaces cannot wire up a copy action, but retain the same panel.
    pub(crate) fn without_copy_label(mut self) -> Self {
        self.show_copy_label = false;
        self
    }

    /// Handle to the collected code block contents.
    pub fn codes(&self) -> Arc<Mutex<Vec<String>>> {
        self.codes.clone()
    }

    /// The block's top padding row: a dim language label on the left and the
    /// optional clickable copy label on the right.
    fn label_line(&self, lang: &str) -> Line<'static> {
        let base = Style::default().fg(palette::TEXT).bg(CODE_BG);
        let label_style = Style::default().fg(LABEL_FG).bg(CODE_BG);

        let mut spans = vec![Span::styled(INDENT, base)];
        if !lang.is_empty() {
            spans.push(Span::styled(lang.to_string(), label_style));
        }
        let used: usize = spans.iter().map(|s| s.width()).sum();

        let button = Span::styled(COPY_LABEL, label_style);
        let button_width = button.width();
        if self.show_copy_label && self.width > used + button_width + RIGHT_PAD {
            spans.push(Span::styled(
                " ".repeat(self.width - used - button_width - RIGHT_PAD),
                base,
            ));
            spans.push(button);
            spans.push(Span::styled(" ".repeat(RIGHT_PAD), base));
        } else if used < self.width {
            // Copy is hidden or the row is too narrow; keep the panel filled.
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

    fn diagram(&self, content: &str) -> Option<Vec<Line<'static>>> {
        let diagram = if is_mindmap(content) {
            render_mindmap(content)
        } else if is_horizontal_flowchart(content) {
            render_horizontal_flowchart(content)?
        } else {
            render_mermaid(content, self.width.saturating_sub(2), None, &AppTheme)?
        };
        let mut lines = Vec::with_capacity(diagram.len() + 2);
        lines.push(self.label_line("mermaid"));
        lines.extend(diagram.into_iter().map(|line| self.shade(line)));
        lines.push(self.blank());
        Some(lines)
    }

    fn formula(&self, content: &str) -> Vec<Line<'static>> {
        let mut lines = vec![self.label_line("math")];
        lines.extend(content.lines().map(|line| {
            self.shade(Line::from(Span::styled(
                format!("{INDENT}{}", latex_to_unicode(line)),
                Style::default().fg(palette::CYAN).bg(CODE_BG),
            )))
        }));
        lines.push(self.blank());
        lines
    }
}

fn is_mindmap(source: &str) -> bool {
    source.lines().any(|line| line.trim() == "mindmap")
}

/// The upstream LR/RL terminal layout confuses node width with vertical
/// spacing. Cycles and CJK labels can therefore produce a screen-tall drawing
/// whose connectors cross its text. Use an explicit edge-list fallback for
/// horizontal flowcharts: every relationship remains visible, branches retain
/// tree connectors, and feedback edges are marked with a return arrow.
fn render_horizontal_flowchart(source: &str) -> Option<Vec<Line<'static>>> {
    let mut labels = HashMap::<String, String>::new();
    let mut order = Vec::<String>::new();
    let mut edges = Vec::<(String, String)>::new();

    let mut remember = |raw: &str| {
        let (id, label) = flowchart_node(raw)?;
        if !labels.contains_key(&id) {
            order.push(id.clone());
            labels.insert(id.clone(), label);
        } else if label != id {
            // A later bare reference (for example `Check --> Model`) must not
            // replace the label declared earlier as `Check[Security check]`.
            labels.insert(id.clone(), label);
        }
        Some(id)
    };

    let mut body = source
        .lines()
        .skip_while(|line| !is_horizontal_flowchart_header(line.trim()))
        .skip(1);
    for source_line in &mut body {
        let line = source_line.trim().trim_end_matches(';').trim();
        if line.is_empty()
            || line.starts_with("%%")
            || line.starts_with("classDef ")
            || line.starts_with("class ")
            || line.starts_with("style ")
        {
            continue;
        }
        let Some((left, right)) = split_flowchart_edge(line) else {
            let _ = remember(line);
            continue;
        };
        let Some(from) = remember(left) else { continue };
        let Some(to) = remember(right) else { continue };
        edges.push((from, to));
    }
    if labels.is_empty() {
        return None;
    }

    let connected = edges
        .iter()
        .flat_map(|(source, target)| [source.as_str(), target.as_str()])
        .collect::<HashSet<_>>();
    let mut rendered_sources = HashSet::<String>::new();
    let mut lines = Vec::new();
    for node in &order {
        let outgoing = edges
            .iter()
            .filter(|(source, _)| source == node)
            .collect::<Vec<_>>();
        if outgoing.is_empty() || !rendered_sources.insert(node.clone()) {
            continue;
        }
        let from = labels.get(node).map(String::as_str).unwrap_or(node);
        if outgoing.len() == 1 {
            let target = outgoing[0].1.as_str();
            let to = labels.get(target).map(String::as_str).unwrap_or(target);
            let connector = if rendered_sources.contains(target) {
                " ↩ "
            } else {
                " ──→ "
            };
            lines.push(flowchart_line("  ", from, connector, to));
        } else {
            lines.push(Line::from(vec![
                Span::styled("  ◆ ", Style::default().fg(palette::BLUE)),
                Span::styled(from.to_string(), Style::default().fg(palette::TEXT)),
            ]));
            for (index, edge) in outgoing.iter().enumerate() {
                let target = edge.1.as_str();
                let to = labels.get(target).map(String::as_str).unwrap_or(target);
                let branch = if index + 1 == outgoing.len() {
                    "  └─→ "
                } else {
                    "  ├─→ "
                };
                let suffix = if rendered_sources.contains(target) {
                    "  ↩"
                } else {
                    ""
                };
                lines.push(Line::from(vec![
                    Span::styled(branch, Style::default().fg(palette::BLUE)),
                    Span::styled(to.to_string(), Style::default().fg(palette::TEXT)),
                    Span::styled(suffix, Style::default().fg(palette::MUTED)),
                ]));
            }
        }
    }
    for node in order {
        if connected.contains(node.as_str()) {
            continue;
        }
        let label = labels.get(&node).map(String::as_str).unwrap_or(&node);
        lines.push(Line::from(vec![
            Span::styled("  • ", Style::default().fg(palette::BLUE)),
            Span::styled(label.to_string(), Style::default().fg(palette::TEXT)),
        ]));
    }
    Some(lines)
}

fn flowchart_line(prefix: &str, from: &str, connector: &str, to: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(prefix.to_string(), Style::default().fg(palette::BLUE)),
        Span::styled(from.to_string(), Style::default().fg(palette::TEXT)),
        Span::styled(connector.to_string(), Style::default().fg(palette::BLUE)),
        Span::styled(to.to_string(), Style::default().fg(palette::TEXT)),
    ])
}

fn split_flowchart_edge(line: &str) -> Option<(&str, &str)> {
    for arrow in ["-->", "==>", "-.->", "---"] {
        if let Some((left, right)) = line.split_once(arrow) {
            let right = right
                .trim()
                .strip_prefix('|')
                .and_then(|rest| rest.split_once('|').map(|(_, target)| target))
                .unwrap_or(right);
            return Some((left.trim(), right.trim()));
        }
    }
    None
}

fn flowchart_node(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let boundary = raw
        .char_indices()
        .find(|(_, ch)| matches!(ch, '[' | '(' | '{'))
        .map_or(raw.len(), |(index, _)| index);
    let id = raw[..boundary].trim().trim_matches('"');
    if id.is_empty() {
        return None;
    }
    let label = if boundary == raw.len() {
        id.to_string()
    } else {
        mermaid_label(&raw[boundary..])
    };
    Some((id.to_string(), label))
}

fn is_horizontal_flowchart_header(line: &str) -> bool {
    let mut words = line.split_whitespace();
    matches!(words.next(), Some("graph" | "flowchart")) && matches!(words.next(), Some("LR" | "RL"))
}

fn is_horizontal_flowchart(source: &str) -> bool {
    source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("%%"))
        .is_some_and(is_horizontal_flowchart_header)
}

fn render_mindmap(source: &str) -> Vec<Line<'static>> {
    let entries = source
        .lines()
        .skip_while(|line| line.trim() != "mindmap")
        .skip(1)
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with("%%"))
        .map(|line| (indent_width(line), mermaid_label(line.trim())))
        .collect::<Vec<_>>();

    let mut levels = Vec::<usize>::new();
    entries
        .into_iter()
        .map(|(indent, label)| {
            while levels.last().is_some_and(|level| *level >= indent) {
                levels.pop();
            }
            let depth = levels.len();
            levels.push(indent);
            let prefix = if depth == 0 {
                "◆ ".to_string()
            } else {
                format!("{}├─ ", "│  ".repeat(depth.saturating_sub(1)))
            };
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(palette::BLUE)),
                Span::styled(label, Style::default().fg(palette::TEXT)),
            ])
        })
        .collect()
}

fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|ch| ch.is_whitespace())
        .fold(0, |width, ch| {
            if ch == '\t' {
                width + (4 - width % 4)
            } else {
                width + 1
            }
        })
}

fn mermaid_label(raw: &str) -> String {
    let raw = raw.split_once(":::").map_or(raw, |(label, _)| label);
    let start = raw.find(&['(', ')', '[', '{'][..]);
    let label = start.map_or(raw, |index| &raw[index..]);
    label
        .trim_matches(|ch: char| matches!(ch, '(' | ')' | '[' | ']' | '{' | '}'))
        .trim()
        .to_string()
}

pub(crate) fn latex_to_unicode(source: &str) -> String {
    let mut rendered = source.to_string();
    for (latex, unicode) in [
        ("\\alpha", "α"),
        ("\\beta", "β"),
        ("\\gamma", "γ"),
        ("\\delta", "δ"),
        ("\\theta", "θ"),
        ("\\lambda", "λ"),
        ("\\mu", "μ"),
        ("\\pi", "π"),
        ("\\rho", "ρ"),
        ("\\sigma", "σ"),
        ("\\phi", "φ"),
        ("\\omega", "ω"),
        ("\\Delta", "Δ"),
        ("\\Sigma", "Σ"),
        ("\\Omega", "Ω"),
        ("\\infty", "∞"),
        ("\\sum", "∑"),
        ("\\prod", "∏"),
        ("\\int", "∫"),
        ("\\sqrt", "√"),
        ("\\times", "×"),
        ("\\cdot", "·"),
        ("\\pm", "±"),
        ("\\neq", "≠"),
        ("\\leq", "≤"),
        ("\\geq", "≥"),
        ("\\approx", "≈"),
        ("\\rightarrow", "→"),
        ("\\leftarrow", "←"),
        ("\\Rightarrow", "⇒"),
        ("\\left", ""),
        ("\\right", ""),
        ("\\frac", ""),
    ] {
        rendered = rendered.replace(latex, unicode);
    }
    rendered = rendered.replace("}{", ")/(");
    rendered = rendered.replace('{', "(").replace('}', ")");
    for (plain, raised) in [
        ("^0", "⁰"),
        ("^1", "¹"),
        ("^2", "²"),
        ("^3", "³"),
        ("^4", "⁴"),
        ("^5", "⁵"),
        ("^6", "⁶"),
        ("^7", "⁷"),
        ("^8", "⁸"),
        ("^9", "⁹"),
        ("_0", "₀"),
        ("_1", "₁"),
        ("_2", "₂"),
        ("_3", "₃"),
        ("_4", "₄"),
        ("_5", "₅"),
        ("_6", "₆"),
        ("_7", "₇"),
        ("_8", "₈"),
        ("_9", "₉"),
    ] {
        rendered = rendered.replace(plain, raised);
    }
    rendered
}

impl RenderHooks for CodeBlockHooks {
    fn render_code_block(&self, lang: &str, content: &str) -> Option<Vec<Line<'static>>> {
        if let Ok(mut codes) = self.codes.lock() {
            codes.push(content.to_string());
        }

        let lang = lang.trim().to_ascii_lowercase();
        if lang == "mermaid" {
            return self.diagram(content);
        }
        if matches!(lang.as_str(), "latex" | "tex" | "math") {
            return Some(self.formula(content));
        }

        let content = content.replace('\t', "    ");
        let base = Style::default().fg(palette::TEXT).bg(CODE_BG);
        let inner = self.width.saturating_sub(INDENT.len() + RIGHT_PAD);

        let segments = highlighter().highlight(&lang, &content);
        let code_lines = segments_to_lines(&content, &segments, INDENT, base, inner);

        let mut lines = Vec::with_capacity(code_lines.len() + 2);

        // Top padding row: dim language label plus the optional copy label.
        lines.push(self.label_line(&lang));
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

    #[test]
    fn hiding_copy_label_preserves_language_shading_and_code_body() {
        for language in ["rust", ""] {
            let conversation = CodeBlockHooks::new(40)
                .render_code_block(language, "let value = 1;")
                .expect("hooks always render a block");
            let read_only = CodeBlockHooks::new(40)
                .without_copy_label()
                .render_code_block(language, "let value = 1;")
                .expect("hooks always render a block");

            assert!(
                conversation[0]
                    .spans
                    .iter()
                    .any(|span| span.content == COPY_LABEL)
            );
            assert!(
                !read_only[0]
                    .spans
                    .iter()
                    .any(|span| span.content == COPY_LABEL)
            );
            assert_eq!(read_only[0].to_string().trim(), language);
            assert_eq!(read_only[0].width(), 40);
            assert!(
                read_only[0]
                    .spans
                    .iter()
                    .all(|span| span.style.bg == Some(CODE_BG))
            );
            if !language.is_empty() {
                assert_eq!(read_only[0].spans[1], conversation[0].spans[1]);
            }
            assert_eq!(read_only[1..], conversation[1..]);
        }
    }

    #[test]
    fn mermaid_flowcharts_render_as_terminal_diagrams() {
        let lines = CodeBlockHooks::new(60)
            .render_code_block("mermaid", "graph LR\nA[Plan] --> B[Build]")
            .expect("supported diagram");
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Plan"), "{rendered}");
        assert!(rendered.contains("Build"), "{rendered}");
        assert!(!rendered.contains("graph LR"), "{rendered}");
    }

    #[test]
    fn mermaid_lr_branches_do_not_use_label_width_as_vertical_spacing() {
        let lines = CodeBlockHooks::new(100)
            .render_code_block(
                "mermaid",
                "graph LR\nRequest[Build request] --> Response{Response type}\nResponse --> Text[Text output]\nResponse --> Tool[Tool call requiring security approval]\nResponse --> Done[Turn complete]",
            )
            .expect("supported diagram");
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("security approval"), "{rendered}");
        assert!(
            lines.len() <= 28,
            "diagram is {} lines tall:\n{rendered}",
            lines.len()
        );
    }

    #[test]
    fn mermaid_lr_disconnected_nodes_do_not_leave_width_sized_blank_gaps() {
        let lines = CodeBlockHooks::new(100)
            .render_code_block(
                "mermaid",
                "graph LR\nRequest[Build request with a deliberately long label]\nDone[Turn complete]",
            )
            .expect("supported diagram");
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Turn complete"), "{rendered}");
        assert!(
            lines.len() <= 15,
            "diagram is {} lines tall:\n{rendered}",
            lines.len()
        );
    }

    #[test]
    fn mermaid_lr_feedback_loop_keeps_connector_routing_compact() {
        let lines = CodeBlockHooks::new(100)
            .render_code_block(
                "mermaid",
                "graph LR\nInput[用户输入] --> Check[安全检查与审批]\nCheck --> Model[调用模型]\nModel --> Kind[响应类型]\nKind --> Tool[执行工具]\nTool --> Input\nKind --> Answer[展示回答]\nAnswer --> Context[结果写回上下文]\nContext --> Save[保存 Session]",
            )
            .expect("supported diagram");
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("保存 Session"), "{rendered}");
        assert!(
            lines.len() <= 35,
            "feedback diagram is {} lines tall:\n{rendered}",
            lines.len()
        );
        let diagram = lines[1..lines.len() - 1]
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            diagram,
            [
                "  用户输入 ──→ 安全检查与审批",
                "  安全检查与审批 ──→ 调用模型",
                "  调用模型 ──→ 响应类型",
                "  ◆ 响应类型",
                "  ├─→ 执行工具",
                "  └─→ 展示回答",
                "  执行工具 ↩ 用户输入",
                "  展示回答 ──→ 结果写回上下文",
                "  结果写回上下文 ──→ 保存 Session",
            ]
        );
    }

    #[test]
    fn mermaid_mindmaps_render_as_an_indented_tree() {
        let lines = CodeBlockHooks::new(50)
            .render_code_block(
                "mermaid",
                "%% terminal fallback\nmindmap\n  root)Release(:::urgent\n\tPlan\n    Ship",
            )
            .expect("mindmap fallback");
        let tree = lines[1..lines.len() - 1]
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>();

        assert_eq!(tree, ["◆ Release", "├─ Plan", "├─ Ship"]);
    }

    #[test]
    fn latex_uses_readable_terminal_symbols() {
        assert_eq!(latex_to_unicode(r"E = mc^2"), "E = mc²");
        assert_eq!(
            latex_to_unicode(r"\frac{\pi}{2} \leq \infty"),
            "(π)/(2) ≤ ∞"
        );
    }

    #[test]
    fn plain_code_uses_the_application_text_color() {
        let hooks = CodeBlockHooks::new(20);
        let lines = hooks
            .render_code_block("", "plain text")
            .expect("hooks always render a block");
        let content = &lines[1];

        assert_eq!(content.spans[0].style.fg, Some(palette::TEXT));
    }
}
