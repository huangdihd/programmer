// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::ask_user::{Question, QuestionKind};
use super::function_tool;
use crate::cancel::CancellationToken;
use crate::security::{SandboxMode, SecurityHandle};
use crate::ui::event::Event;
use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

pub const NAME: &str = "request_permission";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Ask the user to change the sandbox mode for this session. Use this only \
         when the current sandbox blocks necessary work. The requested change is \
         never applied without explicit user approval and is not persisted across \
         application restarts.",
        json!({
            "mode": {
                "type": "string",
                "enum": ["restricted", "network", "off"],
                "description": "Requested sandbox mode: restricted (sandboxed with network denied), network (sandboxed with network allowed), or off (no process sandbox)."
            },
            "reason": {
                "type": "string",
                "minLength": 1,
                "description": "A concise explanation of why this permission is needed."
            }
        }),
        &["mode", "reason"],
    )
}

#[derive(Deserialize)]
struct Args {
    mode: String,
    reason: String,
}

pub async fn run(
    arguments: &str,
    sender: &mpsc::UnboundedSender<Event>,
    cancel: &CancellationToken,
    operation_id: u64,
    security: &SecurityHandle,
) -> Result<String, String> {
    let args: Args = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    let mode = SandboxMode::parse(&args.mode)
        .ok_or_else(|| format!("error: unknown sandbox mode '{}'", args.mode))?;
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err("error: permission reason must not be empty".to_string());
    }

    let current = security.sandbox_mode();
    if current == mode {
        return Ok(format!("sandbox mode is already {}", mode.label()));
    }

    let approve = "Approve for this session".to_string();
    let answer = super::ask_user::prompt(
        Question {
            text: format!(
                "The agent requests sandbox mode '{}' (currently '{}').\nReason: {reason}\n\
                 This change lasts for this application session only.",
                mode.label(),
                current.label()
            ),
            kind: QuestionKind::Choice {
                options: vec![
                    approve.clone(),
                    "Deny".to_string(),
                    "Other\u{2026}".to_string(),
                ],
                other_index: 2,
            },
        },
        sender,
        cancel,
        operation_id,
    )
    .await?;

    if answer != approve {
        return Ok(format!(
            "permission denied; sandbox remains {}",
            security.sandbox_mode().label()
        ));
    }

    security.set_sandbox_mode(mode)?;
    Ok(format!(
        "permission granted for this session; sandbox mode is now {}",
        mode.label()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityManager;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn permission_change_requires_an_explicit_approval() {
        let manager = SecurityManager::standalone().expect("standalone security");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let args = r#"{"mode":"network","reason":"download dependencies"}"#;

        let task = tokio::spawn({
            let tx = tx.clone();
            let cancel = cancel.clone();
            let security = security.clone();
            async move { run(args, &tx, &cancel, 7, &security).await }
        });

        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        let Event::App(crate::ui::event::AppEvent::QuestionPrompt { answer_tx, .. }) = event else {
            panic!("expected a question prompt");
        };
        answer_tx.send("Approve for this session".to_string());

        let output = task
            .await
            .expect("permission task")
            .expect("permission result");
        assert!(output.contains("permission granted"));
        assert_eq!(security.sandbox_mode(), SandboxMode::Network);
    }
}
