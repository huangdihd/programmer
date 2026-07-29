// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::ask_user::{Question, QuestionKind};
use super::function_tool;
use crate::cancel::CancellationToken;
use crate::security::{AccessKind, SandboxMode, SecurityHandle};
use crate::ui::event::Event;
use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

pub const NAME: &str = "request_permission";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Ask the user for a session-only security permission when necessary work is \
         blocked. Request either a sandbox mode change or exact filesystem access. \
         Filesystem permission applies only to the requested operation and normalized \
         path; retry the blocked tool call after approval. No change is applied without \
         explicit user approval, and approvals are not persisted across application \
         restarts.",
        json!({
            "kind": {
                "type": "string",
                "enum": ["sandbox", "filesystem"],
                "description": "The kind of permission to request."
            },
            "mode": {
                "type": ["string", "null"],
                "enum": ["restricted", "network", "off", null],
                "description": "Sandbox requests: restricted (network denied), network (network allowed), or off (no process sandbox). Use null for filesystem requests."
            },
            "operation": {
                "type": ["string", "null"],
                "enum": ["read", "write", "execute", null],
                "description": "Filesystem requests: the exact operation to allow. Use null for sandbox requests."
            },
            "path": {
                "type": ["string", "null"],
                "minLength": 1,
                "description": "Filesystem requests: the exact path to allow. Relative paths are resolved against the project directory. Use null for sandbox requests."
            },
            "reason": {
                "type": "string",
                "minLength": 1,
                "description": "A concise explanation of why this permission is needed."
            }
        }),
        &["kind", "mode", "operation", "path", "reason"],
    )
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Args {
    Sandbox {
        mode: String,
        reason: String,
    },
    Filesystem {
        operation: AccessKind,
        path: String,
        reason: String,
    },
}

struct RequestContext<'a> {
    sender: &'a mpsc::UnboundedSender<Event>,
    cancel: &'a CancellationToken,
    operation_id: u64,
    security: &'a SecurityHandle,
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
    let context = RequestContext {
        sender,
        cancel,
        operation_id,
        security,
    };

    match args {
        Args::Sandbox { mode, reason } => request_sandbox(&mode, &reason, &context).await,
        Args::Filesystem {
            operation,
            path,
            reason,
        } => request_filesystem(operation, &path, &reason, &context).await,
    }
}

async fn request_sandbox(
    mode: &str,
    reason: &str,
    context: &RequestContext<'_>,
) -> Result<String, String> {
    let mode =
        SandboxMode::parse(mode).ok_or_else(|| format!("error: unknown sandbox mode '{mode}'"))?;
    let reason = required_reason(reason)?;
    let current = context.security.sandbox_mode();
    if current == mode {
        return Ok(format!("sandbox mode is already {}", mode.label()));
    }

    let approved = prompt_approval(
        format!(
            "The agent requests sandbox mode '{}' (currently '{}').\nReason: {reason}\n\
             This change lasts for this application session only.",
            mode.label(),
            current.label()
        ),
        context,
    )
    .await?;

    if !approved {
        return Ok(format!(
            "permission denied; sandbox remains {}",
            context.security.sandbox_mode().label()
        ));
    }

    context.security.set_sandbox_mode(mode)?;
    Ok(format!(
        "permission granted for this session; sandbox mode is now {}",
        mode.label()
    ))
}

