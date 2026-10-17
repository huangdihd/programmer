// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

use serde::{Deserialize, Serialize};

/// Durable front-end envelope. The runner owns the exact batch continuation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub id: String,
    pub kind: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<crate::runner::RunnerCheckpoint>,
    pub session_id: String,
    pub resume_command: String,
    #[serde(default)]
    pub options: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub details: serde_json::Map<String, serde_json::Value>,
}

impl Pending {
    pub fn plan(text: String, session_id: String) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            resume_command: resume_command(&session_id, &id),
            id,
            kind: "plan_review".into(),
            prompt: text,
            checkpoint: None,
            session_id,
            options: ["manual", "auto", "yolo", "revise:<feedback>"]
                .into_iter()
                .map(|answer| serde_json::json!({"answer": answer}))
                .collect(),
            details: Default::default(),
        }
    }
}

impl Pending {
    pub fn tool(checkpoint: crate::runner::RunnerCheckpoint, session_id: String) -> Self {
        let p = &checkpoint.pending;
        let kind = match p.kind {
            crate::runner::PendingKind::Review => "tool_approval",
            crate::runner::PendingKind::Question => "question",
            crate::runner::PendingKind::Permission => "permission",
        };
        let mut result = Self::plan(p.prompt.clone(), session_id);
        result.id = p.id.clone();
        result.kind = kind.into();
        result.resume_command = resume_command(&result.session_id, &result.id);
        result.options = match p.kind {
            crate::runner::PendingKind::Review => vec![
                serde_json::json!({"answer":"approve"}),
                serde_json::json!({"answer":"deny"}),
            ],
            crate::runner::PendingKind::Permission => vec![
                serde_json::json!({"answer":"allow"}),
                serde_json::json!({"answer":"deny"}),
            ],
            crate::runner::PendingKind::Question => p
                .options
                .iter()
                .enumerate()
                .map(|(i, label)| serde_json::json!({"label":label,"answer":(i+1).to_string()}))
                .collect(),
        };
        result
            .details
            .insert("tool_name".into(), serde_json::json!(p.call.name));
        result.details.insert(
            "arguments".into(),
            serde_json::from_str(&p.call.arguments)
                .unwrap_or_else(|_| serde_json::json!(p.call.arguments)),
        );
        result
            .details
            .insert("reason".into(), serde_json::json!(p.reason));
        result.checkpoint = Some(checkpoint);
        result
    }
}

/// A copy-paste continuation that pins the workspace: answering re-runs the
/// pending call with its original, possibly relative, arguments.
fn resume_command(session_id: &str, id: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    format!(
        "programmer run --cwd {} --resume {session_id} --answer-id {} --answer '<ANSWER>'",
        shell_quote(&cwd.to_string_lossy()),
        shell_quote(id)
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_command_pins_the_quoted_workspace() {
        let pending = Pending::plan("Review".into(), "sid".into());
        let cwd = std::env::current_dir().unwrap();
        assert!(pending.resume_command.starts_with(&format!(
            "programmer run --cwd {} --resume sid --answer-id '{}'",
            shell_quote(&cwd.to_string_lossy()),
            pending.id
        )));
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
