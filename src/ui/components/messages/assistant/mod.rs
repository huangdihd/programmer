pub mod text;
pub mod reasoning;
pub mod unsupported;
pub mod tool_call;

use ratatui::style::{Modifier, Style};

use crate::ui::markdown_theme::palette;

/// Dim, italic style shared by the non-text assistant items (the reasoning label
/// and the unsupported-item placeholder).
pub(crate) fn muted_style() -> Style {
    Style::new()
        .fg(palette::MUTED)
        .add_modifier(Modifier::DIM | Modifier::ITALIC)
}
