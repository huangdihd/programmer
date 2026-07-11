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

pub mod ui;

/// The completion popup widget.
pub struct CompletionPopup<'a> {
    /// Display strings for each candidate.
    pub candidates: &'a [String],
    /// Currently selected index.
    pub selected: usize,
    /// Scroll offset (items scrolled off the top).
    pub scroll_offset: usize,
}
