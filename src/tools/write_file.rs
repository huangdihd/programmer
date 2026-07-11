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

use std::path::Path;

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;

pub const NAME: &str = "write_file";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Write text to a file, creating it (and any missing parent directories) or \
         overwriting it if it already exists.",
        json!({
            "path": {
                "type": "string",
                "description": "Path to the file to write."
            },
            "content": {
                "type": "string",
                "description": "The full contents to write to the file."
            }
        }),
        &["path", "content"],
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

pub async fn run(arguments: &str) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return format!("error: invalid arguments: {error}"),
    };

    if let Some(parent) = Path::new(&args.path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                return format!("error: could not create {}: {error}", parent.display());
            }
        }
    }

    match tokio::fs::write(&args.path, &args.content).await {
        Ok(()) => format!("wrote {} bytes to {}", args.content.len(), args.path),
        Err(error) => format!("error: could not write {}: {error}", args.path),
    }
}
