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

use async_openai::types::responses::{
    FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall,
};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use unicode_width::UnicodeWidthStr;

use crate::ui::components::messages::assistant::detail_style;
use crate::ui::markdown_theme::palette;

/// Renders a tool call the model made, together with its result once it has
/// one. Collapsed it is the tool name plus a one-line summary, with the first
/// line of the result below; expanded it shows each argument and the
/// full result. Successful calls render in green; failed calls render in red.
pub struct ToolCallMessage<'a> {
    call: &'a FunctionToolCall,
    output: Option<&'a FunctionCallOutputItemParam>,
    failed: bool,
    expanded: bool,
    /// Human-readable label explaining why this tool call was approved or
    /// denied (e.g. "approved by Auto mode", "denied in Manual mode by user").
    approval_label: Option<&'a str>,
    /// Live output streamed by a still-running command (cleaned of terminal
    /// control sequences). Rendered in place of the "waiting" spinner until the
    /// call's committed `output` arrives.
    live_output: Option<&'a str>,
}

impl<'a> ToolCallMessage<'a> {
    pub fn new(call: &'a FunctionToolCall) -> Self {
        Self {
            call,
            output: None,
            failed: false,
            expanded: false,
            approval_label: None,
            live_output: None,
        }
    }

    pub fn output(mut self, output: Option<&'a FunctionCallOutputItemParam>) -> Self {
        self.output = output;
        self
    }

    pub fn live_output(mut self, live_output: Option<&'a str>) -> Self {
        self.live_output = live_output;
        self
    }

    pub fn failed(mut self, failed: bool) -> Self {
        self.failed = failed;
        self
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn approval_label(mut self, label: Option<&'a str>) -> Self {
        self.approval_label = label;
        self
    }

    pub fn into_text(self) -> Text<'static> {
        let result_text = self.output.map(|o| match &o.output {
            FunctionCallOutput::Text(t) => t.clone(),
            FunctionCallOutput::Content(_) => "[non-text output]".to_string(),
        });
        let failed = self.failed;

        let status_color = if failed { palette::RED } else { palette::GREEN };
        let status_char = if failed { "\u{2717}" } else { "\u{2713}" };
        let accent = Style::new()
            .fg(status_color)
            .add_modifier(Modifier::BOLD);
        let muted = Style::new().fg(palette::MUTED);
        let value = serde_json::from_str::<serde_json::Value>(&self.call.arguments).ok();

        if !self.expanded {
            let mut spans = vec![
                Span::styled("\u{25B8} ", muted),
                Span::styled(format!("\u{1F527} {}", self.call.name), accent),
            ];
            let (summary, multiline) = one_line_summary(value.as_ref(), &self.call.arguments);
            if !summary.is_empty() {
                let suffix = if multiline { "..." } else { "" };
                spans.push(Span::styled(format!("  {summary}{suffix}"), muted));
            }
            let mut lines = vec![Line::from(spans)];
            // Approval label (collapsed): subtle line below the tool name.
            if let Some(label) = self.approval_label {
                let label_style = Style::new().fg(palette::MUTED).add_modifier(Modifier::DIM);
                lines.push(Line::from(Span::styled(
                    format!("  {label}"),
                    label_style,
                )));
            }
            if let Some(text) = &result_text {
                let dim = if failed {
                    Style::new().fg(palette::RED_MUTED)
                } else {
                    Style::new().fg(palette::MUTED).add_modifier(Modifier::DIM)
                };
                let mut result_lines = text.lines();
                let first = result_lines.next().unwrap_or("[no output]");
                let suffix = if result_lines.next().is_some() { "..." } else { "" };
                lines.push(Line::from(Span::styled(
                    format!("  \u{23BF} {first}{suffix}"),
                    dim,
                )));
            } else if let Some(live) = self.live_output {
                push_live_tail(&mut lines, live, muted);
            } else if !failed {
                lines.push(Line::from(Span::styled("  \u{23BF} \u{2026}", muted)));
            }
            return Text::from(lines);
        }

        let mut lines = vec![Line::from(vec![
            Span::styled("\u{25BE} ", muted),
            Span::styled(format!("\u{1F527} {}", self.call.name), accent),
        ])];

        match &value {
            // `edit_file` renders its old_string → new_string change as a
            // colored unified diff instead of two opaque text fields.
            Some(serde_json::Value::Object(map))
                if self.call.name == "edit_file"
                    && map.get("old_string").and_then(|v| v.as_str()).is_some()
                    && map.get("new_string").and_then(|v| v.as_str()).is_some() =>
            {
                if let Some(path) = map.get("path").and_then(|v| v.as_str()) {
                    push_field(&mut lines, "path", path, detail_style());
                }
                let old = map.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                let new = map.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                // `offset` (1-based) hints where the search began; use it as the
                // base line number when present, otherwise number from 1.
                let base = map
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.max(1) as usize)
                    .unwrap_or(1);
                for line in diff_lines(old, new, base) {
                    lines.push(line);
                }
            }
            Some(serde_json::Value::Object(map)) => {
                for (key, val) in map {
                    push_field(&mut lines, key, &value_text(val), detail_style());
                }
            }
            _ => {
                for line in self.call.arguments.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        detail_style(),
                    )));
                }
            }
        }

        // Approval label (expanded): subtle line below the arguments.
        if let Some(label) = self.approval_label {
            let label_style = Style::new().fg(palette::MUTED).add_modifier(Modifier::DIM);
            lines.push(Line::from(Span::styled(
                format!("  {label}"),
                label_style,
            )));
        }

        if let Some(text) = &result_text {
            let result_style = if failed {
                Style::new().fg(palette::RED_MUTED)
            } else {
                detail_style()
            };
            let mut first = true;
            for line in text.lines() {
                let prefix = format!("  {} {status_char} ", if first { "\u{23BF}" } else { " " });
                lines.push(Line::from(Span::styled(
                    format!("{prefix}{line}"),
                    result_style,
                )));
                first = false;
            }
            if first {
                lines.push(Line::from(Span::styled("  \u{23BF} [no output]", detail_style())));
            }
        } else if let Some(live) = self.live_output {
            push_live_tail(&mut lines, live, muted);
        }

        Text::from(lines)
    }
}

