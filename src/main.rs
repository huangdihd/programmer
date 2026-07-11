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

use crate::config::programmer_config::ProgrammerConfig;
use ::config::Config;
use ::config::Environment;
use ::config::File;
use app::App;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::Path;

pub mod app;
pub mod clipboard;
pub mod commands;
pub mod config;
pub mod providers;
pub mod response;
pub mod tools;
mod ui;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let config_dir = dirs::config_dir().unwrap();
    let programmer_dir = config_dir.join("programmer");
    if !Path::new(programmer_dir.as_path()).exists() {
        std::fs::create_dir(&programmer_dir)?;
    }
    let config_path = programmer_dir.join("config.toml");
    color_eyre::install()?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    // Without the kitty keyboard protocol, terminals send Ctrl+Enter and
    // Shift+Enter as a plain Enter, so modifier detection needs this.
    let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let programmer_config = Config::builder()
        .add_source(File::with_name(config_path.as_path().to_str().unwrap()).required(false))
        .add_source(Environment::with_prefix("Programmer"))
        .build()
        .unwrap_or_default();

    let mut programmer_config: ProgrammerConfig = programmer_config.try_deserialize()?;

    // Migrate v0.1.x config (single model/base_url/api_key) to v0.2.x
    // multi-provider format.
    if programmer_config.migrate_if_needed() {
        // Persist the migrated config back to disk so the old format is
        // never seen again.
        std::fs::write(&config_path, toml::to_string(&programmer_config)?)?;
    }

    if !Path::new(config_path.as_path()).exists() {
        std::fs::write(config_path, toml::to_string(&programmer_config)?)?;
    }

    let result = App::new(programmer_config).await.run(terminal).await;
    // Pop while still on the alternate screen: kitty keeps separate flag
    // stacks per screen buffer.
    if keyboard_enhanced {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    result
}
