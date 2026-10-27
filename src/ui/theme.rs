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

//! Presentation-only theme conversion. Cached Markdown retains its original
//! dark styles; conversion happens once on the completed frame, so switching
//! themes also updates cached/live content without rebuilding it.
use ratatui::{buffer::Buffer, layout::Rect, style::Color};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use super::markdown_theme::palette;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

static TERMINAL_BACKGROUND: OnceLock<Option<(u8, u8, u8)>> = OnceLock::new();

impl Theme {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn resolve(self, background: Option<(u8, u8, u8)>) -> Self {
        match (self, background) {
            (Self::Auto, Some((r, g, b))) => {
                let brightness = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
                if brightness >= 128_000 {
                    Self::Light
                } else {
                    Self::Dark
                }
            }
            (Self::Auto, None) => Self::Dark,
            _ => self,
        }
    }

    pub fn effective(self) -> Self {
        self.resolve(TERMINAL_BACKGROUND.get().copied().flatten())
    }

    pub fn status(self) -> String {
        self.status_with_background(TERMINAL_BACKGROUND.get().copied().flatten())
    }

    fn status_with_background(self, background: Option<(u8, u8, u8)>) -> String {
        if self != Self::Auto {
            return format!("Theme: {} (manual override)", self.label());
        }
        match background {
            Some((r, g, b)) => format!(
                "Theme: auto → {} (startup terminal background #{r:02x}{g:02x}{b:02x})",
                self.resolve(background).label()
            ),
            None => {
                "Theme: auto → dark (terminal background detection unavailable; fallback)".into()
            }
        }
    }
}

pub(crate) fn set_terminal_background(background: Option<(u8, u8, u8)>) {
    let _ = TERMINAL_BACKGROUND.set(background);
}

// Light counterpart to the midnight palette. Neutral surfaces stay cool while
// semantic accents are dark enough to remain legible on pale backgrounds.
const BACKGROUND: Color = Color::Rgb(246, 247, 251);
const TEXT: Color = Color::Rgb(37, 42, 53);
const MUTED: Color = Color::Rgb(111, 119, 133);
const BLUE: Color = Color::Rgb(79, 111, 216);
const GREEN: Color = Color::Rgb(62, 148, 100);
const CYAN: Color = Color::Rgb(40, 136, 155);
const YELLOW: Color = Color::Rgb(168, 117, 24);
const PURPLE: Color = Color::Rgb(138, 92, 194);
const RED: Color = Color::Rgb(212, 78, 98);

// Reserved presentation tokens distinguish decorative UI roles from syntax and
// status colors. Resolve only after rendering so cached paragraphs stay valid.
pub(crate) mod role {
    use ratatui::style::Color;
    pub const BORDER: Color = Color::Rgb(1, 0, 1);
    pub const FOCUS: Color = Color::Rgb(1, 0, 2);
    pub const HEADING: Color = Color::Rgb(1, 0, 3);
    pub const SECTION_PROVIDERS: Color = Color::Rgb(1, 0, 4);
    pub const SECTION_SKILLS: Color = Color::Rgb(1, 0, 5);
    pub const SECTION_MCP: Color = Color::Rgb(1, 0, 6);
    pub const SECTION_TODOS: Color = Color::Rgb(1, 0, 7);
    pub const SECTION_TASKS: Color = Color::Rgb(1, 0, 8);
    pub const SECTION_AGENTS: Color = Color::Rgb(1, 0, 9);
    pub const SECTION_DIAGNOSTICS: Color = Color::Rgb(1, 0, 10);
    pub const TOOL_SUCCESS: Color = Color::Rgb(1, 0, 11);
    pub const TOOL_PENDING: Color = Color::Rgb(1, 0, 12);
    pub const TITLE: Color = Color::Rgb(1, 0, 13);
    pub const LOGO: Color = Color::Rgb(1, 0, 14);
    pub const SUBTLE: Color = Color::Rgb(1, 0, 15);
    pub const DIVIDER: Color = Color::Rgb(1, 0, 16);
    pub const WELCOME_DIVIDER: Color = Color::Rgb(1, 0, 17);
    pub const SELECTION_BG: Color = Color::Rgb(1, 0, 18);
    pub const SELECTION_TEXT: Color = Color::Rgb(1, 0, 19);
    pub const INTERACTION_PRESSED_BG: Color = Color::Rgb(1, 0, 20);
    pub const STATUS_CONNECTING: Color = Color::Rgb(1, 0, 21);
    pub const STATUS_THINKING: Color = Color::Rgb(1, 0, 22);
    pub const STATUS_THINKING_DEEP: Color = Color::Rgb(1, 0, 23);
    pub const STATUS_THINKING_DEEPEST: Color = Color::Rgb(1, 0, 24);
    pub const STATUS_TOOL_RUNNING: Color = Color::Rgb(1, 0, 25);
}

