pub mod text;
pub mod reasoning;
pub mod unsupported;

use ratatui::prelude::Color;
use ratatui::style::{Modifier, Style};

/// Dim, italic style shared by the non-text assistant items (the reasoning label
/// and the unsupported-item placeholder).
pub(crate) fn muted_style() -> Style {
    Style::new()
        .fg(Color::Rgb(0x80, 0x88, 0x90))
        .add_modifier(Modifier::DIM | Modifier::ITALIC)
}
