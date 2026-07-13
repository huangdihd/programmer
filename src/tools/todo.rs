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

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;
use crate::todos::TodoList;

pub const NAME: &str = "todo";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Manage a project-level todo list for tracking tasks across the \
         coding session. Use this to plan, track progress, and demonstrate \
         thoroughness. Actions: \
         `list` (show all todos), \
         `add` (create a new todo with title and optional description), \
         `update` (modify title/description/status by id), \
         `delete` (remove a todo by id).",
        json!({
            "action": {
                "type": "string",
                "description": "The action to perform: list, add, update, or delete.",
                "enum": ["list", "add", "update", "delete"]
            },
            "id": {
                "type": "string",
                "description": "The todo id (required for update and delete)."
            },
            "title": {
                "type": "string",
                "description": "Title of the todo (required for add, optional for update)."
            },
            "description": {
                "type": "string",
                "description": "Optional longer description of the todo."
            },
            "status": {
                "type": "string",
                "description": "Status to set: pending, in_progress, completed, or cancelled \
                                (defaults to pending for new todos, optional for update).",
                "enum": ["pending", "in_progress", "completed", "cancelled"]
            }
        }),
        &["action"],
    )
}

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

pub async fn run(arguments: &str) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(a) => a,
        Err(e) => return format!("error: invalid arguments: {e}"),
    };

    let mut list = TodoList::load();

    let result = match args.action.as_str() {
        "list" => list.render_table(),

        "add" => {
            let title = match args.title {
                Some(ref t) if !t.trim().is_empty() => t.clone(),
                _ => return "error: 'title' is required for add".to_string(),
            };
            let todo = list.add(title, args.description);
            // Snapshot fields before saving (save needs &mut list).
            let id = todo.id.clone();
            let todo_title = todo.title.clone();
            let has_desc = todo.description.is_some();
            let _ = list.save_to_file();
            let desc_note = if has_desc { " (with description)" } else { "" };
            format!("created todo {id}: {todo_title}{desc_note}")
        }

        "update" => {
            let id = match args.id {
                Some(ref i) if !i.trim().is_empty() => i.clone(),
                _ => return "error: 'id' is required for update".to_string(),
            };
            let status = args
                .status
                .as_deref()
                .and_then(crate::todos::TodoStatus::parse);
            match list.update(&id, args.title, args.description, status) {
                Ok(todo) => {
                    let tid = todo.id.clone();
                    let ttitle = todo.title.clone();
                    let tstatus = todo.status.label();
                    let _ = list.save_to_file();
                    format!("updated todo {tid}: title={ttitle:?} status={tstatus}")
                }
                Err(e) => e,
            }
        }

        "delete" => {
            let id = match args.id {
                Some(ref i) if !i.trim().is_empty() => i.clone(),
                _ => return "error: 'id' is required for delete".to_string(),
            };
            match list.delete(&id) {
                Ok(()) => {
                    let _ = list.save_to_file();
                    format!("deleted todo {id}")
                }
                Err(e) => e,
            }
        }

        other => format!(
            "error: unknown action '{other}' — use list, add, update, or delete"
        ),
    };

    result
}
