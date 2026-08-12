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

//! Diagnostics: baseline seeding and state reset. The runner now owns the
//! post-edit feedback loop; these helpers only manage the shared state that
//! both the runner and the sidebar read.

use super::App;

/// Persist the profile edited by `/diagnostics manage`. An empty profile means
/// diagnostics are disabled and removes the project file entirely.
pub(crate) fn save_profile(profile: &crate::diagnostics::DiagnosticsProfile) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let path = crate::diagnostics::DiagnosticsProfile::path_in(&cwd);
    if profile.checkers.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove {}: {error}", path.display()))?;
        }
        return Ok(());
    }
    profile.validate()?;
    let text = profile.to_toml()?;
    let parent = path
        .parent()
        .ok_or_else(|| "diagnostics profile has no parent directory".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    std::fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

/// Re-run the configured checkers without blocking the TUI and update the
/// shared sidebar snapshot when the matching result arrives.
pub(crate) fn start_update(app: &mut App<'_>, notify_started: bool) {
    app.diagnostics_update_generation = app.diagnostics_update_generation.wrapping_add(1);
    let generation = app.diagnostics_update_generation;
    if notify_started {
        app.conversation_panel
            .add_info_string("Updating diagnostics in the background…");
    }
    let sender = app.events.sender.clone();
    tokio::spawn(async move {
        let cwd =
            std::env::current_dir().unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
        let snapshot =
            crate::diagnostics::collect(&cwd, &crate::cancel::CancellationToken::new()).await;
        let _ = sender.send(crate::ui::event::Event::App(
            crate::ui::event::AppEvent::DiagnosticsUpdated {
                generation,
                snapshot,
            },
        ));
    });
}

/// On the first turn of a session with a diagnostics profile, run the
/// checkers once in the background to establish a baseline in the shared
/// [`crate::runner::DiagnosticsState`] (accessible to both the runner and the UI).
pub(crate) fn maybe_seed_diagnostics_baseline(app: &mut App<'_>) {
    {
        let state = app.diagnostics_state.lock().unwrap();
        if state.baseline.is_some() {
            return;
        }
    }
    if !std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
        return;
    }
    let state = app.diagnostics_state.clone();
    tokio::spawn(async move {
        let cwd =
            std::env::current_dir().unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
        let snapshot = crate::diagnostics::collect(&cwd, &crate::cancel::CancellationToken::new())
            .await
            .unwrap_or_default();
        let mut state = state.lock().unwrap();
        if state.baseline.is_none() {
            state.baseline = Some(snapshot.diagnostics);
        }
    });
}

/// Forget the diagnostics baseline and edit counter in the shared state.
pub(crate) fn reset_diagnostics_state(app: &mut App<'_>) {
    let mut state = app.diagnostics_state.lock().unwrap();
    state.baseline = None;
    state.mutating_turns = 0;
}
