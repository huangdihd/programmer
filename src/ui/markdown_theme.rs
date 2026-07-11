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

use ratatui::prelude::Color;
use ratatui_markdown::theme::{Generation, RichTextTheme};

/// Central color palette for the whole UI (Tokyo Night-ish). Every color used
/// anywhere in the app is defined here so the theme lives in one place.
pub mod palette {
    use ratatui::prelude::Color;

    /// Primary foreground text.
    pub const TEXT: Color = Color::Rgb(0xd8, 0xd8, 0xd8);
    /// Dimmed text (reasoning label, tool details, secondary info).
    pub const MUTED: Color = Color::Rgb(0x80, 0x88, 0x90);
    /// Even fainter text (code-block language label, JSON null).
    pub const FAINT: Color = Color::Rgb(0x56, 0x5f, 0x89);

    /// Blue accent (primary, focused border, links, user prompt marker).
    pub const BLUE: Color = Color::Rgb(0x7a, 0xa2, 0xf7);
    /// Green accent (secondary, strings).
    pub const GREEN: Color = Color::Rgb(0x9e, 0xce, 0x6a);
    /// Cyan accent (info).
    pub const CYAN: Color = Color::Rgb(0x7d, 0xcf, 0xff);
    /// Yellow accent (numbers, tool call marker).
    pub const YELLOW: Color = Color::Rgb(0xe0, 0xaf, 0x68);
    /// Purple accent (booleans).
    pub const PURPLE: Color = Color::Rgb(0xbb, 0x9a, 0xf7);
    /// Red (errors).
    pub const RED: Color = Color::Rgb(0xf7, 0x76, 0x8e);
    /// Muted red (error body text).
    pub const RED_MUTED: Color = Color::Rgb(0xd8, 0x8b, 0x94);

    /// Border lines.
    pub const BORDER: Color = Color::Rgb(0x3b, 0x42, 0x52);
    /// Panel / selected-row background (user bar, expanded detail panel).
    pub const SURFACE: Color = Color::Rgb(0x2a, 0x2f, 0x3a);
    /// Code-block background (a touch darker than `SURFACE`).
    pub const CODE_BG: Color = Color::Rgb(0x1f, 0x23, 0x35);
}

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
        palette::YELLOW
    }
    fn get_json_bool_color(&self) -> Color {
        palette::PURPLE
    }
    fn get_json_null_color(&self) -> Color {
        palette::FAINT
    }
    fn get_accent_yellow(&self) -> Color {
        palette::YELLOW
    }
    fn get_popup_selected_text_color(&self) -> Color {
        palette::TEXT
    }

    fn get_background_color(&self) -> Color {
        Color::Reset
    }
}
