// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Dream presentation only. The adapter owns typed audit data; the application
//! still owns conflict checking and rollback authorization.
use super::*;
use crate::ui::markdown_theme::{AppTheme, palette};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Position,
    style::{Color, Modifier},
    text::{Span, Text},
};

use ratatui_markdown::markdown::MarkdownRenderer;

const ITEM_HEIGHT: u16 = 4;

#[derive(Debug, Clone, Copy)]
pub enum DreamStatusTone {
    Pending,
    Applied,
    Failed,
    Neutral,
}

impl DreamStatusTone {
    fn color(self) -> Color {
        match self {
            Self::Pending => palette::YELLOW,
            Self::Applied => palette::GREEN,
            Self::Failed => palette::RED,
            Self::Neutral => palette::MUTED,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DreamPresentation {
    pub status: String,
    pub tone: DreamStatusTone,
    pub detail_lines: Vec<Line<'static>>,
    pub metadata: String,
    pub sources: String,
}

#[derive(Debug, Default)]
pub(super) struct DreamState {
    presentations: BTreeMap<String, DreamPresentation>,
    pub(super) metadata_expanded: bool,
    pub(super) sources_expanded: bool,
    offset: usize,
    timeline: Rect,
    rows: Rect,
    details: Rect,
    metadata: Rect,
    sources: Rect,
}

impl ActivityPanel {
    pub fn set_dream_presentations(&mut self, presentations: BTreeMap<String, DreamPresentation>) {
        self.dream.presentations = presentations;
    }

    pub(super) fn handle_dream_mouse(&mut self, event: MouseEvent) -> ActivityAction {
        if self.confirmation.is_some() {
            match event.kind {
                MouseEventKind::ScrollUp => self.scroll_details(KeyCode::Up),
                MouseEventKind::ScrollDown => self.scroll_details(KeyCode::Down),
                _ => {}
            }
            return ActivityAction::None;
        }
        let position = Position::new(event.column, event.row);
        let timeline = self.dream.timeline.contains(position);
        if !timeline && !self.dream.details.contains(position) {
            return ActivityAction::None;
        }
        if !matches!(
            event.kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
        ) {
            return ActivityAction::None;
        }
        self.search
            .handle_key(KeyEvent::new(KeyCode::Enter, event.modifiers));
        self.details_focused = !timeline;
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if self.dream.rows.contains(position) => {
                let index =
                    self.dream.offset + usize::from((event.row - self.dream.rows.y) / ITEM_HEIGHT);
                if index < self.visible_entries().len() && index != self.selected {
                    self.selected = index;
                    self.pending_rollback = None;
                    self.reset_details();
                }
            }
            MouseEventKind::Down(MouseButton::Left) if self.dream.metadata.contains(position) => {
                self.dream.metadata_expanded = !self.dream.metadata_expanded
            }
            MouseEventKind::Down(MouseButton::Left) if self.dream.sources.contains(position) => {
                self.dream.sources_expanded = !self.dream.sources_expanded
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let code = if event.kind == MouseEventKind::ScrollUp {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                return self.handle_key(KeyEvent::new(code, event.modifiers));
            }
            _ => {}
        }
        ActivityAction::None
    }

    pub(super) fn render_dream(&mut self, area: Rect, buffer: &mut Buffer) {
        self.dream.timeline = Rect::default();
        self.dream.rows = Rect::default();
        self.dream.details = Rect::default();
        self.dream.sources = Rect::default();
        self.dream.metadata = Rect::default();
        Clear.render(area, buffer);
        if area.is_empty() {
            return;
        }
        let sections = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::from(self.notice.is_some())),
            Constraint::Min(0),
            Constraint::Length(if area.height >= 8 { 2 } else { 1 }),
        ])
        .split(area);
        Paragraph::new(format!(" {}", self.title))
            .style(Style::default().fg(role::FOCUS))
            .render(sections[0], buffer);
        if let Some(notice) = &self.notice {
            Paragraph::new(format!("Notice: {notice}"))
                .style(Style::default().fg(palette::YELLOW))
                .render(sections[1], buffer);
        }
        if let Some((id, details)) = &self.confirmation {
            let inner = region(sections[2], format!("Confirm rollback: {id}"), true, buffer);
            self.render_dream_text(inner, buffer, Text::from(details.clone()));
            Paragraph::new("y confirm whole-run rollback · n/Esc cancel · ↑↓ scroll")
                .style(Style::default().fg(palette::YELLOW))
                .render(sections[3], buffer);
            return;
        }
        let content = sections[2];
        if content.width < 80 {
            if self.details_focused {
                self.render_dream_details(content, buffer);
            } else {
                self.render_dream_timeline(content, buffer);
            }
        } else {
            let columns =
                Layout::horizontal([Constraint::Percentage(37), Constraint::Percentage(63)])
                    .split(content);
            self.render_dream_timeline(columns[0], buffer);
            self.render_dream_details(columns[1], buffer);
        }
        let navigation = if self.details_focused {
            "Details · ↑↓/jk scroll · Esc timeline"
        } else {
            "Timeline · ↑↓/jk select run · Enter details · Esc close"
        };
        Paragraph::new(format!("{navigation}\nTab focus · / search · R refresh · r rollback · s sources · m metadata · q close"))
            .style(Style::default().fg(palette::MUTED)).render(sections[3], buffer);
    }

