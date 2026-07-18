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

//! The UI-free agent engine: the turn primitives (request building, streaming,
//! classification, tool execution) that both the TUI event loop and a headless
//! driver share, plus the driver itself. Extracted incrementally from the
//! `app` event handlers so the TUI keeps working unchanged while the same logic
//! becomes reusable for a print mode and, later, in-process sub-agents.

pub(crate) mod request;
pub(crate) mod stream;
