// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! On-demand loading for enabled agent skills.

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;

pub const NAME: &str = "load_skill";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Load the complete instructions for one enabled skill. Call this before acting when the user's request clearly matches a skill listed in the available-skills catalog.",
        json!({
            "name": {
                "type": "string",
                "description": "Exact skill name from the available-skills catalog."
            }
        }),
        &["name"],
    )
}

#[derive(Deserialize)]
struct Args {
    name: String,
}

pub fn run(arguments: &str, registry: &crate::skills::SkillRegistry) -> Result<String, String> {
    let args: Args = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    registry
        .instructions(&args.name)
        .ok_or_else(|| format!("error: skill '{}' is not enabled or installed", args.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_enabled_skill_and_rejects_unknown_skill() {
        let registry = crate::skills::SkillRegistry::load();

        let text = run(r#"{"name":"programmer-guide"}"#, &registry).unwrap();
        assert!(text.contains("## Skill: programmer-guide"));
        assert!(run(r#"{"name":"missing"}"#, &registry).is_err());
    }
}
