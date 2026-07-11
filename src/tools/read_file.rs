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
            },
            "offset": {
                "type": "integer",
                "description": "Optional 1-based line number to start reading from. Default: 1 (beginning of file)."
            },
            "limit": {
                "type": "integer",
                "description": "Optional maximum number of lines to read. Default: read the entire file."
            }
        }),
        &["path"],
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
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

    slice_lines(contents, args.offset, args.limit)
}

fn slice_lines(contents: String, offset: Option<usize>, limit: Option<usize>) -> String {
    let start = offset.map(|n| n.saturating_sub(1)).unwrap_or(0);
    let lines: Vec<&str> = contents.lines().collect();

    if start >= lines.len() {
        return format!(
            "offset {} exceeds file length ({} lines); file has no content at this offset.",
            start + 1,
            lines.len()
        );
    }

    let end = match limit {
        Some(lim) => (start + lim).min(lines.len()),
        None => lines.len(),
    };

    lines[start..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_lines_full() {
        let out = slice_lines("a\nb\nc\n".to_string(), None, None);
        assert_eq!(out, "a\nb\nc");
    }

    #[test]
    fn slice_lines_offset_only() {
        let out = slice_lines("a\nb\nc\n".to_string(), Some(2), None);
        assert_eq!(out, "b\nc");
    }

    #[test]
    fn slice_lines_limit_only() {
        let out = slice_lines("a\nb\nc\n".to_string(), None, Some(2));
        assert_eq!(out, "a\nb");
    }

    #[test]
    fn slice_lines_offset_and_limit() {
        let out = slice_lines("a\nb\nc\nd\n".to_string(), Some(2), Some(2));
        assert_eq!(out, "b\nc");
    }

    #[test]
    fn slice_lines_offset_past_end() {
        let out = slice_lines("a\nb\n".to_string(), Some(10), None);
        assert!(out.contains("offset 10 exceeds file length (2 lines)"));
    }
}
