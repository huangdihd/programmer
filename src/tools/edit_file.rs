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
    let edit_properties = json!({
        "old_string": {
            "type": "string",
            "description": "The exact text to replace. Must be unique in the search range unless replace_all is true."
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
            "description": "Optional number of lines to search within when offset is given (default 1)."
        },
        "replace_all": {
            "type": "boolean",
            "default": false,
            "description": "Replace all non-overlapping matches in the search range. Default false; no matches is always an error."
        }
    });
    let mut properties = edit_properties.clone();
    properties["path"] = json!({
        "type": "string",
        "description": "Path to the file to edit."
    });
    properties["edits"] = json!({
        "type": "array",
        "minItems": 1,
        "description": "A non-empty batch of edits applied sequentially to an in-memory buffer; each edit searches the result of the preceding edit, including updated line numbers. Cannot be combined with top-level old_string, new_string, offset, limit, or replace_all. The file is written only after every edit succeeds.",
        "items": {
            "type": "object",
            "properties": edit_properties,
            "required": ["old_string", "new_string"],
            "additionalProperties": false
        }
    });
    function_tool(
        NAME,
        "Replace exact substrings in a file. Supply either top-level old_string/new_string \
         (optionally offset, limit, replace_all), or a non-empty edits array, never both. \
         Each old_string must match exactly once unless replace_all is true (default false). \
         offset/limit restrict matching to a 1-based line range. Batch edits run sequentially \
         in memory; a failed edit leaves the file unchanged. CRLF input is normalized to LF.",
        properties,
        &["path"],
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(flatten)]
    edit: Edit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    old_string: String,
    new_string: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchArgs {
    path: String,
    edits: Vec<Edit>,
}

fn parse_arguments(arguments: &str) -> Result<(String, Vec<Edit>), String> {
    let value: serde_json::Value = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    if value.get("edits").is_some() {
        if ["old_string", "new_string", "offset", "limit", "replace_all"]
            .iter()
            .any(|key| value.get(key).is_some())
        {
            return Err("error: edits cannot be combined with top-level edit fields".into());
        }
        let args: BatchArgs = serde_json::from_value(value)
            .map_err(|error| format!("error: invalid arguments: {error}"))?;
        if args.edits.is_empty() {
            return Err("error: edits must not be empty".into());
        }
        Ok((args.path, args.edits))
    } else {
        let args: Args = serde_json::from_value(value)
            .map_err(|error| format!("error: invalid arguments: {error}"))?;
        Ok((args.path, vec![args.edit]))
    }
}

pub async fn run(arguments: &str) -> Result<String, String> {
    let security = crate::security::SecurityManager::standalone()?;
    run_with_security(arguments, &security).await
}

pub(crate) async fn run_with_security(
    arguments: &str,
    security: &crate::security::SecurityManager,
) -> Result<String, String> {
    run_with_security_scope(arguments, security, 0).await
}

pub(crate) async fn run_with_security_scope(
    arguments: &str,
    security: &crate::security::SecurityManager,
    scope: u64,
) -> Result<String, String> {
    let (display_path, edits) = parse_arguments(arguments)?;
    let path =
        security.authorize_path(crate::security::policy::AccessKind::Write, &display_path)?;
    let bytes = match tokio::fs::read(&path).await {
        Ok(contents) => contents,
        Err(error) => return Err(format!("error: could not read {display_path}: {error}")),
    };
    security.validate_write_scoped(scope, &path, Some(&bytes))?;
    let contents = String::from_utf8(bytes)
        .map_err(|error| format!("error: could not read {}: {error}", path.display()))?;
    let updated = apply_edits(&contents, &display_path, &edits)?;
    match tokio::fs::write(&path, &updated).await {
        Ok(()) => {
            security.record_read_scoped(scope, &path, updated.as_bytes());
            Ok(format!("edited {display_path}"))
        }
        Err(error) => Err(format!(
            "error: could not write {}: {error}",
            path.display()
        )),
    }
}

