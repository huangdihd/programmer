// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::ask_user::{Question, QuestionHandler, QuestionKind};
use super::function_tool;
use crate::cancel::{CancellationToken, OperationId};
use crate::security::{AccessKind, SandboxMode, SecurityHandle};
use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

pub const NAME: &str = "request_permission";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Ask the user for a session-only security permission when necessary work is \
         blocked. Request either a sandbox mode change or filesystem access. Filesystem \
         permission applies only to the requested operation and normalized path. An \
         existing directory grant covers that directory and its descendants, including \
         paths denied by configuration; a file grant covers only that exact file. \
         Sandboxed child-process writes need overlapping recursive read and write grants. \
         Filesystem grants do not constrain commands when the process sandbox is off. \
         Retry the blocked tool call after approval. No change is applied without explicit \
         user approval, and approvals are not persisted across application restarts.",
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
                "enum": ["read", "write", null],
                "description": "Filesystem requests: the exact operation to allow. Use null for sandbox requests."
            },
            "path": {
                "type": ["string", "null"],
                "minLength": 1,
                "description": "Filesystem requests: the path to allow. An existing directory covers itself and descendants; a file covers only itself. Relative paths are resolved against the project directory. Use null for sandbox requests."
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
    questions: &'a QuestionHandler,
    cancel: &'a CancellationToken,
    operation_id: OperationId,
    security: &'a SecurityHandle,
}

