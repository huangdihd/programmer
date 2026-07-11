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
         appear exactly once in the file so the edit is unambiguous.",
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
}

pub async fn run(arguments: &str) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return format!("error: invalid arguments: {error}"),
    };

    let contents = match tokio::fs::read_to_string(&args.path).await {
        Ok(contents) => contents,
        Err(error) => return format!("error: could not read {}: {error}", args.path),
    };

    match contents.matches(&args.old_string).count() {
        0 => return format!("error: old_string not found in {}", args.path),
        1 => {}
        n => {
            return format!(
                "error: old_string appears {n} times in {}; add more surrounding \
                 context so it is unique",
                args.path
            );
        }
    }

    let updated = contents.replacen(&args.old_string, &args.new_string, 1);
    match tokio::fs::write(&args.path, updated).await {
        Ok(()) => format!("edited {}", args.path),
        Err(error) => format!("error: could not write {}: {error}", args.path),
    }
}