    fn render_dream_timeline(&mut self, area: Rect, buffer: &mut Buffer) {
        self.dream.timeline = area;
        let visible = self.visible_entries();
        let inner = region(
            area,
            format!("1 Timeline ({}/{})", visible.len(), self.entries.len()),
            !self.details_focused,
            buffer,
        );
        let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
        let search = self
            .search
            .block_title(visible.len(), self.entries.len())
            .unwrap_or_else(|| Line::from("/ Search runs"));
        Paragraph::new(search)
            .style(Style::default().fg(palette::MUTED))
            .render(sections[0], buffer);
        let rows = sections[1];
        if visible.is_empty() {
            let message = if self.entries.is_empty() {
                "No Dream history in this view.\n\nHistory starts with newly audited runs.\nOld runs are not reconstructed.\nEmpty history does not mean empty memory.\n\n/memory dream shows queue status."
            } else {
                "No matching runs. Clear or change the search."
            };
            Paragraph::new(message)
                .style(Style::default().fg(palette::MUTED))
                .wrap(Wrap { trim: false })
                .render(rows, buffer);
            self.dream.rows = Rect::default();
            return;
        }
        let capacity = usize::from((rows.height / ITEM_HEIGHT).max(1));
        let offset = self
            .dream
            .offset
            .min(self.selected)
            .max(self.selected.saturating_sub(capacity - 1));
        let lines: Vec<_> = visible
            .iter()
            .enumerate()
            .skip(offset)
            .take(capacity)
            .flat_map(|(index, entry)| {
                let presentation = self.dream.presentations.get(&entry.id);
                let style =
                    Style::default()
                        .fg(palette::TEXT)
                        .add_modifier(if index == self.selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        });
                let mut title = vec![Span::raw(format!(
                    "{} ",
                    if index == self.selected { "›" } else { " " }
                ))];
                if let Some((presentation, suffix)) = presentation.and_then(|value| {
                    entry
                        .title
                        .strip_prefix(&value.status)
                        .map(|suffix| (value, suffix))
                }) {
                    title.push(Span::styled(
                        presentation.status.clone(),
                        Style::default().fg(presentation.tone.color()),
                    ));
                    title.push(Span::raw(suffix.to_owned()));
                } else {
                    title.push(Span::raw(entry.title.clone()));
                }
                [
                    Line::default(),
                    Line::from(title).style(style),
                    Line::from(format!("  {}", entry.summary))
                        .style(Style::default().fg(palette::MUTED)),
                    Line::default(),
                ]
            })
            .collect();
        Paragraph::new(lines).render(rows, buffer);
        let selected_top = rows
            .y
            .saturating_add(((self.selected - offset) as u16).saturating_mul(ITEM_HEIGHT));
        // Selection is a persistent path marker, independent of keyboard focus.
        for y in selected_top..selected_top.saturating_add(ITEM_HEIGHT).min(rows.bottom()) {
            for x in rows.x..rows.right() {
                buffer[(x, y)].set_bg(palette::SURFACE);
            }
        }
        self.dream.offset = offset;
        self.dream.rows = Rect {
            height: rows.height.min(capacity as u16 * ITEM_HEIGHT),
            ..rows
        };
    }

    fn render_dream_details(&mut self, area: Rect, buffer: &mut Buffer) {
        self.dream.details = area;
        let inner = region(area, "2 Details".into(), self.details_focused, buffer);
        let sections = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
        let Some(entry) = self.selected_entry() else {
            Paragraph::new("Select a run to inspect its changes and source excerpts.")
                .style(Style::default().fg(palette::MUTED))
                .wrap(Wrap { trim: false })
                .render(sections[0], buffer);
            return;
        };
        let presentation = self.dream.presentations.get(&entry.id);
        let mut lines = vec![
            Line::default(),
            Line::from(entry.title.clone()).style(Style::default().add_modifier(Modifier::BOLD)),
            Line::from(entry.summary.clone()).style(Style::default().fg(palette::MUTED)),
            Line::default(),
        ];
        if let Some(presentation) = presentation {
            lines.extend(presentation.detail_lines.clone());
            if self.dream.sources_expanded {
                lines.push(Line::default());
                lines.push(Line::from("Source excerpts (not full conversations):"));
                lines.extend(dream_markdown(&presentation.sources, sections[0].width));
            }
            if self.dream.metadata_expanded {
                lines.push(Line::default());
                lines.push(Line::from("Metadata / recorded plan:"));
                lines.extend(
                    presentation
                        .metadata
                        .lines()
                        .map(|line| Line::from(line.to_owned())),
                );
            }
        } else {
            lines.extend(dream_markdown(&entry.details, sections[0].width));
        }
        lines.push(Line::default());
        lines.push(
            Line::from(if entry.rollback_allowed {
                "r · Preview whole-run rollback (conflict checked)"
            } else {
                "Rollback unavailable for this record."
            })
            .style(Style::default().fg(palette::MUTED)),
        );
        self.render_dream_text(sections[0], buffer, Text::from(lines));
        self.dream.sources = sections[1];
        self.dream.metadata = sections[2];
        for (area, expanded, label) in [
            (
                sections[1],
                self.dream.sources_expanded,
                "Source excerpts [s]",
            ),
            (
                sections[2],
                self.dream.metadata_expanded,
                "Metadata / recorded plan [m]",
            ),
        ] {
            Paragraph::new(format!("{} {label}", if expanded { "▾" } else { "▸" }))
                .style(Style::default().fg(palette::MUTED))
                .render(area, buffer);
        }
    }

    fn render_dream_text(&mut self, area: Rect, buffer: &mut Buffer, text: Text<'static>) {
        if area.is_empty() {
            self.details_limit = 0;
            return;
        }
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(palette::TEXT))
            .wrap(Wrap { trim: false });
        self.details_limit = paragraph
            .line_count(area.width)
            .saturating_sub(usize::from(area.height))
            .min(usize::from(u16::MAX)) as u16;
        self.details_scroll = self.details_scroll.min(self.details_limit);
        paragraph
            .scroll((self.details_scroll, 0))
            .render(area, buffer);
    }
}