// Validate the entire batch on an in-memory buffer before the caller writes anything.
fn apply_edits(contents: &str, path: &str, edits: &[Edit]) -> Result<String, String> {
    let mut updated = contents.replace("\r\n", "\n");
    for (index, edit) in edits.iter().enumerate() {
        updated = apply_edit(&updated, path, edit).map_err(|error| {
            if edits.len() > 1 {
                format!("error: edit {} failed: {error}", index + 1)
            } else {
                error
            }
        })?;
    }
    Ok(updated)
}

fn apply_edit(contents: &str, path: &str, args: &Edit) -> Result<String, String> {
    // Normalize CRLF → LF so old_string matching works across platforms.
    let old_normalized = args.old_string.replace("\r\n", "\n");
    let region = if let Some(offset) = args.offset {
        let limit = args.limit.unwrap_or(1);
        let line_count = contents.lines().count();
        let start_line = offset.saturating_sub(1); // 1-based → 0-based
        if start_line >= line_count {
            return Err(format!(
                "error: offset {offset} is past end of {} ({line_count} lines)",
                path
            ));
        }
        let end_line = start_line.saturating_add(limit).min(line_count);
        let mut starts = std::iter::once(0)
            .chain(
                contents
                    .match_indices('\n')
                    .map(|(index, _)| index + 1)
                    .filter(|&index| index < contents.len()),
            )
            .skip(start_line);
        let start = starts.next().unwrap();
        let end = if end_line == start_line {
            start
        } else {
            starts
                .nth(end_line - start_line - 1)
                .unwrap_or(contents.len())
        };
        Some(start..end)
    } else {
        None
    };
    let matches_count = region.as_ref().map_or_else(
        || contents.matches(&old_normalized).count(),
        |range| contents[range.clone()].matches(&old_normalized).count(),
    );

    if matches_count == 0 {
        return Err(if let Some(offset) = args.offset {
            let limit = args.limit.unwrap_or(1);
            format!(
                "error: old_string not found in {} lines {offset}-{}",
                path,
                offset.saturating_add(limit.saturating_sub(1))
            )
        } else {
            format!("error: old_string not found in {}", path)
        });
    }
    if matches_count > 1 && !args.replace_all {
        if args.offset.is_some() {
            return Err(format!(
                "error: old_string appears {matches_count} times in the given range; \
                 add more surrounding context so it is unique",
            ));
        }
        return Err(format!(
            "error: old_string appears {matches_count} times in {}; add more surrounding \
             context so it is unique",
            path
        ));
    }

    let count = if args.replace_all { matches_count } else { 1 };
    let updated = if let Some(range) = region {
        let updated_region =
            contents[range.clone()].replacen(&old_normalized, &args.new_string, count);
        format!(
            "{}{}{}",
            &contents[..range.start],
            updated_region,
            &contents[range.end..]
        )
    } else {
        contents.replacen(&old_normalized, &args.new_string, count)
    };
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(contents: &str, mut arguments: serde_json::Value) -> Result<String, String> {
        arguments["path"] = json!("file.txt");
        let (path, edits) = parse_arguments(&arguments.to_string())?;
        apply_edits(contents, &path, &edits)
    }

    #[test]
    fn legacy_unique_matching_and_ranges() {
        assert_eq!(
            apply(
                "a\r\nb\r\n",
                json!({"old_string":"a\r\n", "new_string":"c\n"})
            )
            .unwrap(),
            "c\nb\n"
        );
        assert!(
            apply("a a", json!({"old_string":"a", "new_string":"b"}))
                .unwrap_err()
                .contains("2 times")
        );
        assert_eq!(
            apply(
                "a\na\na\n",
                json!({"old_string":"a", "new_string":"b", "offset":2})
            )
            .unwrap(),
            "a\nb\na\n"
        );
        assert_eq!(
            apply(
                "a\nb\nc\n",
                json!({"old_string":"b\nc", "new_string":"d", "offset":2, "limit":2})
            )
            .unwrap(),
            "a\nd\n"
        );
        // A limit without an offset is ignored, as in the original API.
        assert_eq!(
            apply(
                "a\nb",
                json!({"old_string":"b", "new_string":"c", "limit":0})
            )
            .unwrap(),
            "a\nc"
        );
    }

    #[test]
    fn replace_all_is_non_overlapping_and_range_limited() {
        assert_eq!(
            apply(
                "aaaaa",
                json!({"old_string":"aa", "new_string":"b", "replace_all":true})
            )
            .unwrap(),
            "bba"
        );
        assert_eq!(apply("猫 猫\n猫 猫\n猫\n", json!({"old_string":"猫", "new_string":"犬", "offset":2, "limit":1, "replace_all":true})).unwrap(), "猫 猫\n犬 犬\n猫\n");
        assert!(
            apply(
                "a a",
                json!({"old_string":"a", "new_string":"b", "replace_all":false})
            )
            .is_err()
        );
        for replace_all in [true, false] {
            assert!(
                apply(
                    "a",
                    json!({"old_string":"z", "new_string":"b", "replace_all":replace_all})
                )
                .unwrap_err()
                .contains("not found")
            );
        }
    }

    #[test]
    fn batch_edits_search_preceding_results_and_updated_line_numbers() {
        let arguments = json!({"edits":[
            {"old_string":"start", "new_string":"a\na"},
            {"old_string":"a", "new_string":"b", "offset":2},
            {"old_string":"b", "new_string":"c", "replace_all":true}
        ]});
        assert_eq!(apply("start\nb\n", arguments).unwrap(), "a\nc\nc\n");
        assert!(
            apply(
                "a",
                json!({"edits":[
                    {"old_string":"a", "new_string":"b b"},
                    {"old_string":"b", "new_string":"c"}
                ]})
            )
            .unwrap_err()
            .contains("edit 2 failed")
        );
    }

    #[test]
    fn rejects_mixed_empty_and_malformed_arguments() {
        for field in ["old_string", "new_string", "offset", "limit", "replace_all"] {
            let mut arguments =
                json!({"path":"file.txt", "edits":[{"old_string":"a", "new_string":"b"}]});
            arguments[field] = serde_json::Value::Null;
            assert!(
                parse_arguments(&arguments.to_string())
                    .unwrap_err()
                    .contains("cannot be combined")
            );
        }
        for arguments in [
            json!({"path":"file.txt", "edits":[]}),
            json!({"path":"file.txt", "edits":null}),
            json!({"path":"file.txt", "edits":[{"old_string":"a"}]}),
            json!({"path":"file.txt", "old_string":"a"}),
            json!({"path":"file.txt", "old_string":"a", "new_string":"b", "replace_all":"true"}),
        ] {
            assert!(parse_arguments(&arguments.to_string()).is_err());
        }
    }

    #[tokio::test]
    async fn batch_failure_does_not_write_and_success_refreshes_read_tracking() {
        let root =
            std::env::temp_dir().join(format!("programmer-edit-batch-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("file.txt");
        let original = "a\r\na\r\n";
        tokio::fs::write(&path, original).await.unwrap();
        let security =
            crate::security::SecurityManager::new(Default::default(), root.clone()).unwrap();
        let read = json!({"path":path}).to_string();
        crate::tools::read_file::run_with_security_scope(&read, &security, 7)
            .await
            .unwrap();
        let mut arguments = json!({"path":path, "edits":[
            {"old_string":"a", "new_string":"b", "replace_all":true},
            {"old_string":"missing", "new_string":"c"}
        ]});
        let error = run_with_security_scope(&arguments.to_string(), &security, 7)
            .await
            .unwrap_err();
        assert!(error.contains("edit 2 failed"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), original);
        arguments["edits"][1] = json!({"old_string":"b", "new_string":"c", "replace_all":true});
        run_with_security_scope(&arguments.to_string(), &security, 7)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "c\nc\n");
        let follow_up = json!({"path":path,"old_string":"c", "new_string":"d", "replace_all":true});
        run_with_security_scope(&follow_up.to_string(), &security, 7)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "d\nd\n");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn schema_advertises_both_edit_modes() {
        let Tool::Function(tool) = tool() else {
            panic!("expected function tool")
        };
        let parameters = tool.parameters.unwrap();
        assert_eq!(parameters["required"], json!(["path"]));
        assert_eq!(parameters["properties"]["replace_all"]["default"], false);
        assert_eq!(parameters["properties"]["edits"]["minItems"], 1);
        assert_eq!(
            parameters["properties"]["edits"]["items"]["required"],
            json!(["old_string", "new_string"])
        );
    }
}