async fn request_filesystem(
    operation: AccessKind,
    path: &str,
    reason: &str,
    context: &RequestContext<'_>,
) -> Result<String, String> {
    if operation == AccessKind::Network {
        return Err("error: filesystem permission does not support network access".to_string());
    }
    let path = path.trim();
    if path.is_empty() {
        return Err("error: filesystem permission requires a non-empty path".to_string());
    }
    let reason = required_reason(reason)?;
    let current = context.security.snapshot();
    let resolved = current.resolve_path(path)?;
    if current.authorize_path(operation, &resolved).is_ok() {
        return Ok(format!(
            "{} access is already allowed for {}",
            operation.label(),
            resolved.display()
        ));
    }

    let approved = prompt_approval(
        format!(
            "The agent requests {} access to:\n{}\nReason: {reason}\n\
             This exact-path permission lasts for this application session only.",
            operation.label(),
            resolved.display()
        ),
        context,
    )
    .await?;
    if !approved {
        return Ok(format!(
            "permission denied; {} access remains blocked for {}",
            operation.label(),
            resolved.display()
        ));
    }

    let granted = context.security.grant_path(operation, &resolved)?;
    Ok(format!(
        "permission granted for this session; {} access is now allowed for {}",
        operation.label(),
        granted.display()
    ))
}

async fn prompt_approval(text: String, context: &RequestContext<'_>) -> Result<bool, String> {
    let approve = "Approve for this session".to_string();
    let answer = super::ask_user::prompt(
        Question {
            text,
            kind: QuestionKind::Choice {
                options: vec![
                    approve.clone(),
                    "Deny".to_string(),
                    "Other\u{2026}".to_string(),
                ],
                other_index: 2,
            },
        },
        context.sender,
        context.cancel,
        context.operation_id,
    )
    .await?;
    Ok(answer == approve)
}

fn required_reason(reason: &str) -> Result<&str, String> {
    let reason = reason.trim();
    if reason.is_empty() {
        Err("error: permission reason must not be empty".to_string())
    } else {
        Ok(reason)
    }
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
        let args = r#"{"kind":"sandbox","mode":"network","operation":null,"path":null,"reason":"download dependencies"}"#;

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

    #[tokio::test]
    async fn filesystem_permission_grants_exact_path_after_approval() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let external =
            std::env::temp_dir().join(format!("programmer-external-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let path = external.join("allowed.txt");
        let manager =
            SecurityManager::new(crate::security::SecurityConfig::default(), root.clone())
                .expect("security manager");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));
        assert!(
            security
                .snapshot()
                .authorize_path(AccessKind::Write, &path)
                .is_err()
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let args = serde_json::json!({
            "kind": "filesystem",
            "mode": null,
            "operation": "write",
            "path": path,
            "reason": "update the requested file"
        })
        .to_string();
        let task = tokio::spawn({
            let tx = tx.clone();
            let cancel = cancel.clone();
            let security = security.clone();
            async move { run(&args, &tx, &cancel, 9, &security).await }
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
        assert!(
            security
                .snapshot()
                .authorize_path(AccessKind::Write, &path)
                .is_ok()
        );

        security.set_sandbox_mode(SandboxMode::Network).unwrap();
        assert!(
            security
                .snapshot()
                .authorize_path(AccessKind::Write, &path)
                .is_ok()
        );

        drop(security);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(external).unwrap();
    }

    #[tokio::test]
    async fn denied_filesystem_permission_does_not_change_policy() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let external =
            std::env::temp_dir().join(format!("programmer-external-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let path = external.join("blocked.txt");
        let manager =
            SecurityManager::new(crate::security::SecurityConfig::default(), root.clone())
                .expect("security manager");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let args = serde_json::json!({
            "kind": "filesystem",
            "mode": null,
            "operation": "write",
            "path": path,
            "reason": "update the requested file"
        })
        .to_string();
        let task = tokio::spawn({
            let tx = tx.clone();
            let cancel = cancel.clone();
            let security = security.clone();
            async move { run(&args, &tx, &cancel, 10, &security).await }
        });

        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        let Event::App(crate::ui::event::AppEvent::QuestionPrompt { answer_tx, .. }) = event else {
            panic!("expected a question prompt");
        };
        answer_tx.send("Deny".to_string());

        let output = task
            .await
            .expect("permission task")
            .expect("permission result");
        assert!(output.contains("permission denied"));
        assert!(
            security
                .snapshot()
                .authorize_path(AccessKind::Write, &path)
                .is_err()
        );

        drop(security);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(external).unwrap();
    }
}
