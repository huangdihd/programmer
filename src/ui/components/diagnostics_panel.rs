// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Full-screen editor for `.programmer/diagnostics.toml`.

use crate::diagnostics::{Checker, CheckerKind, DiagnosticsProfile};
use crate::ui::interaction::InteractionAnimation;
use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};

const LABELS: [&str; 7] = [
    "name",
    "kind (command/lsp)",
    "command",
    "parser",
    "pattern",
    "run_on (comma-separated)",
    "lint (true/false)",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelAction {
    None,
    Close,
    Saved(DiagnosticsProfile),
}

#[derive(Debug)]
struct Form {
    original: Option<usize>,
    fields: [String; 7],
    focus: usize,
    error: Option<String>,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            original: None,
            fields: [
                String::new(),
                "command".to_string(),
                String::new(),
                "gnu".to_string(),
                String::new(),
                String::new(),
                "false".to_string(),
            ],
            focus: 0,
            error: None,
        }
    }
}

#[derive(Debug)]
enum Mode {
    List,
    ConfirmDelete(usize),
    Form(Box<Form>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseTarget {
    Checker(usize),
    FormField(usize),
    Add,
    Edit,
    Delete,
    ConfirmDelete,
    Cancel,
    Save,
    Close,
}

#[derive(Debug)]
pub struct DiagnosticsPanel {
    profile: DiagnosticsProfile,
    selected: usize,
    mode: Mode,
    mouse_hover: Option<MouseTarget>,
    mouse_pressed: Option<MouseTarget>,
    interaction_animation: InteractionAnimation,
}

impl DiagnosticsPanel {
    pub(crate) fn load() -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let profile = match DiagnosticsProfile::load(&cwd) {
            Some(result) => result?,
            None => DiagnosticsProfile::default(),
        };
        Ok(Self {
            profile,
            selected: 0,
            mode: Mode::List,
            mouse_hover: None,
            mouse_pressed: None,
            interaction_animation: InteractionAnimation::default(),
        })
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PanelAction {
        match &mut self.mode {
            Mode::List => self.handle_list_key(key),
            Mode::ConfirmDelete(_) => self.handle_confirm_key(key),
            Mode::Form(_) => self.handle_form_key(key),
        }
    }

    pub(crate) fn handle_paste(&mut self, data: &str) {
        if let Mode::Form(form) = &mut self.mode {
            let clean = data.replace(['\r', '\n'], " ");
            form.fields[form.focus].push_str(&clean);
        }
    }

    /// Handle pointer input using the same actions as the keyboard UI.
    /// Activation occurs only when a left-button release matches its press.
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PanelAction {
        let target = self.mouse_target(mouse.column, mouse.row, area);
        match mouse.kind {
            MouseEventKind::Moved => self.set_mouse_hover(target),
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_mouse_hover(target);
                self.mouse_pressed = target;
                self.select_mouse_target(target);
                self.interaction_animation.cancel();
            }
            MouseEventKind::Drag(MouseButton::Left) => self.set_mouse_hover(target),
            MouseEventKind::Up(MouseButton::Left) => {
                self.set_mouse_hover(target);
                let activate = self
                    .mouse_pressed
                    .take()
                    .filter(|pressed| Some(*pressed) == target);
                if activate.is_some() {
                    self.interaction_animation.restart();
                }
                return activate.map_or(PanelAction::None, |target| {
                    self.activate_mouse_target(target)
                });
            }
            MouseEventKind::ScrollUp if self.pointer_over_list(mouse.column, mouse.row, area) => {
                return self.handle_key(KeyEvent::new(KeyCode::Up, mouse.modifiers));
            }
            MouseEventKind::ScrollDown if self.pointer_over_list(mouse.column, mouse.row, area) => {
                return self.handle_key(KeyEvent::new(KeyCode::Down, mouse.modifiers));
            }
            _ => {}
        }
        PanelAction::None
    }

    pub(crate) fn interaction_animation_active(&self) -> bool {
        self.interaction_animation.active()
    }

    pub(crate) fn cancel_pointer_gesture(&mut self) {
        self.mouse_hover = None;
        self.mouse_pressed = None;
        self.interaction_animation.cancel();
    }

    fn set_mouse_hover(&mut self, target: Option<MouseTarget>) {
        if self.mouse_hover != target {
            self.mouse_hover = target;
            if target.is_some() {
                self.interaction_animation.restart();
            } else {
                self.interaction_animation.cancel();
            }
        }
    }

    fn select_mouse_target(&mut self, target: Option<MouseTarget>) {
        match target {
            Some(MouseTarget::Checker(index)) => self.selected = index,
            Some(MouseTarget::FormField(index)) => {
                if let Mode::Form(form) = &mut self.mode {
                    form.focus = index;
                }
            }
            _ => {}
        }
    }

    fn activate_mouse_target(&mut self, target: MouseTarget) -> PanelAction {
        self.select_mouse_target(Some(target));
        let code = match target {
            MouseTarget::Checker(_) | MouseTarget::Edit => KeyCode::Enter,
            MouseTarget::FormField(_) => return PanelAction::None,
            MouseTarget::Add => KeyCode::Char('a'),
            MouseTarget::Delete => KeyCode::Char('d'),
            MouseTarget::ConfirmDelete => KeyCode::Char('y'),
            MouseTarget::Cancel => KeyCode::Esc,
            MouseTarget::Save => KeyCode::Enter,
            MouseTarget::Close => KeyCode::Esc,
        };
        self.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn mouse_target(&self, column: u16, row: u16, area: Rect) -> Option<MouseTarget> {
        if !area.contains((column, row).into()) {
            return None;
        }
        let chunks = panel_chunks(area);
        let content = Block::default().borders(Borders::ALL).inner(chunks[1]);
        if content.contains((column, row).into()) {
            let content_row = usize::from(row - content.y);
            return match &self.mode {
                Mode::List | Mode::ConfirmDelete(_) => {
                    let visible = usize::from(content.height) / 2;
                    if visible == 0 {
                        return None;
                    }
                    let offset = self.selected.saturating_add(1).saturating_sub(visible);
                    let index = offset + content_row / 2;
                    (content_row / 2 < visible && index < self.profile.checkers.len())
                        .then_some(MouseTarget::Checker(index))
                }
                Mode::Form(_) => {
                    (content_row < LABELS.len()).then_some(MouseTarget::FormField(content_row))
                }
            };
        }
        if row != chunks[2].y {
            return None;
        }
        match self.mode {
            Mode::List => hit_horizontal_target(
                column,
                chunks[2].x,
                &[
                    ("↑↓ navigate  ", None),
                    ("a add  ", Some(MouseTarget::Add)),
                    ("Enter/e edit  ", Some(MouseTarget::Edit)),
                    ("d delete  ", Some(MouseTarget::Delete)),
                    ("q/Esc close", Some(MouseTarget::Close)),
                ],
            ),
            Mode::ConfirmDelete(_) => hit_horizontal_target(
                column,
                chunks[2].x,
                &[
                    ("Delete checker?  ", None),
                    ("y yes  ", Some(MouseTarget::ConfirmDelete)),
                    ("n/Esc cancel", Some(MouseTarget::Cancel)),
                ],
            ),
            Mode::Form(_) => hit_horizontal_target(
                column,
                chunks[2].x,
                &[
                    ("Tab/↑↓ field  ", None),
                    ("Enter save  ", Some(MouseTarget::Save)),
                    ("Esc cancel", Some(MouseTarget::Cancel)),
                ],
            ),
        }
    }

    fn pointer_over_list(&self, column: u16, row: u16, area: Rect) -> bool {
        matches!(self.mode, Mode::List)
            && Block::default()
                .borders(Borders::ALL)
                .inner(panel_chunks(area)[1])
                .contains((column, row).into())
    }

    fn mouse_style(&self, target: MouseTarget) -> Option<Style> {
        if self.mouse_pressed == Some(target) && self.mouse_hover == Some(target) {
            Some(
                Style::default()
                    .fg(role::SELECTION_TEXT)
                    .bg(role::INTERACTION_PRESSED_BG)
                    .bold(),
            )
        } else if self.mouse_hover == Some(target) {
            Some(
                Style::default()
                    .fg(role::SELECTION_TEXT)
                    .bg(self.interaction_animation.hover_background()),
            )
        } else {
            None
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> PanelAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return PanelAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected =
                    (self.selected + 1).min(self.profile.checkers.len().saturating_sub(1));
            }
            KeyCode::Char('a') => self.mode = Mode::Form(Box::default()),
            KeyCode::Char('e') | KeyCode::Enter => {
                if let Some(checker) = self.profile.checkers.get(self.selected) {
                    self.mode = Mode::Form(Box::new(Form {
                        original: Some(self.selected),
                        fields: checker_fields(checker),
                        focus: 0,
                        error: None,
                    }));
                }
            }
            KeyCode::Char('d') if !self.profile.checkers.is_empty() => {
                self.mode = Mode::ConfirmDelete(self.selected);
            }
            _ => {}
        }
        PanelAction::None
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> PanelAction {
        match key.code {
            KeyCode::Char('y' | 'Y') => {
                let Mode::ConfirmDelete(index) = self.mode else {
                    unreachable!();
                };
                self.profile.checkers.remove(index);
                self.selected = self
                    .selected
                    .min(self.profile.checkers.len().saturating_sub(1));
                self.mode = Mode::List;
                PanelAction::Saved(self.profile.clone())
            }
            KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                self.mode = Mode::List;
                PanelAction::None
            }
            _ => PanelAction::None,
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent) -> PanelAction {
        let Mode::Form(form) = &mut self.mode else {
            unreachable!();
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % LABELS.len(),
            KeyCode::BackTab | KeyCode::Up => {
                form.focus = (form.focus + LABELS.len() - 1) % LABELS.len();
            }
            KeyCode::Backspace => {
                form.fields[form.focus].pop();
            }
            KeyCode::Char(character) => form.fields[form.focus].push(character),
            KeyCode::Enter => return self.submit_form(),
            _ => {}
        }
        PanelAction::None
    }

    fn submit_form(&mut self) -> PanelAction {
        let Mode::Form(form) = &mut self.mode else {
            unreachable!();
        };
        let checker = match checker_from_fields(&form.fields) {
            Ok(checker) => checker,
            Err(error) => {
                form.error = Some(error);
                return PanelAction::None;
            }
        };
        let mut candidate = self.profile.clone();
        if let Some(index) = form.original {
            candidate.checkers[index] = checker;
        } else {
            candidate.checkers.push(checker);
            self.selected = candidate.checkers.len() - 1;
        }
        if let Err(error) = candidate.validate() {
            form.error = Some(error);
            return PanelAction::None;
        }
        self.profile = candidate;
        self.mode = Mode::List;
        PanelAction::Saved(self.profile.clone())
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let chunks = panel_chunks(area);
        Paragraph::new(Line::from(vec![
            Span::styled(
                "◇ Diagnostics",
                Style::default().fg(role::SECTION_DIAGNOSTICS).bold(),
            ),
            Span::styled(
                format!("  ({} checkers)", self.profile.checkers.len()),
                Style::default().fg(palette::MUTED),
            ),
        ]))
        .render(chunks[0], buf);

        match &self.mode {
            Mode::List | Mode::ConfirmDelete(_) => {
                let items = self
                    .profile
                    .checkers
                    .iter()
                    .enumerate()
                    .map(|(index, checker)| {
                        let kind = match checker.kind {
                            CheckerKind::Command => "command",
                            CheckerKind::Lsp => "lsp",
                        };
                        let lint = if checker.lint { "  lint" } else { "" };
                        let item = ListItem::new(vec![
                            Line::from(vec![
                                Span::styled(
                                    &checker.name,
                                    Style::default().fg(palette::TEXT).bold(),
                                ),
                                Span::styled(
                                    format!("  {kind}"),
                                    Style::default().fg(palette::CYAN),
                                ),
                                Span::styled(lint, Style::default().fg(palette::PURPLE)),
                            ]),
                            Line::from(vec![
                                Span::styled("  ", Style::default()),
                                Span::styled(&checker.command, Style::default().fg(palette::MUTED)),
                                Span::styled(
                                    format!("  · {}", checker.parser),
                                    Style::default().fg(palette::FAINT),
                                ),
                            ]),
                        ]);
                        if let Some(style) = self.mouse_style(MouseTarget::Checker(index)) {
                            item.style(style)
                        } else {
                            item
                        }
                    });
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(role::BORDER))
                            .title(" .programmer/diagnostics.toml "),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(role::SELECTION_TEXT)
                            .bg(role::SELECTION_BG)
                            .bold(),
                    )
                    .highlight_symbol("› ");
                let mut state = ListState::default()
                    .with_selected((!self.profile.checkers.is_empty()).then_some(self.selected));
                ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut state);

                let hint = if matches!(self.mode, Mode::ConfirmDelete(_)) {
                    Line::from(vec![
                        Span::styled("Delete checker?  ", Style::default().fg(palette::YELLOW)),
                        Span::styled(
                            "y yes  ",
                            self.mouse_style(MouseTarget::ConfirmDelete)
                                .unwrap_or_else(|| Style::default().fg(palette::GREEN).bold()),
                        ),
                        Span::styled(
                            "n/Esc cancel",
                            self.mouse_style(MouseTarget::Cancel)
                                .unwrap_or_else(|| Style::default().fg(palette::RED).bold()),
                        ),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw("↑↓ navigate  "),
                        Span::styled(
                            "a add  ",
                            self.mouse_style(MouseTarget::Add).unwrap_or_default(),
                        ),
                        Span::styled(
                            "Enter/e edit  ",
                            self.mouse_style(MouseTarget::Edit).unwrap_or_default(),
                        ),
                        Span::styled(
                            "d delete  ",
                            self.mouse_style(MouseTarget::Delete).unwrap_or_default(),
                        ),
                        Span::styled(
                            "q/Esc close",
                            self.mouse_style(MouseTarget::Close).unwrap_or_default(),
                        ),
                    ])
                };
                Paragraph::new(hint).render(chunks[2], buf);
            }
            Mode::Form(form) => {
                let mut lines = LABELS
                    .iter()
                    .enumerate()
                    .map(|(index, label)| {
                        let focused = index == form.focus;
                        let style = if focused {
                            Style::default().fg(palette::BLUE).bold()
                        } else {
                            Style::default().fg(palette::MUTED)
                        };
                        let line = Line::from(vec![
                            Span::styled(format!("{label:>26}: "), style),
                            Span::styled(&form.fields[index], Style::default().fg(palette::TEXT)),
                            Span::styled(
                                if focused { "▌" } else { "" },
                                Style::default().fg(palette::BLUE),
                            ),
                        ]);
                        if let Some(style) = self.mouse_style(MouseTarget::FormField(index)) {
                            line.style(style)
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>();
                if let Some(error) = &form.error {
                    lines.push(Line::from(Span::styled(
                        error,
                        Style::default().fg(palette::RED),
                    )));
                }
                Paragraph::new(lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(role::FOCUS))
                            .title_style(Style::default().fg(role::FOCUS).bold())
                            .title(if form.original.is_some() {
                                " Edit checker "
                            } else {
                                " Add checker "
                            }),
                    )
                    .render(chunks[1], buf);
                Paragraph::new(Line::from(vec![
                    Span::raw("Tab/↑↓ field  "),
                    Span::styled(
                        "Enter save  ",
                        self.mouse_style(MouseTarget::Save).unwrap_or_default(),
                    ),
                    Span::styled(
                        "Esc cancel",
                        self.mouse_style(MouseTarget::Cancel).unwrap_or_default(),
                    ),
                ]))
                .render(chunks[2], buf);
            }
        }
    }
}

fn hit_horizontal_target(
    column: u16,
    start: u16,
    spans: &[(&str, Option<MouseTarget>)],
) -> Option<MouseTarget> {
    let x = column.checked_sub(start)?;
    let mut offset = 0_u16;
    for (text, target) in spans {
        let width = Line::from(*text).width().min(usize::from(u16::MAX)) as u16;
        if x >= offset && x < offset.saturating_add(width) {
            return *target;
        }
        offset = offset.saturating_add(width);
    }
    None
}

fn panel_chunks(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(area)
}

fn checker_fields(checker: &Checker) -> [String; 7] {
    [
        checker.name.clone(),
        match checker.kind {
            CheckerKind::Command => "command",
            CheckerKind::Lsp => "lsp",
        }
        .to_string(),
        checker.command.clone(),
        checker.parser.clone(),
        checker.pattern.clone().unwrap_or_default(),
        checker.run_on.join(", "),
        checker.lint.to_string(),
    ]
}

fn checker_from_fields(fields: &[String; 7]) -> Result<Checker, String> {
    let name = fields[0].trim();
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    let kind = match fields[1].trim().to_ascii_lowercase().as_str() {
        "command" => CheckerKind::Command,
        "lsp" => CheckerKind::Lsp,
        _ => return Err("kind must be command or lsp".to_string()),
    };
    let lint = fields[6]
        .trim()
        .parse::<bool>()
        .map_err(|_| "lint must be true or false".to_string())?;
    Ok(Checker {
        name: name.to_string(),
        kind,
        command: fields[2].trim().to_string(),
        parser: fields[3].trim().to_string(),
        pattern: (!fields[4].trim().is_empty()).then(|| fields[4].trim().to_string()),
        run_on: fields[5]
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        lint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn checker(name: &str) -> Checker {
        Checker {
            name: name.to_string(),
            kind: CheckerKind::Command,
            command: "cargo check".to_string(),
            parser: "gnu".to_string(),
            pattern: None,
            run_on: Vec::new(),
            lint: false,
        }
    }

    fn panel_with_checkers(count: usize) -> DiagnosticsPanel {
        DiagnosticsPanel {
            profile: DiagnosticsProfile {
                checkers: (0..count)
                    .map(|index| checker(&format!("checker-{index}")))
                    .collect(),
            },
            selected: 0,
            mode: Mode::List,
            mouse_hover: None,
            mouse_pressed: None,
            interaction_animation: InteractionAnimation::default(),
        }
    }

    #[test]
    fn mouse_requires_matching_release_and_scopes_wheel_to_list() {
        let area = Rect::new(0, 0, 80, 10);
        let chunks = panel_chunks(area);
        let list = Block::default().borders(Borders::ALL).inner(chunks[1]);
        let mut panel = panel_with_checkers(3);

        assert_eq!(
            panel.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0), area),
            PanelAction::None
        );
        assert_eq!(panel.selected, 0);
        assert_eq!(
            panel.handle_mouse(mouse(MouseEventKind::ScrollDown, list.x, list.y), area),
            PanelAction::None
        );
        assert_eq!(panel.selected, 1);

        let footer = chunks[2];
        let add_x = footer.x + "↑↓ navigate  ".width() as u16;
        let edit_x = add_x + "a add  ".width() as u16;
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), add_x, footer.y),
                area
            ),
            PanelAction::None
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), edit_x, footer.y),
                area
            ),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::List));
    }

    #[test]
    fn mouse_delete_only_confirms_on_matching_button_release() {
        let area = Rect::new(0, 0, 100, 12);
        let chunks = panel_chunks(area);
        let footer = chunks[2];
        let delete_x = footer.x
            + ("↑↓ navigate  ".width() + "a add  ".width() + "Enter/e edit  ".width()) as u16;
        let mut panel = panel_with_checkers(1);

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), delete_x, footer.y),
            area,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), delete_x, footer.y),
                area
            ),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::ConfirmDelete(0)));

        let yes_x = footer.x + "Delete checker?  ".width() as u16;
        let cancel_x = yes_x + "y yes  ".width() as u16;
        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), yes_x, footer.y),
            area,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), cancel_x, footer.y),
                area
            ),
            PanelAction::None
        );
        assert_eq!(panel.profile.checkers.len(), 1);
        assert!(matches!(panel.mode, Mode::ConfirmDelete(0)));

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), yes_x, footer.y),
            area,
        );
        assert!(matches!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), yes_x, footer.y),
                area
            ),
            PanelAction::Saved(_)
        ));
        assert!(panel.profile.checkers.is_empty());
    }

    #[test]
    fn fields_parse_command_checker() {
        let fields = [
            "cargo".into(),
            "command".into(),
            "cargo check --message-format=json".into(),
            "rustc-json".into(),
            String::new(),
            "*.rs, Cargo.toml".into(),
            "false".into(),
        ];
        let checker = checker_from_fields(&fields).unwrap();
        assert_eq!(checker.run_on, ["*.rs", "Cargo.toml"]);
        assert_eq!(checker.kind, CheckerKind::Command);
    }
}
