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

//! RAII terminal setup / teardown guard.

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, supports_keyboard_enhancement,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cache the hash of the last title so we skip redundant OSC writes.
static LAST_TITLE_HASH: AtomicU64 = AtomicU64::new(0);

/// Write an OSC 0 terminal title sequence to stdout.  No-ops when `title` is
/// the same as the last call (fast hash compare — no allocation).
pub(crate) fn set_terminal_title(title: &str) {
    let mut hasher = DefaultHasher::new();
    title.hash(&mut hasher);
    let h = hasher.finish();
    if LAST_TITLE_HASH.swap(h, Ordering::Relaxed) == h {
        return; // same title, skip
    }
    use std::io::Write;
    print!("\x1b]0;{}\x07", title);
    let _ = io::stdout().flush();
}

/// Initialises the fullscreen TUI, returning the terminal handle and a guard
/// that restores the console on drop.
pub(crate) struct TerminalGuard {
    keyboard_enhanced: bool,
}

impl TerminalGuard {
    pub(crate) fn enter(
        project_name: &str,
    ) -> color_eyre::Result<(Self, Terminal<CrosstermBackend<io::Stdout>>)> {
        color_eyre::install()?;
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
        if keyboard_enhanced {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                )
            )?;
        }
        // Set initial title before the TUI starts rendering.
        set_terminal_title(&format!("\u{25cf} Ready {project_name} \u{b7} programmer"));
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok((Self { keyboard_enhanced }, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Reset the terminal title before restoring the screen.
        {
            use std::io::Write;
            print!("\x1b]0;\x07");
            let _ = io::stdout().flush();
        }
        if self.keyboard_enhanced {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        ratatui::restore();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        // Invalidate the title cache so a future run starts fresh.
        LAST_TITLE_HASH.store(0, Ordering::Relaxed);
    }
}