/// Lines of live command output shown while it runs. Only the tail matters for
/// "is it making progress"; the full output lands in the committed result.
const LIVE_TAIL_LINES: usize = 8;

/// Append the tail of a running command's live output to `lines`, each row
/// dimmed and marked with the result gutter. A leading `⋯` shows when earlier
/// lines were dropped.
fn push_live_tail(lines: &mut Vec<Line<'static>>, live: &str, muted: Style) {
    let dim = muted.add_modifier(Modifier::DIM);
    let all: Vec<&str> = live.lines().collect();
    // A trailing newline yields no final empty entry from `.lines()`, so this is
    // simply every non-terminated line the command has produced so far.
    if all.is_empty() {
        lines.push(Line::from(Span::styled("  \u{23BF} \u{2026}", muted)));
        return;
    }
    let start = all.len().saturating_sub(LIVE_TAIL_LINES);
    if start > 0 {
        lines.push(Line::from(Span::styled("  \u{23BF} \u{22EF}", dim)));
    }
    for line in &all[start..] {
        lines.push(Line::from(Span::styled(format!("  \u{23BF} {line}"), dim)));
    }
}

fn one_line_summary(value: Option<&serde_json::Value>, raw: &str) -> (String, bool) {
    let truncated = |s: &str| {
        let mut lines = s.lines();
        let first = lines.next().unwrap_or("").to_string();
        let has_more = lines.next().is_some();
        (first, has_more)
    };
    if let Some(serde_json::Value::Object(map)) = value {
        for key in ["command", "path"] {
            if let Some(text) = map.get(key).and_then(|v| v.as_str()) {
                return truncated(text);
            }
        }
        if let Some(text) = map.values().find_map(|v| v.as_str()) {
            return truncated(text);
        }
    }
    truncated(raw)
}

/// One row of a line diff.
enum DiffOp {
    /// Unchanged line, referenced by its old-side index.
    Equal(usize),
    /// Line removed from the old side (old index).
    Delete(usize),
    /// Line added on the new side (new index).
    Insert(usize),
}

/// Number of unchanged context lines kept on each side of a change; longer
/// unchanged runs are collapsed into a single `⋯` separator.
const DIFF_CONTEXT: usize = 3;

/// Compute a line-level diff via longest-common-subsequence backtracking.
/// Equal lines are matched so an unchanged line inside the replaced region is
/// not reported as a delete+insert pair.
fn diff_ops(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    let (n, m) = (old.len(), new.len());
    // dp[i][j] = LCS length of old[i..] and new[j..].
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(DiffOp::Equal(i));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffOp::Delete(i));
            i += 1;
        } else {
            ops.push(DiffOp::Insert(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp::Delete(i));
        i += 1;
    }
    while j < m {
        ops.push(DiffOp::Insert(j));
        j += 1;
    }
    ops
}

