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

//! Shared, short-lived interaction animation for terminal pointer feedback.
//!
//! Terminals cannot blend every frame reliably across light and dark themes,
//! so the transition uses three semantic palette steps over 120 ms. Frames are
//! requested only while a transition is active.

use crate::ui::markdown_theme::palette;
use ratatui::style::Color;
use std::time::{Duration, Instant};

const HOVER_STEP: Duration = Duration::from_millis(40);
const HOVER_DURATION: Duration = Duration::from_millis(120);

#[derive(Debug, Default)]
pub(crate) struct InteractionAnimation {
    started_at: Option<Instant>,
}

impl InteractionAnimation {
    pub(crate) fn restart(&mut self) {
        self.started_at = Some(Instant::now());
    }

    /// Stop requesting frames when no animated target remains visible.
    pub(crate) fn cancel(&mut self) {
        self.started_at = None;
    }

    pub(crate) fn hover_background(&self) -> Color {
        self.started_at
            .map(|started_at| hover_background_at(started_at.elapsed()))
            .unwrap_or(palette::SURFACE_HOVER)
    }

    pub(crate) fn active(&self) -> bool {
        self.started_at
            .is_some_and(|started_at| started_at.elapsed() < HOVER_DURATION)
    }
}

fn hover_background_at(elapsed: Duration) -> Color {
    if elapsed < HOVER_STEP {
        palette::SURFACE_HOVER_SOFT
    } else if elapsed < HOVER_STEP.saturating_mul(2) {
        palette::SURFACE_HOVER_MEDIUM
    } else {
        palette::SURFACE_HOVER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_animation_advances_to_a_stable_final_color() {
        assert_eq!(
            hover_background_at(Duration::ZERO),
            palette::SURFACE_HOVER_SOFT
        );
        assert_eq!(
            hover_background_at(Duration::from_millis(40)),
            palette::SURFACE_HOVER_MEDIUM
        );
        assert_eq!(
            hover_background_at(Duration::from_millis(80)),
            palette::SURFACE_HOVER
        );
        assert_eq!(
            hover_background_at(Duration::from_secs(1)),
            palette::SURFACE_HOVER
        );
    }

    #[test]
    fn default_is_stable_and_restart_activates_the_transition() {
        let mut animation = InteractionAnimation::default();
        assert!(!animation.active());
        assert_eq!(animation.hover_background(), palette::SURFACE_HOVER);
        animation.restart();
        assert!(animation.active());
        assert_eq!(animation.hover_background(), palette::SURFACE_HOVER_SOFT);
        animation.started_at = Some(Instant::now() - HOVER_DURATION);
        assert!(!animation.active());
        assert_eq!(animation.hover_background(), palette::SURFACE_HOVER);

        animation.cancel();
        assert!(!animation.active());
        assert_eq!(animation.hover_background(), palette::SURFACE_HOVER);
    }
}
