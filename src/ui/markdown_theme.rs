use ratatui::prelude::Color;
use ratatui_markdown::theme::{Generation, RichTextTheme};

pub struct AppTheme;

impl RichTextTheme for AppTheme {
    fn generation(&self) -> Generation { Generation(2) }

    fn get_text_color(&self) -> Color { Color::Rgb(0xd8, 0xd8, 0xd8) }
    fn get_muted_text_color(&self) -> Color { Color::Rgb(0x80, 0x88, 0x90) }

    fn get_primary_color(&self) -> Color { Color::Rgb(0x7a, 0xa2, 0xf7) }
    fn get_popup_selected_background(&self) -> Color { Color::Rgb(0x2a, 0x2f, 0x3a) }
    fn get_border_color(&self) -> Color { Color::Rgb(0x3b, 0x42, 0x52) }
    fn get_focused_border_color(&self) -> Color { Color::Rgb(0x7a, 0xa2, 0xf7) }

    fn get_secondary_color(&self) -> Color { Color::Rgb(0x9e, 0xce, 0x6a) }
    fn get_info_color(&self) -> Color { Color::Rgb(0x7d, 0xcf, 0xff) }

    fn get_json_key_color(&self) -> Color { Color::Rgb(0x7a, 0xa2, 0xf7) }
    fn get_json_string_color(&self) -> Color { Color::Rgb(0x9e, 0xce, 0x6a) }

    fn get_json_number_color(&self) -> Color { Color::Rgb(0xe0, 0xaf, 0x68) }
    fn get_json_bool_color(&self) -> Color { Color::Rgb(0xbb, 0x9a, 0xf7) }
    fn get_json_null_color(&self) -> Color { Color::Rgb(0x56, 0x5f, 0x89) }
    fn get_accent_yellow(&self) -> Color { Color::Rgb(0xe0, 0xaf, 0x68) }
    fn get_popup_selected_text_color(&self) -> Color { Color::Rgb(0xd8, 0xd8, 0xd8) }

    fn get_background_color(&self) -> Color { Color::Reset }
}