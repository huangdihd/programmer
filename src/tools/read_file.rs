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
