pub mod command;
pub mod read_file;
pub mod write_file;
pub mod edit_file;

use async_openai::types::responses::{
    FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall, Tool,
};

/// The host shell used by the `command` tool: `(program, flag)`.
pub fn shell() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    }
}

/// A short description of the runtime environment, appended to the system prompt
/// so the model knows which OS/shell/working directory it is operating in.
pub fn environment_info() -> String {
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let (program, _) = shell();

    format!(
        "# Environment info\n\
         - Operating system: {os} ({arch})\n\
         - Shell for the `command` tool: {shell}\n\
         - Working directory: {cwd}",
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        shell = program,
        cwd = cwd,
    )
}

/// Upper bound on the text handed back to the model for a single tool call, so a
/// huge file or noisy command can't blow up the context window.
const MAX_OUTPUT_CHARS: usize = 30_000;

/// The full set of tool definitions advertised to the model on every request.
pub fn tools() -> Vec<Tool> {
    vec![
        command::tool(),
        read_file::tool(),
        write_file::tool(),
        edit_file::tool(),
    ]
}

/// Executes a single tool call and wraps the result as a `function_call_output`
/// item ready to be sent back to the model.
pub async fn run_tool_call(call: &FunctionToolCall) -> FunctionCallOutputItemParam {
    let output = match call.name.as_str() {
        command::NAME => command::run(&call.arguments).await,
        read_file::NAME => read_file::run(&call.arguments).await,
        write_file::NAME => write_file::run(&call.arguments).await,
        edit_file::NAME => edit_file::run(&call.arguments).await,
        other => format!("error: unknown tool '{other}'"),
    };

    FunctionCallOutputItemParam {
        call_id: call.call_id.clone(),
        output: FunctionCallOutput::Text(truncate(output)),
        id: None,
        status: None,
    }
}

/// Truncates on a char boundary so we never split a UTF-8 sequence.
fn truncate(text: String) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text;
    }
    let mut out: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    out.push_str("\n[output truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_runs_and_captures_output() {
        let out = command::run(r#"{"command":"echo hello"}"#).await;
        assert!(out.contains("hello"), "unexpected output: {out}");
    }

    #[tokio::test]
    async fn write_read_edit_round_trip() {
        let dir = std::env::temp_dir().join(format!("programmer_tools_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("scratch.txt");
        // Escape backslashes so the path is valid inside a JSON string (Windows).
        let json_path = path.to_string_lossy().replace('\\', "\\\\");

        let wrote = write_file::run(&format!(
            r#"{{"path":"{json_path}","content":"alpha\nbeta\n"}}"#
        ))
        .await;
        assert!(wrote.starts_with("wrote"), "unexpected: {wrote}");

        let read = read_file::run(&format!(r#"{{"path":"{json_path}"}}"#)).await;
        assert_eq!(read, "alpha\nbeta\n");

        let edited = edit_file::run(&format!(
            r#"{{"path":"{json_path}","old_string":"alpha","new_string":"gamma"}}"#
        ))
        .await;
        assert_eq!(edited, format!("edited {}", path.to_string_lossy()));

        let read_again = read_file::run(&format!(r#"{{"path":"{json_path}"}}"#)).await;
        assert_eq!(read_again, "gamma\nbeta\n");

        let missing = edit_file::run(&format!(
            r#"{{"path":"{json_path}","old_string":"nope","new_string":"x"}}"#
        ))
        .await;
        assert!(missing.starts_with("error: old_string not found"), "got: {missing}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A `Tool::Function` definition with a strict JSON-schema object for parameters.
/// `required` should list every property name for strict mode to validate.
fn function_tool(
    name: &str,
    description: &str,
    properties: serde_json::Value,
    required: &[&str],
) -> Tool {
    use async_openai::types::responses::FunctionTool;
    use serde_json::json;

    Tool::Function(FunctionTool {
        name: name.to_string(),
        description: Some(description.to_string()),
        parameters: Some(json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })),
        strict: Some(true),
        defer_loading: None,
    })
}
