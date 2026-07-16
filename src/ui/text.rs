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

//! Shared CJK-aware text helpers for terminal rendering.
//!
//! All widths here are **display columns** (a CJK character occupies 2), not
//! bytes or chars — slicing by bytes panics on multi-byte characters and
//! counting chars misaligns wide glyphs. Every panel that truncates or wraps
//! text goes through these helpers.

use std::time::Duration;

/// Largest byte index in `s` such that `s[..index]` is at most `max` display
/// columns wide. Always lands on a char boundary.
pub fn byte_index_at_width(s: &str, max: usize) -> usize {
    let mut width = 0usize;
    for (i, c) in s.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
        if width + w > max {
            return i;
        }
        width += w;
    }
    s.len()
}

/// Truncate `s` to at most `max` display columns, appending `…` when cut.
pub fn truncate_to_width(s: &str, max: usize) -> String {
    let end = byte_index_at_width(s, max);
    if end == s.len() {
        s.to_string()
    } else {
        format!("{}…", &s[..end])
    }
}

/// Split `text` into chunks of at most `max` display columns, preferring to
/// break at spaces. `max == 0` returns the text unsplit.
pub fn wrap_to_width(text: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let fit = byte_index_at_width(remaining, max);
        if fit == remaining.len() {
            chunks.push(remaining.to_string());
            break;
        }
        let mut break_at = fit;
        if let Some(pos) = remaining[..fit].rfind(' ') {
            break_at = pos;
        }
        if break_at == 0 {
            // No usable break point and the first char alone exceeds the
            // budget (e.g. a wide char with max == 1): take it anyway.
            break_at = remaining
                .char_indices()
                .nth(1)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
        }
        chunks.push(remaining[..break_at].to_string());
        remaining = remaining[break_at..].trim_start();
    }
    chunks
}

/// Compact human-readable duration: `42s`, `3m12s`, `1h04m`.
pub fn format_duration_secs(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_width_aware_and_boundary_safe() {
        // ASCII within budget: unchanged.
        assert_eq!(truncate_to_width("hello", 10), "hello");
        // ASCII over budget: cut with ellipsis.
        assert_eq!(truncate_to_width("hello world", 5), "hello…");
        // CJK chars are 2 columns wide; never split mid-char.
        assert_eq!(truncate_to_width("浏览器布局", 4), "浏览…");
        assert_eq!(truncate_to_width("浏览器布局", 5), "浏览…");
        // Zero budget is safe.
        assert_eq!(truncate_to_width("x", 0), "…");
        assert_eq!(truncate_to_width("", 0), "");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_survives_cjk() {
        assert_eq!(wrap_to_width("ab cd", 3), vec!["ab", "cd"]);
        // A long CJK run wraps at width boundaries without panicking.
        let chunks = wrap_to_width("浏览器上的布局错位问题", 6);
        assert!(chunks.iter().all(|c| c.chars().count() <= 3));
        assert_eq!(chunks.concat(), "浏览器上的布局错位问题");
        // Degenerate budget still makes progress.
        assert_eq!(wrap_to_width("浏", 1), vec!["浏"]);
        assert_eq!(wrap_to_width("abc", 0), vec!["abc"]);
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration_secs(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration_secs(Duration::from_secs(192)), "3m12s");
        assert_eq!(format_duration_secs(Duration::from_secs(3840)), "1h04m");
    }
}
