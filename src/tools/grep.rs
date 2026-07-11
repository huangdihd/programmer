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

pub const NAME: &str = "grep";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Search for a regex pattern across files in a directory tree. Returns matching \
         file paths, line numbers, and line contents. Useful for finding definitions, \
         usages, or patterns in the codebase.",
        json!({
            "pattern": {
                "type": "string",
                "description": "The regex pattern to search for (Rust regex syntax)."
            },
            "path": {
                "type": "string",
                "description": "Optional directory or file to search. Default: the project directory."
            },
            "include": {
                "type": "string",
                "description": "Optional file glob pattern to filter included files (e.g. '*.rs', '*.{rs,toml}')."
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
    #[serde(default)]
    include: Option<String>,
}

/// Maximum total matches to return so a broad search can't blow up the context.
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

    if let Err(error) = search(&root, &re, &args.include, &mut results, &mut count) {
        return format!("error: {error}");
    }

    if results.is_empty() {
        return format!("no matches found for pattern '{}'", args.pattern);
    }

    if count >= MAX_MATCHES {
        results.push(format!(
            "[truncated: {count} matches, showing first {MAX_MATCHES}]"
        ));
    }

    results.join("\n")
}

fn search(
    root: &str,
    re: &Regex,
    include: &Option<String>,
    results: &mut Vec<String>,
    count: &mut usize,
) -> Result<(), String> {
    let metadata = std::fs::metadata(root).map_err(|e| format!("cannot access {root}: {e}"))?;

    if metadata.is_file() {
        search_file(root, re, results, count);
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

        // Skip hidden directories and common non-source dirs.
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
            }
            // Recursively search subdirectories
            if let Err(_) = search(&path_str, re, include, results, count) {
                continue;
            }
        } else if path.is_file() {
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if should_skip_file(file_name, include) {
                continue;
            }
            search_file(&path_str, re, results, count);
        }
    }
    Ok(())
}

fn should_skip_file(file_name: &str, include: &Option<String>) -> bool {
    if let Some(glob) = include {
        return !simple_glob_match(glob, file_name);
    }
    // Skip binary-looking files by extension.
    matches!(
        std::path::Path::new(file_name)
            .extension()
            .and_then(|e| e.to_str()),
        Some(
            "o" | "a"
                | "so"
                | "dylib"
                | "dll"
                | "exe"
                | "bin"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "ico"
                | "svg"
                | "mp3"
                | "mp4"
                | "avi"
                | "mov"
                | "pdf"
                | "zip"
                | "tar"
                | "gz"
                | "bz2"
                | "xz"
                | "7z"
                | "ttf"
                | "otf"
                | "woff"
                | "woff2"
                | "wasm"
                | "class"
                | "pyc"
                | "pyo"
        )
    )
}

/// Simple glob matching that supports `*` wildcards and `{a,b}` alternation.
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    // Handle {a,b} alternation by trying each alternative.
    if let Some(start) = pattern.find('{') {
        if let Some(end) = pattern[start..].find('}') {
            let end = start + end;
            let prefix = &pattern[..start];
            let alts = &pattern[start + 1..end];
            let suffix = &pattern[end + 1..];
            for alt in alts.split(',') {
                let candidate = format!("{prefix}{alt}{suffix}");
                if simple_glob_match(&candidate, name) {
                    return true;
                }
            }
            return false;
        }
    }
    // Simple wildcard matching: * matches anything.
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == name;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    // Handle leading/trailing wildcards.
    if parts.len() == 1 {
        return name == parts[0];
    }
    let mut remaining = name;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // Must start with this prefix.
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if i == parts.len() - 1 {
            // Must end with this suffix.
            if !remaining.ends_with(part) {
                return false;
            }
        } else {
            // Find the part somewhere in remaining.
            match remaining.find(part) {
                Some(pos) => remaining = &remaining[pos + part.len()..],
                None => return false,
            }
        }
    }
    true
}

fn search_file(path: &str, re: &Regex, results: &mut Vec<String>, count: &mut usize) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return, // skip binary / unreadable files silently
    };

    // Quick pre-check: does the file contain the pattern anywhere?
    if !re.is_match(&content) {
        return;
    }

    for (line_num, line) in content.lines().enumerate() {
        if *count >= MAX_MATCHES {
            return;
        }
        if re.is_match(line) {
            let trimmed = line.trim();
            results.push(format!("{}:{}: {}", path, line_num + 1, trimmed));
            *count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_glob_basic() {
        assert!(simple_glob_match("*.rs", "foo.rs"));
        assert!(!simple_glob_match("*.rs", "foo.toml"));
        assert!(simple_glob_match("*.{rs,toml}", "foo.rs"));
        assert!(simple_glob_match("*.{rs,toml}", "foo.toml"));
        assert!(!simple_glob_match("*.{rs,toml}", "foo.py"));
        assert!(simple_glob_match("*", "anything"));
        assert!(simple_glob_match("main.*", "main.rs"));
    }

    #[tokio::test]
    async fn grep_finds_matches() {
        let dir = std::env::temp_dir().join(format!("programmer_grep_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let file_a = dir.join("a.rs");
        let file_b = dir.join("b.txt");
        std::fs::write(&file_a, "fn hello() {\n    println!(\"world\");\n}\n").unwrap();
        std::fs::write(&file_b, "hello from b\n").unwrap();

        let json_path = dir.to_string_lossy().replace('\\', "\\\\");

        let out = run(&format!(r#"{{"pattern":"hello","path":"{json_path}"}}"#)).await;
        assert!(out.contains("a.rs:1:"), "got: {out}");
        assert!(out.contains("b.txt:1:"), "got: {out}");

        let out_filtered = run(&format!(
            r#"{{"pattern":"hello","path":"{json_path}","include":"*.rs"}}"#
        ))
        .await;
        assert!(out_filtered.contains("a.rs"), "got: {out_filtered}");
        assert!(!out_filtered.contains("b.txt"), "got: {out_filtered}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_reports_no_matches() {
        let dir =
            std::env::temp_dir().join(format!("programmer_grep_empty_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let json_path = dir.to_string_lossy().replace('\\', "\\\\");

        let out = run(&format!(
            r#"{{"pattern":"zzz_nonexistent_xyz","path":"{json_path}"}}"#
        ))
        .await;
        assert!(out.starts_with("no matches found"), "got: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_rejects_invalid_regex() {
        let out = run(r#"{"pattern":"[invalid"}"#).await;
        assert!(out.starts_with("error: invalid regex"), "got: {out}");
    }
}
