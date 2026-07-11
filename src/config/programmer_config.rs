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

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ProgrammerConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
}

impl Default for ProgrammerConfig {
    fn default() -> Self {
        ProgrammerConfig {
            model: "Type your model here".to_string(),
            base_url: "Type your base_url here".to_string(),
            api_key: "Type your api_key here".to_string(),
        }
    }
}

impl ProgrammerConfig {
    pub fn new(model: &str, base_url: &str, api_key: &str) -> Self {
        ProgrammerConfig {
            model: model.to_string(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
    }
}