fn role_color(color: Color, light: bool) -> Option<Color> {
    let (dark, bright) = match color {
        role::BORDER => (palette::BORDER, Color::Rgb(205, 211, 224)),
        role::SELECTION_BG => (palette::SURFACE, Color::Rgb(225, 231, 245)),
        role::SELECTION_TEXT => (palette::TEXT, TEXT),
        role::INTERACTION_PRESSED_BG => {
            (Color::Rgb(0x31, 0x3d, 0x52), Color::Rgb(0xd4, 0xdc, 0xea))
        }
        role::STATUS_CONNECTING => (palette::BLUE, BLUE),
        role::STATUS_THINKING => (palette::BLUE, BLUE),
        role::STATUS_THINKING_DEEP => (Color::Rgb(111, 145, 224), Color::Rgb(65, 91, 185)),
        role::STATUS_THINKING_DEEPEST => (Color::Rgb(93, 121, 194), Color::Rgb(52, 72, 154)),
        role::STATUS_TOOL_RUNNING => (palette::YELLOW, YELLOW),
        role::FOCUS => (palette::BLUE, BLUE),
        role::HEADING => (palette::BLUE, BLUE),
        role::SECTION_PROVIDERS => (palette::GREEN, GREEN),
        role::SECTION_SKILLS => (palette::PURPLE, PURPLE),
        role::SECTION_MCP => (palette::PURPLE, PURPLE),
        role::SECTION_TODOS => (palette::YELLOW, YELLOW),
        role::SECTION_TASKS => (palette::CYAN, CYAN),
        role::SECTION_AGENTS => (palette::BLUE, BLUE),
        role::SECTION_DIAGNOSTICS => (palette::RED, RED),
        role::TOOL_SUCCESS => (palette::GREEN, GREEN),
        role::TOOL_PENDING => (palette::YELLOW, YELLOW),
        role::TITLE => (palette::TEXT, TEXT),
        role::LOGO => (palette::BLUE, BLUE),
        role::SUBTLE => (palette::MUTED, MUTED),
        role::DIVIDER => (palette::BORDER, Color::Rgb(215, 220, 230)),
        role::WELCOME_DIVIDER => (palette::BORDER, Color::Rgb(215, 220, 230)),
        _ => return None,
    };
    Some(if light { bright } else { dark })
}

fn dark_color(color: Color, background: bool) -> Color {
    match color {
        Color::Reset => Color::Reset,
        Color::Black if !background => palette::CODE_BG,
        Color::White | Color::Gray => palette::TEXT,
        Color::DarkGray => palette::MUTED,
        Color::Blue | Color::LightBlue => palette::BLUE,
        Color::Green | Color::LightGreen => palette::GREEN,
        Color::Cyan | Color::LightCyan => palette::CYAN,
        Color::Yellow | Color::LightYellow => palette::YELLOW,
        Color::Magenta | Color::LightMagenta => palette::PURPLE,
        Color::Red | Color::LightRed => palette::RED,
        Color::Rgb(30, 30, 40) => palette::SURFACE,
        other => other,
    }
}

fn light_color(color: Color, background: bool) -> Color {
    match color {
        Color::Reset if background => BACKGROUND,
        Color::Reset | Color::White | palette::TEXT => TEXT,
        Color::Gray => MUTED,
        Color::DarkGray | palette::MUTED | palette::FAINT => MUTED,
        Color::Blue | Color::LightBlue | palette::BLUE => BLUE,
        Color::Green | Color::LightGreen | palette::GREEN => GREEN,
        Color::Cyan | Color::LightCyan | palette::CYAN => CYAN,
        Color::Yellow | Color::LightYellow | palette::YELLOW => YELLOW,
        Color::Magenta | Color::LightMagenta | palette::PURPLE => PURPLE,
        Color::Red | Color::LightRed | palette::RED => RED,
        palette::RED_MUTED => Color::Rgb(190, 91, 108),
        palette::BORDER => Color::Rgb(205, 211, 224),
        palette::SURFACE | Color::Rgb(30, 30, 40) => Color::Rgb(236, 239, 245),
        palette::SURFACE_HOVER_SOFT => Color::Rgb(233, 236, 243),
        palette::SURFACE_HOVER_MEDIUM => Color::Rgb(229, 233, 241),
        palette::SURFACE_HOVER => Color::Rgb(225, 230, 240),
        palette::CODE_BG => Color::Rgb(231, 235, 243),
        palette::DIFF_REMOVED_BG => Color::Rgb(248, 229, 233),
        palette::DIFF_ADDED_BG => Color::Rgb(228, 241, 233),
        other => other,
    }
}

