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

//! Diagnostics pipeline: baseline seeding, snapshot collection, diffing, and
//! post-edit feedback delivery.

use super::App;
use super::helpers;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::event::{AppEvent, Event};
use crate::cancel::CancellationToken;

/// Deliver post-edit feedback (a diagnostics summary and/or a reminder).
pub(crate) fn emit_post_edit_feedback(app: &mut App<'_>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(call_id) = last_edit_output_call_id(app) {
        let block = format!("\n\n--- Post-edit check ---\n{text}");
        if app.conversation_panel.append_to_tool_output(&call_id, &block) {
            return;
        }
    }
    app.conversation_panel
        .add_meta("\u{25B8} System", text);
}

/// After a file-editing batch, run diagnostics (if configured) and count the
/// turn toward the periodic PROGRAMMER.md reminder. Returns `true` when it
/// spawned an async diagnostics run that will continue the turn itself via
/// [`AppEvent::DiagnosticsCompleted`]; `false` means the caller should
/// resume the stream normally.
pub(crate) fn continue_with_diagnostics(
    app: &mut App<'_>,
    edited_files: bool,
    cancel_token: CancellationToken,
) -> bool {
    if !edited_files {
        return false;
    }
    app.diag.mutating_turns += 1;
    let reminder_due = app.diag.mutating_turns.is_multiple_of(crate::consts::OVERVIEW_REMINDER_EVERY)
        && std::path::Path::new("PROGRAMMER.md").exists();

    if std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
        spawn_diagnostics(app, reminder_due, cancel_token);
        return true;
    }

    if reminder_due {
        emit_post_edit_feedback(app, helpers::overview_reminder());
        super::session::mark_dirty(app);
    }
    false
}

/// Forget the diagnostics baseline and edit counter.
pub(crate) fn reset_diagnostics_state(app: &mut App<'_>) {
    app.diag.baseline = None;
    app.diag.mutating_turns = 0;
}

/// Spawn the diagnostics checkers in the background.
pub(crate) fn spawn_diagnostics(
    app: &mut App<'_>,
    reminder_due: bool,
    cancel_token: CancellationToken,
) {
    app.conversation_panel.phase = ActivePhase::Checking;
    let sender = app.events.sender.clone();
    tokio::spawn(async move {
        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
        let snapshot = crate::diagnostics::collect(&cwd).await.unwrap_or_default();
        let _ = sender.send(Event::App(AppEvent::DiagnosticsCompleted {
            snapshot,
            reminder_due,
            seed: false,
            cancel_token,
        }));
    });
}

/// On the first turn of a session with a diagnostics profile, run the
/// checkers once in the background to establish a baseline.
pub(crate) fn maybe_seed_diagnostics_baseline(app: &mut App<'_>) {
    if app.diag.baseline.is_some() {
        return;
    }
    if !std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
        return;
    }
    let sender = app.events.sender.clone();
    tokio::spawn(async move {
        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
        let snapshot = crate::diagnostics::collect(&cwd).await.unwrap_or_default();
        let _ = sender.send(Event::App(AppEvent::DiagnosticsCompleted {
            snapshot,
            reminder_due: false,
            seed: true,
            cancel_token: CancellationToken::new(),
        }));
    });
}

/// Apply the diagnostics snapshot: diff against baseline, emit feedback, and
/// optionally include a PROGRAMMER.md reminder.
pub(crate) fn apply_diagnostics(
    app: &mut App<'_>,
    snapshot: crate::diagnostics::Snapshot,
    reminder_due: bool,
) {
    let mut parts: Vec<String> = Vec::new();

    match &app.diag.baseline {
        Some(old) => {
            let d = crate::diagnostics::diff(old, &snapshot.diagnostics);
            if let Some(summary) = d.summary() {
                parts.push(summary);
            }
        }
        None => {
            if !snapshot.diagnostics.is_empty() {
                parts.push(format!(
                    "Diagnostics baseline established: {} problem(s) \
                     currently in the project. Future edits will report \
                     changes relative to this.",
                    snapshot.diagnostics.len()
                ));
            }
        }
    }
    for e in &snapshot.errors {
        parts.push(format!("Diagnostics checker error: {e}"));
    }
    app.diag.baseline = Some(snapshot.diagnostics);

    if reminder_due {
        parts.push(helpers::overview_reminder());
    }

    emit_post_edit_feedback(app, parts.join("\n\n"));
}

// ---------------------------------------------------------------------------
// Helpers for locating editing tool output call_ids
// ---------------------------------------------------------------------------

use crate::response::message_item::MessageItem;
use async_openai::types::responses::OutputItem;

/// The call id of the most recent file-editing tool output.
fn last_edit_output_call_id(app: &App<'_>) -> Option<String> {
    let names: std::collections::HashMap<&str, &str> = app
        .conversation_panel
        .items()
        .filter_map(|it| match it {
            MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                Some((fc.call_id.as_str(), fc.name.as_str()))
            }
            _ => None,
        })
        .collect();
    app.conversation_panel
        .items()
        .filter_map(|it| match it {
            MessageItem::ToolOutput { output: fco, .. } => {
                let name = names.get(fco.call_id.as_str()).copied();
                matches!(
                    name,
                    Some(crate::tools::write_file::NAME) | Some(crate::tools::edit_file::NAME)
                )
                .then(|| fco.call_id.clone())
            }
            _ => None,
        })
        .last()
}
