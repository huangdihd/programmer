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

pub const NAME: &str = "edit_file";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Replace an exact substring in a file with new text. `old_string` must \
         appear exactly once in the file so the edit is unambiguous. \
         Use `offset` and `limit` to restrict the search to a specific line \
         range (1-based) when the string appears multiple times.",
        json!({
            "path": {
                "type": "string",
                "description": "Path to the file to edit."
            },
            "old_string": {
                "type": "string",
                "description": "The exact text to replace. Must be unique in the file."
            },
            "new_string": {
                "type": "string",
                "description": "The text to replace it with."
            },
            "offset": {
                "type": "integer",
                "description": "Optional 1-based line number to start searching from."
            },
            "limit": {
                "type": "integer",
                "description": "Optional number of lines to search within when offset is given."
            }
        }),
        &["path", "old_string", "new_string"],
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn run(arguments: &str) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return Err(format!("error: invalid arguments: {error}")),
    };

    let contents = match tokio::fs::read_to_string(&args.path).await {
        Ok(contents) => contents,
        Err(error) => return Err(format!("error: could not read {}: {error}", args.path)),
    };

    // Normalize CRLF → LF so old_string matching works across platforms.
    let contents = contents.replace("\r\n", "\n");
    let old_normalized = args.old_string.replace("\r\n", "\n");
    let matches_count = if let Some(offset) = args.offset {
        let limit = args.limit.unwrap_or(1);
        let lines: Vec<&str> = contents.lines().collect();
        let start = offset.saturating_sub(1); // 1-based → 0-based
        let end = (start + limit).min(lines.len());
        if start >= lines.len() {
            return Err(format!(
                "error: offset {offset} is past end of {} ({} lines)",
                args.path,
                lines.len()
            ));
        }
        let region = lines[start..end].join("\n");
        region.matches(&old_normalized).count()
    } else {
        contents.matches(&old_normalized).count()
    };

    if matches_count == 0 {
        return Err(if let Some(offset) = args.offset {
            let limit = args.limit.unwrap_or(1);
            format!(
                "error: old_string not found in {} lines {offset}-{}",
                args.path,
                offset + limit - 1
            )
        } else {
            format!("error: old_string not found in {}", args.path)
        });
    }
    if matches_count > 1 {
        if args.offset.is_some() {
            return Err(format!(
                "error: old_string appears {matches_count} times in the given range; \
                 add more surrounding context so it is unique",
            ));
        }
        return Err(format!(
            "error: old_string appears {matches_count} times in {}; add more surrounding \
             context so it is unique",
            args.path
        ));
    }

    let updated = contents.replacen(&old_normalized, &args.new_string, 1);
    match tokio::fs::write(&args.path, updated).await {
        Ok(()) => Ok(format!("edited {}", args.path)),
        Err(error) => Err(format!("error: could not write {}: {error}", args.path)),
    }
}