pub(crate) fn apply(theme: Theme, area: Rect, buf: &mut Buffer) {
    let light = theme.effective() == Theme::Light;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let Some(cell) = buf.cell_mut((x, y)) else {
                continue;
            };
            // Graphics protocols and half-block images carry actual pixel colors.
            if cell.diff_option == ratatui::buffer::CellDiffOption::Skip
                || (matches!(cell.symbol(), "▀" | "▄" | "█")
                    && role_color(cell.fg, light).is_none())
            {
                continue;
            }
            let foreground_role = role_color(cell.fg, light);
            let background_role = role_color(cell.bg, light);
            if !light {
                cell.fg = foreground_role.unwrap_or_else(|| dark_color(cell.fg, false));
                cell.bg = background_role.unwrap_or_else(|| dark_color(cell.bg, true));
                if theme != Theme::Dark {
                    continue;
                }
                // An explicit dark override paints a deterministic canvas;
                // auto-dark keeps the terminal's own background transparent.
                if cell.bg == Color::Reset {
                    cell.bg = Color::Rgb(16, 19, 26);
                }
                if cell.fg == Color::Reset {
                    cell.fg = palette::TEXT;
                }
                continue;
            }
            let accent_background = matches!(
                cell.bg,
                Color::Cyan
                    | Color::LightCyan
                    | Color::Blue
                    | Color::LightBlue
                    | palette::CYAN
                    | palette::BLUE
            );
            cell.fg = if let Some(color) = foreground_role {
                color
            } else if accent_background {
                Color::White
            } else {
                light_color(cell.fg, false)
            };
            cell.bg = background_role.unwrap_or_else(|| light_color(cell.bg, true));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_palette_matches_the_midnight_counterpart() {
        assert_eq!(BACKGROUND, Color::Rgb(246, 247, 251));
        assert_eq!(TEXT, Color::Rgb(37, 42, 53));
        assert_eq!(MUTED, Color::Rgb(111, 119, 133));
        assert_eq!(BLUE, Color::Rgb(79, 111, 216));
        assert_eq!(GREEN, Color::Rgb(62, 148, 100));
        assert_eq!(CYAN, Color::Rgb(40, 136, 155));
        assert_eq!(YELLOW, Color::Rgb(168, 117, 24));
        assert_eq!(PURPLE, Color::Rgb(138, 92, 194));
        assert_eq!(RED, Color::Rgb(212, 78, 98));
        assert_eq!(
            light_color(palette::SURFACE, true),
            Color::Rgb(236, 239, 245)
        );
        assert_eq!(
            light_color(palette::SURFACE_HOVER_SOFT, true),
            Color::Rgb(233, 236, 243)
        );
        assert_eq!(
            light_color(palette::SURFACE_HOVER_MEDIUM, true),
            Color::Rgb(229, 233, 241)
        );
        assert_eq!(
            light_color(palette::SURFACE_HOVER, true),
            Color::Rgb(225, 230, 240)
        );
        assert_eq!(
            light_color(palette::CODE_BG, true),
            Color::Rgb(231, 235, 243)
        );
        for (source, token) in [
            (palette::BLUE, role::SECTION_AGENTS),
            (palette::GREEN, role::SECTION_PROVIDERS),
            (palette::TEXT, role::TITLE),
            (palette::PURPLE, role::SECTION_SKILLS),
            (palette::YELLOW, role::SECTION_TODOS),
            (palette::RED, role::SECTION_DIAGNOSTICS),
        ] {
            assert_eq!(Some(light_color(source, false)), role_color(token, true));
        }
    }

    #[test]
    fn light_accents_have_sufficient_contrast() {
        fn luminance(color: Color) -> f64 {
            let Color::Rgb(r, g, b) = color else {
                panic!("RGB accent")
            };
            let linear = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        }
        let background_luminance = luminance(BACKGROUND);
        for accent in [BLUE, GREEN, CYAN, YELLOW, PURPLE, RED] {
            let accent_luminance = luminance(accent);
            let contrast = (background_luminance + 0.05) / (accent_luminance + 0.05);
            assert!(contrast >= 3.0, "accent contrast was {contrast:.2}");
        }
        for (token, expected) in [
            (role::HEADING, BLUE),
            (role::SECTION_PROVIDERS, GREEN),
            (role::SECTION_SKILLS, PURPLE),
            (role::SECTION_TODOS, YELLOW),
            (role::TITLE, TEXT),
            (role::TOOL_SUCCESS, GREEN),
        ] {
            assert_eq!(role_color(token, true), Some(expected));
        }
    }

    #[test]
    fn manual_status_does_not_claim_to_follow_terminal_background() {
        let background = Some((36, 39, 58));
        assert_eq!(
            Theme::Light.status_with_background(background),
            "Theme: light (manual override)"
        );
        assert!(
            Theme::Auto
                .status_with_background(background)
                .contains("auto → dark (startup")
        );
        assert!(
            Theme::Auto
                .status_with_background(None)
                .contains("fallback")
        );
    }

    #[test]
    fn semantic_roles_separate_decoration_from_status_and_preserve_dark() {
        assert_eq!(role_color(role::SECTION_SKILLS, true), Some(PURPLE));
        assert_eq!(
            role_color(role::SECTION_SKILLS, false),
            Some(palette::PURPLE)
        );
        assert_eq!(role_color(role::BORDER, false), Some(palette::BORDER));
        assert_eq!(
            role_color(role::BORDER, true),
            Some(Color::Rgb(205, 211, 224))
        );
        assert_ne!(
            role_color(role::BORDER, true),
            role_color(role::HEADING, true)
        );
        assert_eq!(role_color(role::TOOL_SUCCESS, true), Some(GREEN));
        assert_eq!(light_color(palette::GREEN, false), GREEN);
        assert_eq!(
            role_color(role::STATUS_CONNECTING, false),
            Some(palette::BLUE)
        );
        assert_eq!(role_color(role::STATUS_CONNECTING, true), Some(BLUE));
        assert_eq!(
            role_color(role::STATUS_TOOL_RUNNING, false),
            Some(palette::YELLOW)
        );
        assert_eq!(role_color(role::STATUS_TOOL_RUNNING, true), Some(YELLOW));
        for light in [false, true] {
            assert_ne!(
                role_color(role::STATUS_THINKING, light),
                role_color(role::STATUS_THINKING_DEEP, light)
            );
            assert_ne!(
                role_color(role::STATUS_THINKING_DEEP, light),
                role_color(role::STATUS_THINKING_DEEPEST, light)
            );
        }
        assert_ne!(light_color(Color::Gray, false), TEXT);
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        buf[(0, 0)].set_symbol("█").set_fg(role::LOGO);
        apply(Theme::Light, area, &mut buf);
        assert_eq!(buf[(0, 0)].fg, BLUE);
        assert_eq!(buf[(0, 0)].bg, BACKGROUND);
    }

    #[test]
    fn auto_detects_brightness_and_falls_back_to_dark() {
        assert_eq!(Theme::default(), Theme::Auto);
        assert_eq!(Theme::Auto.resolve(None), Theme::Dark);
        assert_eq!(Theme::Auto.resolve(Some((20, 20, 20))), Theme::Dark);
        assert_eq!(Theme::Auto.resolve(Some((245, 245, 245))), Theme::Light);
        assert_eq!(Theme::Dark.resolve(Some((255, 255, 255))), Theme::Dark);
        assert_eq!(Theme::Light.resolve(None), Theme::Light);
        assert_eq!(Theme::parse(" invalid "), None);
    }

    #[test]
    fn conversion_preserves_dark_and_changes_light_selection_and_diff() {
        let area = Rect::new(0, 0, 3, 1);
        let mut original = Buffer::empty(area);
        original[(1, 0)].set_fg(Color::Black).set_bg(Color::Cyan);
        original[(2, 0)]
            .set_fg(palette::RED)
            .set_bg(palette::DIFF_REMOVED_BG);
        let mut dark = original.clone();
        apply(Theme::Dark, area, &mut dark);
        assert_eq!(dark[(0, 0)].bg, Color::Rgb(16, 19, 26));
        assert_eq!(dark[(1, 0)].fg, palette::CODE_BG);
        assert_eq!(dark[(1, 0)].bg, palette::CYAN);
        assert_eq!(dark[(2, 0)], original[(2, 0)]);
        apply(Theme::Light, area, &mut original);
        assert_eq!(original[(0, 0)].bg, BACKGROUND);
        assert_eq!(original[(0, 0)].fg, TEXT);
        assert_eq!(original[(1, 0)].fg, Color::White);
        assert_eq!(original[(1, 0)].bg, CYAN);
        assert_eq!(original[(2, 0)].bg, Color::Rgb(248, 229, 233));
    }
}