pub async fn run(
    arguments: &str,
    questions: &QuestionHandler,
    cancel: &CancellationToken,
    operation_id: OperationId,
    security: &SecurityHandle,
) -> Result<String, String> {
    let args: Args = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    let context = RequestContext {
        questions,
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
    if !current.can_request(mode) {
        return Err(format!(
            "error: sandbox mode request rejected: '{}' is stricter than the current '{}' mode; permission requests may only relax sandbox restrictions",
            mode.label(),
            current.label()
        ));
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

    let active = context.security.relax_sandbox_mode(mode)?;
    if active != mode {
        return Ok(format!(
            "permission approval was no longer needed; sandbox remains {}",
            active.label()
        ));
    }
    Ok(format!(
        "permission granted for this session; sandbox mode is now {}",
        active.label()
    ))
}

async fn request_filesystem(
    operation: AccessKind,
    path: &str,
    reason: &str,
    context: &RequestContext<'_>,
) -> Result<String, String> {
    if !matches!(operation, AccessKind::Read | AccessKind::Write) {
        return Err("error: filesystem permission supports only read or write access".to_string());
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

    let recursive = resolved.is_dir();
    let permission_scope = if recursive {
        "This permission covers this directory and all of its descendants, overriding matching configured denies"
    } else {
        "This permission covers this file only, overriding a matching configured deny"
    };
    let approved = prompt_approval(
        format!(
            "The agent requests {} access to:\n{}\nReason: {reason}\n\
             {permission_scope} and lasts for this application session only.",
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

    let granted = context
        .security
        .grant_path_if_unchanged(operation, path, &resolved, recursive)?;
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
        context.questions,
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

    #[test]
    fn sandbox_permission_only_allows_relaxing_modes() {
        assert!(!SandboxMode::Off.can_request(SandboxMode::Network));
        assert!(!SandboxMode::Network.can_request(SandboxMode::Restricted));
        assert!(SandboxMode::Restricted.can_request(SandboxMode::Network));
        assert!(SandboxMode::Network.can_request(SandboxMode::Off));
    }

    #[tokio::test]
    async fn cancelled_permission_request_never_changes_policy() {
        let manager = SecurityManager::standalone().expect("standalone security");
        let security = SecurityHandle::new(Arc::new(manager));
        security.set_sandbox_mode(SandboxMode::Restricted).unwrap();
        let (questions, mut receiver) = super::super::ask_user::question_channel();
        let cancel = CancellationToken::new();
        let args = r#"{"kind":"sandbox","mode":"network","operation":null,"path":null,"reason":"download dependencies"}"#;
        let request = run(args, &questions, &cancel, OperationId(17), &security);
        tokio::pin!(request);
        assert!(futures::poll!(&mut request).is_pending());
        let prompt = receiver.try_recv().expect("permission question");
        assert_eq!(prompt.operation_id, OperationId(17));
        cancel.cancel();
        let output = request.await.unwrap();
        assert!(output.contains("permission denied"));
        assert_eq!(security.sandbox_mode(), SandboxMode::Restricted);
        assert!(
            prompt
                .answer
                .send("Approve for this session".to_string())
                .is_err()
        );
    }

    #[tokio::test]
    async fn stricter_sandbox_request_is_rejected_without_prompt() {
        let manager = SecurityManager::standalone().expect("standalone security");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));
        security.set_sandbox_mode(SandboxMode::Network).unwrap();
        let (tx, mut rx) = super::super::ask_user::question_channel();
        let cancel = CancellationToken::new();
        let args = r#"{"kind":"sandbox","mode":"restricted","operation":null,"path":null,"reason":"tighten security"}"#;
        let output = run(args, &tx, &cancel, OperationId(7), &security).await;
        assert!(
            output
                .expect_err("stricter mode must be rejected")
                .contains("stricter than the current")
        );
        assert!(rx.try_recv().is_err(), "must not prompt for approval");
    }

    #[tokio::test]
    async fn an_older_sandbox_approval_cannot_tighten_a_newer_mode() {
        let manager = SecurityManager::standalone().expect("standalone security");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));
        security.set_sandbox_mode(SandboxMode::Restricted).unwrap();
        let (questions, mut receiver) = super::super::ask_user::question_channel();
        let cancel = CancellationToken::new();
        let arguments = r#"{"kind":"sandbox","mode":"network","operation":null,"path":null,"reason":"download dependencies"}"#;
        let request = tokio::spawn({
            let questions = questions.clone();
            let cancel = cancel.clone();
            let security = security.clone();
            async move { run(arguments, &questions, &cancel, OperationId(18), &security).await }
        });

        let prompt = tokio::time::timeout(Duration::from_millis(200), receiver.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        security.set_sandbox_mode(SandboxMode::Off).unwrap();
        prompt
            .answer
            .send("Approve for this session".to_string())
            .unwrap();

        let output = request
            .await
            .expect("permission task")
            .expect("permission result");
        assert!(output.contains("sandbox remains off"));
        assert_eq!(security.sandbox_mode(), SandboxMode::Off);
    }

    #[tokio::test]
    async fn permission_change_requires_an_explicit_approval() {
        let manager = SecurityManager::standalone().expect("standalone security");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));
        security.set_sandbox_mode(SandboxMode::Restricted).unwrap();
        let (tx, mut rx) = super::super::ask_user::question_channel();
        let cancel = CancellationToken::new();
        let args = r#"{"kind":"sandbox","mode":"network","operation":null,"path":null,"reason":"download dependencies"}"#;

        let task = tokio::spawn({
            let tx = tx.clone();
            let cancel = cancel.clone();
            let security = security.clone();
            async move { run(args, &tx, &cancel, OperationId(7), &security).await }
        });

        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        let answer_tx = event.answer;
        answer_tx
            .send("Approve for this session".to_string())
            .unwrap();

        let output = task
            .await
            .expect("permission task")
            .expect("permission result");
        assert!(output.contains("permission granted"));
        assert_eq!(security.sandbox_mode(), SandboxMode::Network);
    }

    #[tokio::test]
    async fn filesystem_permission_rejects_unrepresentable_execute_access() {
        let manager = SecurityManager::standalone().expect("standalone security");
        let security = SecurityHandle::new(Arc::new(manager));
        let (questions, mut receiver) = super::super::ask_user::question_channel();
        let cancel = CancellationToken::new();
        let arguments = r#"{"kind":"filesystem","mode":null,"operation":"execute","path":"bin/tool","reason":"run it"}"#;

        let error = run(arguments, &questions, &cancel, OperationId(8), &security)
            .await
            .expect_err("execute permission must fail closed");

        assert!(error.contains("supports only read or write"));
        assert!(receiver.try_recv().is_err(), "must not prompt for approval");
    }

    #[tokio::test]
    async fn filesystem_permission_grants_exact_path_with_sandbox_off_after_approval() {
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
        security.set_sandbox_mode(SandboxMode::Off).unwrap();
        assert!(
            security
                .snapshot()
                .authorize_path(AccessKind::Write, &path)
                .is_err()
        );

        let (tx, mut rx) = super::super::ask_user::question_channel();
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
            async move { run(&args, &tx, &cancel, OperationId(9), &security).await }
        });

        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        assert!(event.question.text.contains("covers this file only"));
        let answer_tx = event.answer;
        answer_tx
            .send("Approve for this session".to_string())
            .unwrap();

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

        assert!(
            security
                .snapshot()
                .authorize_path(AccessKind::Write, external.join("other.txt"))
                .is_err()
        );
        assert_eq!(security.sandbox_mode(), SandboxMode::Off);
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
    async fn filesystem_directory_permission_discloses_and_grants_descendant_access() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let external =
            std::env::temp_dir().join(format!("programmer-external-{}", uuid::Uuid::new_v4()));
        let granted_directory = external.join("granted");
        let nested_file = granted_directory.join("nested/file.txt");
        let sibling_file = external.join("sibling/file.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(nested_file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(sibling_file.parent().unwrap()).unwrap();
        let manager =
            SecurityManager::new(crate::security::SecurityConfig::default(), root.clone())
                .expect("security manager");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));

        let (questions, mut receiver) = super::super::ask_user::question_channel();
        let cancel = CancellationToken::new();
        let arguments = serde_json::json!({
            "kind": "filesystem",
            "mode": null,
            "operation": "write",
            "path": granted_directory,
            "reason": "update generated files"
        })
        .to_string();
        let task = tokio::spawn({
            let questions = questions.clone();
            let cancel = cancel.clone();
            let security = security.clone();
            async move { run(&arguments, &questions, &cancel, OperationId(11), &security).await }
        });

        let event = tokio::time::timeout(Duration::from_millis(200), receiver.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        assert!(
            event
                .question
                .text
                .contains("covers this directory and all of its descendants")
        );
        event
            .answer
            .send("Approve for this session".to_string())
            .unwrap();
        assert!(
            task.await
                .expect("permission task")
                .expect("permission result")
                .contains("permission granted")
        );

        let policy = security.snapshot();
        assert!(
            policy
                .authorize_path(AccessKind::Write, nested_file)
                .is_ok()
        );
        assert!(
            policy
                .authorize_path(AccessKind::Write, sibling_file)
                .is_err()
        );

        drop(policy);
        drop(security);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(external).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_permission_rejects_a_target_redirected_during_approval() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let external =
            std::env::temp_dir().join(format!("programmer-external-{}", uuid::Uuid::new_v4()));
        let parent = external.join("parent");
        let original_parent = external.join("original-parent");
        let requested = parent.join("granted");
        let redirected = external.join("redirected");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&requested).unwrap();
        std::fs::create_dir_all(redirected.join("granted")).unwrap();
        let manager =
            SecurityManager::new(crate::security::SecurityConfig::default(), root.clone())
                .expect("security manager");
        let security = Arc::new(SecurityHandle::new(Arc::new(manager)));
        let (questions, mut receiver) = super::super::ask_user::question_channel();
        let cancel = CancellationToken::new();
        let arguments = serde_json::json!({
            "kind": "filesystem",
            "mode": null,
            "operation": "write",
            "path": requested,
            "reason": "update generated files"
        })
        .to_string();
        let task = tokio::spawn({
            let questions = questions.clone();
            let cancel = cancel.clone();
            let security = security.clone();
            async move { run(&arguments, &questions, &cancel, OperationId(12), &security).await }
        });

        let event = tokio::time::timeout(Duration::from_millis(200), receiver.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        std::fs::rename(&parent, &original_parent).unwrap();
        std::os::unix::fs::symlink(&redirected, &parent).unwrap();
        event
            .answer
            .send("Approve for this session".to_string())
            .unwrap();

        let error = task
            .await
            .expect("permission task")
            .expect_err("redirected target must be rejected");
        assert!(error.contains("target changed while awaiting approval"));
        assert_eq!(security.snapshot().session_grant_count(), 0);

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
        security.set_sandbox_mode(SandboxMode::Off).unwrap();

        let (tx, mut rx) = super::super::ask_user::question_channel();
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
            async move { run(&args, &tx, &cancel, OperationId(10), &security).await }
        });

        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("permission prompt")
            .expect("event channel");
        let answer_tx = event.answer;
        answer_tx.send("Deny".to_string()).unwrap();

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
