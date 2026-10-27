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

//! Shared static pointer feedback for terminal controls.
//!
//! Hover transitions previously requested several full TUI redraws. Native
//! image protocols can turn those decorative frames into expensive terminal
//! output, so hover now changes once and remains stable until the target moves.

use crate::ui::markdown_theme::palette;
use ratatui::style::Color;

#[derive(Debug, Default)]
pub(crate) struct InteractionAnimation {
    _private: (),
}

impl InteractionAnimation {
    pub(crate) fn restart(&mut self) {}

    pub(crate) fn cancel(&mut self) {}

    pub(crate) fn hover_background(&self) -> Color {
        palette::SURFACE_HOVER
    }

    pub(crate) fn active(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_feedback_is_immediate_and_never_schedules_animation_frames() {
        let mut feedback = InteractionAnimation::default();

        feedback.restart();
        assert_eq!(feedback.hover_background(), palette::SURFACE_HOVER);
        assert!(!feedback.active());

        feedback.cancel();
        assert_eq!(feedback.hover_background(), palette::SURFACE_HOVER);
        assert!(!feedback.active());
    }
}
