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
use super::grep::simple_glob_match;
use super::search_budget::SearchBudget;

pub const NAME: &str = "blob";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Find files by name using a glob pattern (e.g. '*.rs', 'auth_*.{rs,toml}'). \
         Walks the directory tree and returns matching files until the search \
         result or traversal budget is reached. \
         Useful for locating files when you know (part of) the filename but \
         not the path.",
        json!({
            "pattern": {
                "type": "string",
                "description": "Glob pattern matched against file names (not paths): `*` matches any run of characters, `{a,b}` matches alternatives. A leading `**/` is accepted and ignored."
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

pub async fn run(arguments: &str) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return Err(format!("error: invalid arguments: {error}")),
    };

    tokio::task::spawn_blocking(move || run_blocking(args))
        .await
        .map_err(|error| format!("error: file search task failed: {error}"))?
}

fn run_blocking(args: Args) -> Result<String, String> {
    // Matching is against file names only, so a `**/` path prefix (a common
    // way to write "at any depth") is redundant — accept and drop it.
    let pattern = args.pattern.strip_prefix("**/").unwrap_or(&args.pattern);
    if pattern.is_empty() {
        return Err("error: empty pattern".to_string());
    }

    let root = args.path.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    let mut results = Vec::new();
    let mut budget = SearchBudget::default();

    if let Err(error) = walk(&root, pattern, &mut results, &mut budget) {
        return Err(format!("error: {error}"));
    }

    if results.is_empty() {
        results.push(format!("no files matching '{}'", args.pattern));
    }
    if let Some(notice) = budget.notice() {
        results.push(notice);
    }

    Ok(results.join("\n"))
}

fn walk(
    root: &str,
    pattern: &str,
    results: &mut Vec<String>,
    budget: &mut SearchBudget,
) -> Result<(), String> {
    let metadata = std::fs::metadata(root).map_err(|e| format!("cannot access {root}: {e}"))?;

    if metadata.is_file() {
        check_file(root, pattern, results, budget);
        return Ok(());
    }

    let entries =
        std::fs::read_dir(root).map_err(|e| format!("cannot read directory {root}: {e}"))?;

    for entry in entries {
        if budget.is_exhausted() || !budget.try_visit_entry() {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let path_str = path.to_string_lossy();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && (name.starts_with('.') || name == "target" || name == "node_modules")
            {
                continue;
            }
            if walk(&path_str, pattern, results, budget).is_err() {
                continue;
            }
        } else if file_type.is_file() {
            check_file(&path_str, pattern, results, budget);
        }
    }
    Ok(())
}

fn check_file(path: &str, pattern: &str, results: &mut Vec<String>, budget: &mut SearchBudget) {
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if simple_glob_match(pattern, file_name) && budget.try_record_match() {
        results.push(path.to_string());
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
            r#"{{"pattern":"auth_*.rs","path":"{json_path}"}}"#
        ))
        .await
        .expect("blob should succeed");
        assert!(out.contains("auth_service.rs"), "got: {out}");
        assert!(out.contains("auth_test.rs"), "got: {out}");
        assert!(!out.contains("main.rs"), "got: {out}");
        assert!(!out.contains("config.toml"), "got: {out}");

        // The most common first attempt: a plain `*.ext` glob must work.
        let star = run(&format!(r#"{{"pattern":"*.rs","path":"{json_path}"}}"#))
            .await
            .expect("glob should succeed");
        assert!(star.contains("main.rs"), "got: {star}");
        assert!(!star.contains("config.toml"), "got: {star}");

        // A `**/` prefix is tolerated and matches at any depth.
        let deep = run(&format!(
            r#"{{"pattern":"**/*.toml","path":"{json_path}"}}"#
        ))
        .await
        .expect("**/ prefix should succeed");
        assert!(deep.contains("config.toml"), "got: {deep}");

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
        .await
        .expect("blob should succeed with no matches");
        assert!(out.starts_with("no files matching"), "got: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn blob_rejects_empty_pattern() {
        let out = run(r#"{"pattern":""}"#)
            .await
            .expect_err("empty pattern should fail");
        assert!(out.starts_with("error: empty pattern"), "got: {out}");
    }

    #[tokio::test]
    async fn blob_stops_after_the_result_budget() {
        let dir =
            std::env::temp_dir().join(format!("programmer_blob_limit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        for index in 0..250 {
            std::fs::write(dir.join(format!("match_{index}.rs")), "").unwrap();
        }
        let json_path = dir.to_string_lossy().replace('\\', "\\\\");

        let out = run(&format!(r#"{{"pattern":"*.rs","path":"{json_path}"}}"#))
            .await
            .expect("blob should stop cleanly");

        assert_eq!(out.lines().count(), 201, "got: {out}");
        assert!(out.contains("[truncated: showing the first 200 matches"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn blob_does_not_follow_directory_symlinks() {
        let dir =
            std::env::temp_dir().join(format!("programmer_blob_symlink_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("source")).unwrap();
        std::fs::write(dir.join("source").join("only.rs"), "").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("source").join("loop")).unwrap();
        let json_path = dir.to_string_lossy().replace('\\', "\\\\");

        let out = run(&format!(r#"{{"pattern":"*.rs","path":"{json_path}"}}"#))
            .await
            .expect("blob should ignore the symlink loop");

        assert_eq!(out.lines().count(), 1, "got: {out}");
        assert!(out.contains("only.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
