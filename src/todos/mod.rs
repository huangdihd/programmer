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

//! Per-session todo list.
//!
//! Todos are stored as part of the session JSON (`Session::todos`). A
//! well-known global file at `~/.config/programmer/todos.json` acts as the
//! communication channel between the `todo` tool (which runs in a spawned
//! task and can't access App state directly) and the App. The App syncs the
//! file with its in-memory `TodoList` on startup, after tool calls, and
//! before saving the session.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    /// Cycle to the next non-terminal status.
    pub fn next(&self) -> Self {
        match self {
            TodoStatus::Pending => TodoStatus::InProgress,
            TodoStatus::InProgress => TodoStatus::Completed,
            TodoStatus::Completed => TodoStatus::Pending,
            TodoStatus::Cancelled => TodoStatus::Pending,
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        }
    }

    /// Icon for TUI display.
    pub fn icon(&self) -> &'static str {
        match self {
            TodoStatus::Pending => " ",
            TodoStatus::InProgress => ">",
            TodoStatus::Completed => "✓",
            TodoStatus::Cancelled => "✗",
        }
    }

    /// Parse from a string (case-insensitive). Returns None for unrecognised.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" | "inprogress" | "in-progress" => Some(TodoStatus::InProgress),
            "completed" | "done" | "complete" => Some(TodoStatus::Completed),
            "cancelled" | "canceled" | "cancel" => Some(TodoStatus::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: TodoStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TodoList {
    pub todos: Vec<Todo>,
}

// ---------------------------------------------------------------------------
// Global file path (communication channel between App and tool)
// ---------------------------------------------------------------------------

/// Returns the path to the well-known todos file used by both the tool and
/// the App: `~/.config/programmer/todos.json`.
pub fn todos_file_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("programmer");
    Some(dir.join("todos.json"))
}

// ---------------------------------------------------------------------------
// File I/O (used by the tool; App syncs its in-memory list separately)
// ---------------------------------------------------------------------------

impl TodoList {
    /// Load from the global todos file. An unparseable file is set aside as
    /// `todos.json.corrupt` (not deleted — it may just be a schema mismatch
    /// with another version of the program).
    pub fn load() -> Self {
        let Some(path) = todos_file_path() else {
            return TodoList::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(list) => list,
                Err(_) => {
                    let quarantine = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(&path, &quarantine);
                    TodoList::default()
                }
            },
            Err(_) => TodoList::default(),
        }
    }

    /// Save to the global todos file atomically, creating parent directories
    /// as needed. Uses temp-file + rename so a crash never leaves a truncated file.
    pub fn save_to_file(&self) -> Result<(), String> {
        let Some(path) = todos_file_path() else {
            return Err("cannot locate config directory".to_string());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create config dir: {e}"))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialisation error: {e}"))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write error: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename error: {e}"))?;
        Ok(())
    }

    /// Delete the global todos file (e.g. on /clear or /new).
    pub fn clear_file() {
        if let Some(path) = todos_file_path() {
            let _ = std::fs::remove_file(&path);
        }
    }

    // -- mutations --

    pub fn add(&mut self, title: String, description: Option<String>) -> &Todo {
        let id = generate_id();
        let now = now_secs();
        self.todos.push(Todo {
            id,
            title,
            description,
            status: TodoStatus::Pending,
            created_at: now,
            updated_at: now,
        });
        self.todos.last().unwrap()
    }

    pub fn update(
        &mut self,
        id: &str,
        title: Option<String>,
        description: Option<String>,
        status: Option<TodoStatus>,
    ) -> Result<&Todo, String> {
        let todo = self
            .todos
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("todo not found: {id}"))?;
        if let Some(t) = title {
            todo.title = t;
        }
        todo.description = description.or(todo.description.take());
        if let Some(s) = status {
            todo.status = s;
        }
        todo.updated_at = now_secs();
        Ok(todo)
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let idx = self
            .todos
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| format!("todo not found: {id}"))?;
        self.todos.remove(idx);
        Ok(())
    }

    pub fn toggle_status(&mut self, id: &str) -> Result<&Todo, String> {
        let todo = self
            .todos
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("todo not found: {id}"))?;
        todo.status = todo.status.next();
        todo.updated_at = now_secs();
        Ok(todo)
    }

    /// Render the entire list as a human-readable table for the tool output.
    pub fn render_table(&self) -> String {
        if self.todos.is_empty() {
            return "No todos. Use `todo add` to create one.".to_string();
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "{:<12} {:<12} {:<50} {}",
            "ID", "STATUS", "TITLE", "DESCRIPTION"
        ));
        lines.push("-".repeat(120));

        for t in &self.todos {
            let short_id = &t.id[..t.id.len().min(10)];
            let desc = t
                .description
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(50)
                .collect::<String>();
            lines.push(format!(
                "{:<12} {:<12} {:<50} {}",
                short_id,
                t.status.label(),
                truncate_str(&t.title, 50),
                desc,
            ));
        }
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let random: u32 = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
        ^ std::process::id())
        .wrapping_mul(1_103_515_245);
    format!("{:x}{:08x}", millis, random)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_cycle() {
        assert_eq!(TodoStatus::Pending.next(), TodoStatus::InProgress);
        assert_eq!(TodoStatus::InProgress.next(), TodoStatus::Completed);
        assert_eq!(TodoStatus::Completed.next(), TodoStatus::Pending);
        assert_eq!(TodoStatus::Cancelled.next(), TodoStatus::Pending);
    }

    #[test]
    fn status_parse() {
        assert_eq!(TodoStatus::parse("pending"), Some(TodoStatus::Pending));
        assert_eq!(TodoStatus::parse("InProgress"), Some(TodoStatus::InProgress));
        assert_eq!(TodoStatus::parse("done"), Some(TodoStatus::Completed));
        assert_eq!(TodoStatus::parse("cancel"), Some(TodoStatus::Cancelled));
        assert_eq!(TodoStatus::parse("bogus"), None);
    }

    #[test]
    fn add_update_delete() {
        let mut list = TodoList::default();
        assert!(list.todos.is_empty());

        let id = {
            let t = list.add("test item".into(), Some("desc".into()));
            assert_eq!(t.title, "test item");
            assert_eq!(t.status, TodoStatus::Pending);
            t.id.clone()
        };
        assert_eq!(list.todos.len(), 1);

        list.update(&id, Some("renamed".into()), None, Some(TodoStatus::Completed))
            .unwrap();
        assert_eq!(list.todos[0].title, "renamed");
        assert_eq!(list.todos[0].status, TodoStatus::Completed);

        list.toggle_status(&id).unwrap();
        assert_eq!(list.todos[0].status, TodoStatus::Pending);

        list.delete(&id).unwrap();
        assert!(list.todos.is_empty());

        // Delete non-existent
        assert!(list.delete(&id).is_err());
    }

    #[test]
    fn todos_file_path_is_in_config_dir() {
        let path = todos_file_path();
        // The path is deterministic; it should end with `programmer/todos.json`.
        if let Some(p) = path {
            assert!(p.ends_with("programmer/todos.json"), "got: {p:?}");
        }
    }
}
