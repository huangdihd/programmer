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

use ratatui::prelude::Color;
use ratatui_markdown::theme::{CodeColors, Generation, RichTextTheme};

/// Central midnight palette for the whole UI. Colors are deliberately muted
/// enough for long terminal sessions while preserving clear semantic contrast.
pub mod palette {
    use ratatui::prelude::Color;

    /// Primary foreground text.
    pub const TEXT: Color = Color::Rgb(0xdc, 0xe3, 0xf1);
    /// Dimmed text (reasoning label, tool details, secondary info).
    pub const MUTED: Color = Color::Rgb(0x7f, 0x8a, 0x9d);
    /// Even fainter text (code-block language label, JSON null).
    pub const FAINT: Color = Color::Rgb(0x55, 0x60, 0x70);

    /// Blue accent (primary, focused border, links, user prompt marker).
    pub const BLUE: Color = Color::Rgb(0x82, 0xaa, 0xff);
    /// Green accent (secondary, strings and success).
    pub const GREEN: Color = Color::Rgb(0x8b, 0xd5, 0xa6);
    /// Cyan accent (information and active output).
    pub const CYAN: Color = Color::Rgb(0x78, 0xdc, 0xe8);
    /// Yellow accent (warnings and pending work).
    pub const YELLOW: Color = Color::Rgb(0xf0, 0xc6, 0x74);
    /// Purple accent (skills and special states).
    pub const PURPLE: Color = Color::Rgb(0xc0, 0x99, 0xff);
    /// Red (errors and destructive actions).
    pub const RED: Color = Color::Rgb(0xff, 0x7a, 0x90);
    /// Muted red (error body text).
    pub const RED_MUTED: Color = Color::Rgb(0xd9, 0x82, 0x91);

    /// Border lines.
    pub const BORDER: Color = Color::Rgb(0x2c, 0x34, 0x42);
    /// Elevated panel / selected-row background.
    pub const SURFACE: Color = Color::Rgb(0x1a, 0x20, 0x2c);
    /// Stepped interactive backgrounds used by the shared 120 ms hover fade.
    pub const SURFACE_HOVER_SOFT: Color = Color::Rgb(0x1d, 0x24, 0x30);
    pub const SURFACE_HOVER_MEDIUM: Color = Color::Rgb(0x21, 0x28, 0x35);
    /// Stable interactive hover background, visibly raised above `SURFACE` cards.
    pub const SURFACE_HOVER: Color = Color::Rgb(0x24, 0x2c, 0x3a);
    /// Code-block background (a touch darker than `SURFACE`).
    pub const CODE_BG: Color = Color::Rgb(0x14, 0x19, 0x23);
    /// Added and removed line backgrounds used by inline diffs.
    pub const DIFF_ADDED_BG: Color = Color::Rgb(0x1d, 0x30, 0x28);
    pub const DIFF_REMOVED_BG: Color = Color::Rgb(0x34, 0x21, 0x2a);
}

/// Syntax-highlight colors derived from the application palette.
pub const CODE_COLORS: CodeColors = CodeColors {
    comment: palette::MUTED,
    keyword: palette::PURPLE,
    string: palette::GREEN,
    string_escape: palette::CYAN,
    number: palette::CYAN,
    constant: palette::PURPLE,
    function: palette::BLUE,
    r#type: palette::CYAN,
    variable: palette::TEXT,
    property: palette::BLUE,
    operator: palette::PURPLE,
    punctuation: palette::MUTED,
    attribute: palette::CYAN,
    tag: palette::BLUE,
    label: palette::RED_MUTED,
    error: palette::RED,
};

pub struct AppTheme;

impl RichTextTheme for AppTheme {
    fn generation(&self) -> Generation {
        Generation(2)
    }

    fn get_text_color(&self) -> Color {
        palette::TEXT
    }
    fn get_muted_text_color(&self) -> Color {
        palette::MUTED
    }

    fn get_primary_color(&self) -> Color {
        palette::BLUE
    }
    fn get_popup_selected_background(&self) -> Color {
        palette::SURFACE
    }
    fn get_border_color(&self) -> Color {
        palette::BORDER
    }
    fn get_focused_border_color(&self) -> Color {
        palette::BLUE
    }

    fn get_secondary_color(&self) -> Color {
        palette::GREEN
    }
    fn get_info_color(&self) -> Color {
        palette::CYAN
    }

    fn get_json_key_color(&self) -> Color {
        palette::BLUE
    }
    fn get_json_string_color(&self) -> Color {
        palette::GREEN
    }

    fn get_json_number_color(&self) -> Color {
        palette::CYAN
    }
    fn get_json_bool_color(&self) -> Color {
        palette::PURPLE
    }
    fn get_json_null_color(&self) -> Color {
        palette::FAINT
    }
    fn get_accent_yellow(&self) -> Color {
        palette::CYAN
    }
    fn get_code_colors(&self) -> CodeColors {
        CODE_COLORS
    }
    fn get_popup_selected_text_color(&self) -> Color {
        palette::TEXT
    }

    fn get_background_color(&self) -> Color {
        Color::Reset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_theme_uses_the_application_palette_for_code() {
        let colors = AppTheme.get_code_colors();

        assert_eq!(colors.comment, palette::MUTED);
        assert_eq!(colors.keyword, palette::PURPLE);
        assert_eq!(colors.string, palette::GREEN);
        assert_eq!(colors.function, palette::BLUE);
        assert_eq!(colors.r#type, palette::CYAN);
        assert_eq!(colors.variable, palette::TEXT);
        assert_eq!(colors.error, palette::RED);
    }
}