fn dream_markdown(source: &str, width: u16) -> Vec<Line<'static>> {
    // No conversation code-block hooks: this read-only panel has no copy handlers.
    let renderer = MarkdownRenderer::new(usize::from(width.max(1)));
    renderer.render(&renderer.parse(source), &AppTheme)
}

fn region(area: Rect, title: String, focused: bool, buffer: &mut Buffer) -> Rect {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(palette::BORDER))
        .title(
            Line::from(format!(" {}{title} ", if focused { "● " } else { "" }))
                .style(Style::default().fg(if focused { role::FOCUS } else { palette::MUTED })),
        );
    let inner = block.inner(area);
    block.render(area, buffer);
    inner
}

#[cfg(test)]
mod tests {
    use super::super::tests::{entry, panel, press, render};
    use super::*;
    use crate::ui::theme::{self, Theme};
    use crossterm::event::KeyModifiers;

    fn click(panel: &mut ActivityPanel, area: Rect, row: u16) {
        panel.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y + row,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn padded_selection_and_status_survive_focus_changes_in_both_themes() {
        for theme in [Theme::Dark, Theme::Light] {
            let mut panel = panel(ActivityMode::Dream);
            panel.entries[0].title = "Applied · just now".into();
            panel.set_dream_presentations(BTreeMap::from([(
                "alpha".into(),
                DreamPresentation {
                    status: "Applied".into(),
                    tone: DreamStatusTone::Applied,
                    detail_lines: vec![Line::from("Actual change")],
                    metadata: "private-metadata-marker".into(),
                    sources: "private-source-marker".into(),
                },
            )]));
            for key in [
                KeyCode::Home,
                KeyCode::Enter,
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Esc,
            ] {
                press(&mut panel, key);
                let area = Rect::new(2, 3, 140, 30);
                let mut buffer = Buffer::empty(area);
                panel.render(area, &mut buffer);
                let rows = panel.dream.rows;
                theme::apply(theme, area, &mut buffer);
                let mut expected = Buffer::empty(Rect::new(0, 0, 1, 1));
                expected[(0, 0)]
                    .set_bg(palette::SURFACE)
                    .set_fg(palette::GREEN);
                theme::apply(theme, expected.area, &mut expected);
                for y in rows.y..rows.y + ITEM_HEIGHT {
                    for x in rows.x..rows.right() {
                        assert_eq!(buffer[(x, y)].bg, expected[(0, 0)].bg);
                    }
                }
                assert_eq!(buffer[(rows.x + 2, rows.y + 1)].fg, expected[(0, 0)].fg);
                for top in [rows.y, rows.y + ITEM_HEIGHT] {
                    for y in [top, top + ITEM_HEIGHT - 1] {
                        for x in rows.x..rows.right() {
                            assert_eq!(buffer[(x, y)].symbol(), " ");
                        }
                    }
                }
                assert_ne!(
                    buffer[(rows.x, rows.y + ITEM_HEIGHT)].bg,
                    expected[(0, 0)].bg
                );
                let screen: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                assert!(!screen.contains("private-metadata-marker"));
                assert!(!screen.contains("private-source-marker"));
            }
            press(&mut panel, KeyCode::Char('m'));
            press(&mut panel, KeyCode::Char('s'));
            let screen = render(&mut panel, 140, 40);
            assert!(screen.contains("private-metadata-marker"));
            assert!(screen.contains("private-source-marker"));
        }
    }

    fn markdown_panel() -> ActivityPanel {
        let mut panel = panel(ActivityMode::Dream);
        panel.set_dream_presentations(BTreeMap::from([(
            "alpha".into(),
            DreamPresentation {
                status: "Applied".into(),
                tone: DreamStatusTone::Applied,
                detail_lines: vec![
                    Line::styled("**Before**", Style::default().fg(palette::RED)),
                    Line::styled("**After**", Style::default().fg(palette::GREEN)),
                ],
                metadata: r##"{"raw": "**literal**", "heading": "# literal"}"##.into(),
                sources: "# Source heading\n\nA **strong** claim with `inline_code`.\n\n- first item\n- second item\n\n```rust\nlet answer = 42;\n```".into(),
            },
        )]));
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Char('s'));
        panel
    }

