// Copyright (C) 2025 huangdihd
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

use async_openai::types::responses::FunctionToolCall;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::ui::components::messages::assistant::detail_style;
use crate::ui::markdown_theme::palette;

/// Renders a tool call the model made. Collapsed it is a single line
/// (`⚡ command  <summary> ▸`); expanded it shows each argument in full. A caret
/// hints that the line can be clicked to toggle.
pub struct ToolCallMessage<'a> {
    call: &'a FunctionToolCall,
    expanded: bool,
}

impl<'a> ToolCallMessage<'a> {
    pub fn new(call: &'a FunctionToolCall) -> Self {
        Self {
            call,
            expanded: false,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn into_text(self) -> Text<'static> {
        let accent = Style::new()
            .fg(palette::YELLOW)
            .add_modifier(Modifier::BOLD);
        let muted = Style::new().fg(palette::MUTED);

        let value = serde_json::from_str::<serde_json::Value>(&self.call.arguments).ok();

        if !self.expanded {
            // Collapsed: a single line — the tool name plus a one-line summary.
            let mut spans = vec![Span::styled(format!("🔧 {}", self.call.name), accent)];
            let summary = one_line_summary(value.as_ref(), &self.call.arguments);
            if !summary.is_empty() {
                spans.push(Span::styled(format!("  {summary}"), muted));
            }
            spans.push(Span::styled(" ▸".to_string(), muted));
            return Text::from(Line::from(spans));
        }

        // Expanded: header plus every argument in full.
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("🔧 {}", self.call.name), accent),
            Span::styled(" ▾".to_string(), detail_style()),
        ])];

        match &value {
            Some(serde_json::Value::Object(map)) => {
                for (key, value) in map {
                    push_field(&mut lines, key, &value_text(value), detail_style());
                }
            }
            _ => {
                // Arguments not parseable (e.g. still streaming); show them raw.
                for line in self.call.arguments.lines() {
                    lines.push(Line::from(Span::styled(format!("  {line}"), detail_style())));
                }
            }
        }

        Text::from(lines)
    }
}

/// A one-line summary for the collapsed view: the command or path if present,
/// otherwise the first non-empty argument value, truncated to a single line.
fn one_line_summary(value: Option<&serde_json::Value>, raw: &str) -> String {
    let first_line = |s: &str| s.lines().next().unwrap_or("").to_string();

    if let Some(serde_json::Value::Object(map)) = value {
        for key in ["command", "path"] {
            if let Some(text) = map.get(key).and_then(|v| v.as_str()) {
                return first_line(text);
            }
        }
        if let Some(text) = map.values().find_map(|v| v.as_str()) {
            return first_line(text);
        }
    }
    first_line(raw)
}

/// Renders one `key: value` argument, keeping multi-line values on their own
/// indented lines.
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

/// String values as-is; anything else as its JSON representation.
fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
