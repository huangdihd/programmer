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

//! Conservative link hit testing for a renderer that discards destinations.
//!
//! Match complete underlined runs in the original paragraph (before theme
//! remapping), never search the visible viewport for arbitrary label text.
//! Ambiguous labels and unsupported Markdown forms deliberately remain inert.

use std::collections::HashMap;
use std::process::{Command, Stdio};

use ratatui::{buffer::Buffer, style::Modifier};

use crate::ui::markdown_theme::palette;
use unicode_width::UnicodeWidthStr;

fn normalized(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Accept only browser URLs, including mixed-case schemes, with a real host.
fn browser_url(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    Some(url.to_string())
}

/// Mirrors the dependency's balanced bracket/parenthesis inline syntax.
/// Destination metadata is not provided by ratatui-markdown 5a43c54.
fn destinations(source: &str) -> HashMap<String, Option<String>> {
    let chars: Vec<char> = source.chars().collect();
    let mut result = HashMap::new();
    for start in 0..chars.len() {
        if chars[start] != '[' {
            continue;
        }
        let mut end = start + 1;
        let mut depth = 0usize;
        while end < chars.len() {
            match chars[end] {
                '[' => depth += 1,
                ']' if depth == 0 => break,
                ']' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        if chars.get(end + 1) != Some(&'(') {
            continue;
        }
        let mut close = end + 2;
        depth = 1;
        while close < chars.len() {
            match chars[close] {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            close += 1;
        }
        // An incomplete streaming destination is never clickable.
        if close == chars.len() {
            continue;
        }
        let target: String = chars[end + 2..close].iter().collect();
        let label: String = chars[start + 1..end].iter().collect();
        let key = normalized(if label.is_empty() { &target } else { &label });
        let url = browser_url(&target);
        result
            .entry(key)
            .and_modify(|old| {
                if *old != url {
                    *old = None;
                }
            })
            .or_insert(url);
    }
    result
}

/// Resolve a cell in a full, unscrolled paragraph. Padding and wrap boundaries
/// do not contribute text; whitespace is ignored because wrapping trims it.
pub(super) fn hit(source: &str, buffer: &Buffer, x: u16, y: u16) -> Option<String> {
    let targets = destinations(source);
    let mut label = String::new();
    let mut contains_click = false;
    let mut answer = None;
    let finish = |label: &mut String, contains_click: &mut bool, answer: &mut Option<String>| {
        if *contains_click {
            *answer = targets.get(&normalized(label)).cloned().flatten();
        }
        label.clear();
        *contains_click = false;
    };
    for row in buffer.area.y..buffer.area.bottom() {
        let mut continuation_until = buffer.area.x;
        let mut row_started = false;
        for col in buffer.area.x..buffer.area.right() {
            if col < continuation_until {
                continue;
            }
            let cell = &buffer[(col, row)];
            continuation_until = col.saturating_add(cell.symbol().width().max(1) as u16);
            let linked = cell.fg == palette::BLUE && cell.modifier.contains(Modifier::UNDERLINED);
            if linked {
                row_started = true;
                label.push_str(cell.symbol());
                contains_click |= x >= col && x < continuation_until && row == y;
            } else if !cell.symbol().trim().is_empty()
                || row_started
                    && ((col + 1)..buffer.area.right())
                        .any(|next| !buffer[(next, row)].symbol().trim().is_empty())
            {
                finish(&mut label, &mut contains_click, &mut answer);
            }
        }
        // Blank rows separate links, unlike padded wrapped rows.
        if (buffer.area.x..buffer.area.right())
            .all(|col| buffer[(col, row)].symbol().trim().is_empty())
        {
            finish(&mut label, &mut contains_click, &mut answer);
        }
    }
    finish(&mut label, &mut contains_click, &mut answer);
    answer
}

/// Pass the validated URL as one process argument, never through a shell.
/// Waiting happens off the UI thread and reaps the launcher process.
pub(crate) fn open(url: &str) -> std::io::Result<()> {
    let url = browser_url(url).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only HTTP(S) links can be opened",
        )
    })?;
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    let mut child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{layout::Rect, style::Style};

    #[test]
    fn destinations_are_balanced_safe_and_unambiguous() {
        let links = destinations(
            "[same](https://one.test) [same](https://two.test) [ok](https://a.test/a(b)) [bad](file:///tmp/x) [unfinished](https://a.test",
        );
        assert_eq!(links["same"], None);
        assert_eq!(links["ok"].as_deref(), Some("https://a.test/a(b)"));
        assert_eq!(links["bad"], None);
        assert!(!links.contains_key("unfinished"));
        assert!(browser_url("https://a.test/\nfoo").is_none());
        assert!(browser_url("javascript:alert(1)").is_none());
        assert!(browser_url("https://a.test/?q=$(touch%20x)&a='x'").is_some());
    }

    #[test]
    fn real_markdown_wraps_without_linking_code_or_plain_text() {
        use ratatui::widgets::Widget;
        use ratatui_widgets::paragraph::Paragraph;
        let source = "[a long link label that wraps](https://example.test/a(b))\n\n`[fake](https://bad.test)`\n\nplain label";
        let (text, _) =
            crate::ui::components::messages::assistant::text::render_markdown(source, 14);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 14, 20));
        Paragraph::new(text).render(buffer.area, &mut buffer);
        let mut rows = std::collections::HashSet::new();
        for y in 0..20 {
            for x in 0..14 {
                if let Some(url) = hit(source, &buffer, x, y) {
                    assert_eq!(url, "https://example.test/a(b)");
                    rows.insert(y);
                }
            }
        }
        assert!(rows.len() >= 2, "link should span wrapped rows: {rows:?}");
    }

    #[test]
    fn wrapped_label_uses_full_paragraph_coordinates() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 3));
        let style = Style::new()
            .fg(palette::BLUE)
            .add_modifier(Modifier::UNDERLINED);
        buffer.set_string(1, 0, "hello", style);
        buffer.set_string(1, 1, "世界", style);
        let source = "[hello 世界](https://example.test)";
        assert_eq!(
            hit(source, &buffer, 1, 1).as_deref(),
            Some("https://example.test/")
        );
        assert_eq!(hit(source, &buffer, 0, 1), None);
        assert_eq!(hit(source, &buffer, 1, 2), None);
    }
}
