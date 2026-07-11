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
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;

pub const NAME: &str = "blob";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Find files by name using a regex pattern. Walks the directory tree and \
         returns every file whose name matches the pattern. Useful for locating \
         files when you know (part of) the filename but not the path.",
        json!({
            "pattern": {
                "type": "string",
                "description": "The regex pattern to match against filenames (Rust regex syntax)."
            },
            "path": {
                "type": "string",
                "description": "Optional directory to search. Default: the project directory."
            }
        }),
        &["pattern"],
    )
}

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

const MAX_MATCHES: usize = 200;

pub async fn run(arguments: &str) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return format!("error: invalid arguments: {error}"),
    };

    let re = match Regex::new(&args.pattern) {
        Ok(re) => re,
        Err(error) => return format!("error: invalid regex: {error}"),
    };

    let root = args.path.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    let mut results = Vec::new();
    let mut count: usize = 0;

    if let Err(error) = walk(&root, &re, &mut results, &mut count) {
        return format!("error: {error}");
    }

    if results.is_empty() {
        return format!("no files matching '{}'", args.pattern);
    }

    if count >= MAX_MATCHES {
        results.push(format!(
            "[truncated: {} matches, showing first {MAX_MATCHES}]",
            count
        ));
    }

    results.join("\n")
}

fn walk(
    root: &str,
    re: &Regex,
    results: &mut Vec<String>,
    count: &mut usize,
) -> Result<(), String> {
    let metadata = std::fs::metadata(root).map_err(|e| format!("cannot access {root}: {e}"))?;

    if metadata.is_file() {
        check_file(root, re, results, count);
        return Ok(());
    }

    let entries =
        std::fs::read_dir(root).map_err(|e| format!("cannot read directory {root}: {e}"))?;

    for entry in entries {
        if *count >= MAX_MATCHES {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let path_str = path.to_string_lossy();

        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
            }
            if walk(&path_str, re, results, count).is_err() {
                continue;
            }
        } else if path.is_file() {
            check_file(&path_str, re, results, count);
        }
    }
    Ok(())
}

fn check_file(path: &str, re: &Regex, results: &mut Vec<String>, count: &mut usize) {
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if re.is_match(file_name) {
        results.push(path.to_string());
        *count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blob_finds_files_by_name() {
        let dir = std::env::temp_dir().join(format!("programmer_blob_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("auth_service.rs"), "").unwrap();
        std::fs::write(dir.join("auth_test.rs"), "").unwrap();
        std::fs::write(dir.join("main.rs"), "").unwrap();
        std::fs::write(dir.join("config.toml"), "").unwrap();

        let json_path = dir.to_string_lossy().replace('\\', "\\\\");

        let out = run(&format!(
            r#"{{"pattern":"auth_.*\\.rs","path":"{json_path}"}}"#
        ))
        .await;
        assert!(out.contains("auth_service.rs"), "got: {out}");
        assert!(out.contains("auth_test.rs"), "got: {out}");
        assert!(!out.contains("main.rs"), "got: {out}");
        assert!(!out.contains("config.toml"), "got: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn blob_reports_no_matches() {
        let dir =
            std::env::temp_dir().join(format!("programmer_blob_empty_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let json_path = dir.to_string_lossy().replace('\\', "\\\\");

        let out = run(&format!(
            r#"{{"pattern":"zzz_nonexistent","path":"{json_path}"}}"#
        ))
        .await;
        assert!(out.starts_with("no files matching"), "got: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn blob_rejects_invalid_regex() {
        let out = run(r#"{"pattern":"[invalid"}"#).await;
        assert!(out.starts_with("error: invalid regex"), "got: {out}");
    }
}