    fn word_cell<'a>(buffer: &'a Buffer, word: &str) -> &'a ratatui::buffer::Cell {
        for row in buffer.content.chunks(usize::from(buffer.area.width)) {
            let text: String = row.iter().map(|cell| cell.symbol()).collect();
            if let Some(index) = text.find(word) {
                return &row[text[..index].chars().count()];
            }
        }
        panic!("missing rendered word: {word}");
    }

    #[test]
    fn source_markdown_styles_prose_without_changing_typed_diff_or_raw_metadata() {
        let mut panel = markdown_panel();
        press(&mut panel, KeyCode::Char('m'));
        let area = Rect::new(0, 0, 140, 50);
        let mut buffer = Buffer::empty(area);
        panel.render(area, &mut buffer);
        for word in ["Source heading", "strong"] {
            assert!(word_cell(&buffer, word).modifier.contains(Modifier::BOLD));
        }
        assert_ne!(word_cell(&buffer, "inline_code").fg, palette::TEXT);
        assert_eq!(word_cell(&buffer, "**Before**").fg, palette::RED);
        assert_eq!(word_cell(&buffer, "**After**").fg, palette::GREEN);
        let screen = render(&mut panel, 140, 50);
        for literal in [
            "# Source heading",
            "**strong**",
            "`inline_code`",
            "- first item",
            "```",
            "⧉ copy",
        ] {
            assert!(!screen.contains(literal), "unexpected literal: {literal}");
        }
        for content in [
            "inline_code",
            "first item",
            "second item",
            "let answer = 42;",
            r##"{"raw": "**literal**", "heading": "# literal"}"##,
        ] {
            assert!(screen.contains(content), "missing content: {content}");
        }
    }

    #[test]
    fn fallback_prose_is_markdown_but_rollback_confirmation_stays_literal() {
        let mut panel = panel(ActivityMode::Dream);
        panel.entries[0].details = "# Fallback\n\n**important** and `code`\n\n- item".into();
        press(&mut panel, KeyCode::Enter);
        let screen = render(&mut panel, 65, 30);
        assert!(screen.contains("Fallback"));
        assert!(screen.contains("important and code"));
        assert!(!screen.contains("# Fallback"));
        assert!(!screen.contains("**important**"));
        panel.confirmation = Some((
            "alpha".into(),
            "# Confirm\n**literal** `code`\n- item".into(),
        ));
        let screen = render(&mut panel, 65, 30);
        assert!(screen.contains("# Confirm"));
        assert!(screen.contains("**literal** `code`"));
        assert!(screen.contains("- item"));
    }

    #[test]
    fn source_markdown_scrolls_and_reflows_at_narrow_widths() {
        let mut panel = markdown_panel();
        let source = &mut panel.dream.presentations.get_mut("alpha").unwrap().sources;
        for index in 0..25 {
            source.push_str(&format!(
                "\n\n- **Row {index}** has enough prose to wrap across a narrow viewport."
            ));
        }
        source.push_str("\n\n**ENDMARKER**");
        for width in [45, 24, 79, 120] {
            press(&mut panel, KeyCode::Home);
            render(&mut panel, width, 16);
            assert_eq!(panel.details_scroll, 0);
            assert!(panel.details_limit > 0);
            press(&mut panel, KeyCode::End);
            let screen = render(&mut panel, width, 16);
            assert_eq!(panel.details_scroll, panel.details_limit);
            assert!(screen.contains("ENDMARKER"));
            assert!(!screen.contains("**ENDMARKER**"));
        }
        for width in 0..8 {
            render(&mut panel, width, 16);
        }
        press(&mut panel, KeyCode::Char('s'));
        render(&mut panel, 45, 30);
        assert_eq!(panel.details_scroll, 0);
        assert_eq!(panel.details_limit, 0);
    }

    #[test]
    fn mouse_padding_scroll_offsets_and_narrow_hierarchy_are_consistent() {
        let mut panel = panel(ActivityMode::Dream);
        panel.entries = (0..30)
            .map(|index| entry(&format!("event-{index}")))
            .collect();
        press(&mut panel, KeyCode::End);
        render(&mut panel, 120, 24);
        let offset = panel.dream.offset;
        assert!(offset > 0);
        let rows = panel.dream.rows;
        for row in 0..ITEM_HEIGHT * 2 {
            click(&mut panel, rows, row);
            assert_eq!(panel.selected, offset + usize::from(row / ITEM_HEIGHT));
            assert!(!panel.details_focused);
        }
        let details = panel.dream.details;
        click(&mut panel, details, 0);
        assert!(panel.details_focused);
        render(&mut panel, 45, 20);
        assert!(panel.dream.rows.is_empty());
        assert!(!panel.dream.details.is_empty());
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::None);
        render(&mut panel, 45, 20);
        assert!(panel.dream.details.is_empty());
        assert!(!panel.dream.rows.is_empty());
        for width in 0..8 {
            for height in 0..8 {
                render(&mut panel, width, height);
            }
        }
        render(&mut panel, 0, 0);
        assert!(panel.dream.rows.is_empty());
        assert!(panel.dream.details.is_empty());
    }

    #[test]
    fn filtered_details_escape_ascends_before_clearing_search_and_closing() {
        let mut panel = panel(ActivityMode::Dream);
        press(&mut panel, KeyCode::Char('/'));
        press(&mut panel, KeyCode::Char('a'));
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Enter);
        assert!(panel.details_focused);
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::None);
        assert!(!panel.details_focused);
        assert!(panel.search.is_filtering());
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::None);
        assert!(!panel.search.is_filtering());
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::Close);
        panel.replace_entries(Vec::new());
        assert!(render(&mut panel, 120, 30).contains("Empty history does not mean empty memory"));
        press(&mut panel, KeyCode::Enter);
        assert!(!panel.details_focused);
    }
}
