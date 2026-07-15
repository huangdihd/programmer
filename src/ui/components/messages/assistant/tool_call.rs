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
}

impl<'a> ToolCallMessage<'a> {
    pub fn new(call: &'a FunctionToolCall) -> Self {
        Self {
            call,
            output: None,
            failed: false,
            expanded: false,
            approval_label: None,
        }
    }

    pub fn output(mut self, output: Option<&'a FunctionCallOutputItemParam>) -> Self {
        self.output = output;
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
        }

        Text::from(lines)
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
