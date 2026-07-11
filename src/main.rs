// Copyright (C) 2025 huangdihd
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

use std::io;
use std::path::Path;
use ::config::Config;
use ::config::File;
use ::config::Environment;
use crossterm::{execute};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use app::App;
use crate::config::programmer_config::ProgrammerConfig;

mod ui;
pub mod config;
pub mod app;
pub mod response;
pub mod tools;

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
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;


    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let programmer_config = Config::builder()
        .add_source(File::with_name(config_path.as_path().to_str().unwrap()).required(false))
        .add_source(Environment::with_prefix("Programmer"))
        .build()
        .unwrap_or_default();

    let programmer_config: ProgrammerConfig  = programmer_config.try_deserialize()?;

    if !Path::new(config_path.as_path()).exists() {
        std::fs::write(config_path, toml::to_string(&programmer_config)?)?;
    }

    let result = App::new(programmer_config).await.run(terminal).await;
    ratatui::restore();
    execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}
