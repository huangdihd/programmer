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

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;

pub const NAME: &str = "read_file";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Read and return the contents of a text file. The path may be absolute or \
         relative to the working directory.",
        json!({
            "path": {
                "type": "string",
                "description": "Path to the file to read."
            }
        }),
        &["path"],
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
}

pub async fn run(arguments: &str) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return format!("error: invalid arguments: {error}"),
    };

    match tokio::fs::read_to_string(&args.path).await {
        Ok(contents) => contents,
        Err(error) => format!("error: could not read {}: {error}", args.path),
    }
}
