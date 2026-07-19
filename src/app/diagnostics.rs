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

//! Diagnostics: baseline seeding and state reset. The engine now owns the
//! post-edit feedback loop; these helpers only manage the shared state that
//! both the engine and the sidebar read.

use super::App;

/// On the first turn of a session with a diagnostics profile, run the
/// checkers once in the background to establish a baseline in the shared
/// [`crate::engine::DiagnosticsState`] (accessible to both the engine and the UI).
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
        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
        let snapshot = crate::diagnostics::collect(&cwd).await.unwrap_or_default();
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