/// Render a line-based unified diff between `old` and `new`, starting line
/// numbering at `base`. Each row carries a two-column gutter (old line / new
/// line). Unchanged lines near a change are dimmed context (far ones are
/// collapsed to `⋯`); removed lines are red on a dark-red block and added
/// lines are green on a dark-green block.
fn diff_lines(old: &str, new: &str, base: usize) -> Vec<Line<'static>> {
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    let ops = diff_ops(&old_lines, &new_lines);

    // Keep an Equal op only when it sits within DIFF_CONTEXT ops of a change;
    // stretches of unchanged lines further away are collapsed.
    let is_change = |op: &DiffOp| !matches!(op, DiffOp::Equal(..));
    let keep: Vec<bool> = ops
        .iter()
        .enumerate()
        .map(|(idx, op)| {
            if is_change(op) {
                return true;
            }
            let lo = idx.saturating_sub(DIFF_CONTEXT);
            ops[lo..=(idx + DIFF_CONTEXT).min(ops.len() - 1)]
                .iter()
                .any(is_change)
        })
        .collect();

    use ratatui::style::Color;
    let context = Style::new().fg(palette::MUTED).add_modifier(Modifier::DIM);
    let removed = Style::new()
        .fg(palette::RED)
        .bg(Color::Rgb(0x3a, 0x22, 0x28));
    let added = Style::new()
        .fg(palette::GREEN)
        .bg(Color::Rgb(0x25, 0x33, 0x1d));

    // Width of the line-number column, sized to the largest number shown.
    let max_no = base + old_lines.len().max(new_lines.len());
    let num_w = max_no.to_string().len();

    // Each row is `<sign> <line-no>  <text>`: the -/+/space sign sits directly
    // in front of the line number so the two read as one unit. Pad every row to
    // a common width so the colored backgrounds form even blocks.
    let text_w = ops
        .iter()
        .zip(&keep)
        .filter(|&(_, &k)| k)
        .map(|(op, _)| {
            let text = match *op {
                DiffOp::Equal(i) | DiffOp::Delete(i) => old_lines[i],
                DiffOp::Insert(j) => new_lines[j],
            };
            UnicodeWidthStr::width(text)
        })
        .max()
        .unwrap_or(0);
    let row_w = 2 + num_w + 2 + text_w; // "<sign> " + number + "  " + text
    let row = |sign: char, n: usize, text: &str| {
        let s = format!("{sign} {n:>num_w$}  {text}");
        let extra = row_w.saturating_sub(UnicodeWidthStr::width(s.as_str()));
        format!("{s}{}", " ".repeat(extra))
    };

    let mut lines = Vec::new();
    let mut pending_gap = false; // a collapsed run precedes the next kept row
    for (idx, op) in ops.iter().enumerate() {
        if !keep[idx] {
            pending_gap = true;
            continue;
        }
        // Emit a single separator for any run of collapsed lines, but not
        // before the very first rendered row.
        if pending_gap && !lines.is_empty() {
            let blank = " ".repeat(num_w);
            lines.push(Line::from(Span::styled(format!("    {blank}\u{22EF}"), context)));
        }
        pending_gap = false;
        let line = match *op {
            DiffOp::Equal(i) => Span::styled(row(' ', base + i, old_lines[i]), context),
            DiffOp::Delete(i) => Span::styled(row('-', base + i, old_lines[i]), removed),
            DiffOp::Insert(j) => Span::styled(row('+', base + j, new_lines[j]), added),
        };
        lines.push(Line::from(line));
    }
    lines
}

fn push_field(lines: &mut Vec<Line<'static>>, key: &str, value: &str, style: Style) {
    let value_lines: Vec<&str> = value.lines().collect();
    if value_lines.len() <= 1 {
        lines.push(Line::from(Span::styled(format!("  {key}: {value}"), style)));
    } else {
        lines.push(Line::from(Span::styled(format!("  {key}:"), style)));
        for line in value_lines {
            lines.push(Line::from(Span::styled(format!("    {line}"), style)));
        }
    }
}

fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_ops_keeps_unchanged_middle_line() {
        // Only the first and last lines change; the middle line is identical
        // and must be reported as Equal, not Delete+Insert.
        let old = ["a", "keep", "b"];
        let new = ["A", "keep", "B"];
        let ops = diff_ops(&old, &new);
        let equals = ops
            .iter()
            .filter(|op| matches!(op, DiffOp::Equal(..)))
            .count();
        assert_eq!(equals, 1, "the shared middle line should match");
    }

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn diff_lines_keeps_context_between_separate_hunks() {
        // Two changes far apart: the big unchanged middle collapses to a single
        // `⋯`, but a few unchanged lines survive on each side of each change.
        let old: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        let mut new_v = old.clone();
        new_v[2] = "CHANGED_A".into();
        new_v[27] = "CHANGED_B".into();
        let rendered = plain(&diff_lines(&old.join("\n"), &new_v.join("\n"), 1));
        let sep = rendered.iter().filter(|l| l.contains('\u{22EF}')).count();
        assert_eq!(sep, 1, "exactly one collapse separator");
        assert!(rendered.iter().any(|l| l.contains("CHANGED_A")));
        assert!(rendered.iter().any(|l| l.contains("CHANGED_B")));
        // Context preserved on the inner side of each hunk...
        assert!(rendered.iter().any(|l| l.contains("line5")), "context after hunk A");
        assert!(rendered.iter().any(|l| l.contains("line24")), "context before hunk B");
        // ...while the distant middle is hidden.
        assert!(!rendered.iter().any(|l| l.contains("line15")), "middle hidden");
    }

    #[test]
    fn diff_lines_collapses_distant_context() {
        // A long unchanged head with a single trailing change should collapse
        // the far context into one separator row.
        let old = (0..20).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let mut new_v: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        *new_v.last_mut().unwrap() = "changed".into();
        let new = new_v.join("\n");
        let rendered = diff_lines(&old, &new, 1);
        // Far fewer than 20 rows survive once distant context is collapsed.
        assert!(rendered.len() < 12, "distant context should collapse, got {}", rendered.len());
    }
}
