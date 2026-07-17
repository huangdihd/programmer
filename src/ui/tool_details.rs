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

//! Human-readable formatting for tool-call argument previews in the approval UI.

/// Extract the most salient fields from a tool call's JSON arguments and
/// return them as short, human-readable lines suitable for display in the
/// approval prompt.
pub(crate) fn format_tool_details(tool_name: &str, arguments: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(_) => return vec![arguments.to_string()],
    };
    match tool_name {
        "command" => {
            let mut lines = Vec::new();
            if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
                lines.push(format!("command: {cmd}"));
            }
            if let Some(dir) = v.get("dir").and_then(|d| d.as_str()) {
                lines.push(format!("  dir: {dir}"));
            }
            if let Some(t) = v.get("timeout") {
                lines.push(format!("  timeout: {t}s"));
            }
            if lines.is_empty() {
                lines.push(arguments.to_string());
            }
            lines
        }
        "task" => {
            let mut lines = Vec::new();
            if let Some(action) = v.get("action").and_then(|a| a.as_str()) {
                lines.push(format!("action: {action}"));
            }
            if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
                lines.push(format!("  command: {cmd}"));
            }
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                lines.push(format!("  name: {name}"));
            }
            if let Some(dir) = v.get("dir").and_then(|d| d.as_str()) {
                lines.push(format!("  dir: {dir}"));
            }
            if let Some(id) = v.get("id") {
                lines.push(format!("  task id: {id}"));
            }
            if lines.is_empty() {
                lines.push(arguments.to_string());
            }
            lines
        }
        "write_file" => {
            let mut lines = Vec::new();
            if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                lines.push(format!("path: {path}"));
            }
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                let preview: String = content.lines().take(5).collect::<Vec<_>>().join("\n");
                let tail = if content.lines().count() > 5 { "…" } else { "" };
                lines.push(format!(
                    "content: {preview}{tail} ({len} bytes)",
                    len = content.len()
                ));
            }
            if lines.is_empty() {
                lines.push(arguments.to_string());
            }
            lines
        }
        "edit_file" => {
            let mut lines = Vec::new();
            if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                lines.push(format!("path: {path}"));
            }
            let old = v.get("old_string").and_then(|o| o.as_str());
            let new = v.get("new_string").and_then(|n| n.as_str());
            if let (Some(old), Some(new)) = (old, new) {
                // Show the change as a compact unified diff (up to 6 lines each
                // side) so the approval prompt reveals what actually changes.
                for line in old.lines().take(6) {
                    lines.push(format!("- {line}"));
                }
                if old.lines().count() > 6 {
                    lines.push("- …".to_string());
                }
                for line in new.lines().take(6) {
                    lines.push(format!("+ {line}"));
                }
                if new.lines().count() > 6 {
                    lines.push("+ …".to_string());
                }
            }
            if lines.is_empty() {
                lines.push(arguments.to_string());
            }
            lines
        }
        "read_file" => {
            let mut lines = Vec::new();
            if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                lines.push(format!("path: {path}"));
            }
            if let Some(offset) = v.get("offset") {
                lines.push(format!("  offset: {offset}"));
            }
            if let Some(limit) = v.get("limit") {
                lines.push(format!("  limit: {limit}"));
            }
            lines
        }
        _ => vec![arguments.to_string()],
    }
}
