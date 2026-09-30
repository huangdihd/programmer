// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

use super::peer_exchange::short_source;
use crate::response::message_item::PeerDelegationState;
use crate::ui::markdown_theme::palette;
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

pub fn paragraph(
    from: &str,
    body: Option<&str>,
    state: PeerDelegationState,
    expanded: bool,
) -> Paragraph<'static> {
    let (status, note) = match state {
        PeerDelegationState::Pending => {
            ("pending", "Received delegation; waiting for your choice.")
        }
        PeerDelegationState::AcceptedQueued => (
            "accepted · queued",
            "Waiting to start; does not interrupt work, input or approvals. Acceptance is not completion.",
        ),
        PeerDelegationState::Started => (
            "started",
            "Delegated execution started; no completion report implied.",
        ),
        PeerDelegationState::Rejected => (
            "rejected",
            "Declined; no delegated model execution was started.",
        ),
    };
    let accent = Style::new().fg(palette::PURPLE);
    let excerpt = body
        .and_then(|body| body.lines().find(|line| !line.trim().is_empty()))
        .map(str::trim)
        .unwrap_or("Task text unavailable");
    let truncated = excerpt.chars().count() > 64;
    let mut excerpt: String = excerpt.chars().take(64).collect();
    if truncated {
        excerpt.push('…');
    }
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "{} ↔ {excerpt} · {status} · {}",
                if expanded { "▾" } else { "▸" },
                short_source(from)
            ),
            accent,
        )),
        Line::from(Span::styled(
            note.to_owned(),
            Style::new().fg(palette::MUTED),
        )),
    ];
    if expanded {
        for (label, text) in [
            ("Source", from),
            ("Task", body.unwrap_or("Task text unavailable")),
        ] {
            lines.push(Line::from(Span::styled(format!("  {label}"), accent)));
            lines.extend(text.lines().map(|line| {
                Line::from(Span::styled(
                    format!("  {line}"),
                    Style::new().fg(palette::TEXT),
                ))
            }));
        }
    }
    lines.push(Line::default());
    let paragraph = Paragraph::new(lines);
    if expanded {
        paragraph.wrap(Wrap { trim: false })
    } else {
        paragraph
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    #[test]
    fn collapsed_has_two_rows_and_trailing_blank_at_small_widths() {
        for state in [
            PeerDelegationState::Pending,
            PeerDelegationState::AcceptedQueued,
            PeerDelegationState::Started,
            PeerDelegationState::Rejected,
        ] {
            for width in [0, 1, 2, 8, 40, 160] {
                let area = Rect::new(0, 0, width, 4);
                let mut buffer = Buffer::empty(area);
                paragraph("peer", Some("检查任务\nfull details"), state, false)
                    .render(area, &mut buffer);
                if width > 0 {
                    assert_eq!(buffer[(0, 0)].symbol(), "▸");
                    assert_eq!(buffer[(0, 0)].fg, palette::PURPLE);
                    assert_ne!(buffer[(0, 1)].symbol(), " ");
                    assert_eq!(buffer[(0, 2)].symbol(), " ");
                }
            }
        }
    }
}
