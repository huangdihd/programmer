// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Full-screen editor for `.programmer/diagnostics.toml`.

use crate::diagnostics::{Checker, CheckerKind, DiagnosticsProfile};
use crate::ui::markdown_theme::palette;
use crossterm::event::{KeyCode, KeyEvent};
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

#[derive(Debug)]
pub struct DiagnosticsPanel {
    profile: DiagnosticsProfile,
    selected: usize,
    mode: Mode,
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(area);
        Paragraph::new(Line::from(vec![
            Span::styled("◇ Diagnostics", Style::default().fg(palette::BLUE).bold()),
            Span::styled(
                format!("  ({} checkers)", self.profile.checkers.len()),
                Style::default().fg(palette::MUTED),
            ),
        ]))
        .render(chunks[0], buf);

        match &self.mode {
            Mode::List | Mode::ConfirmDelete(_) => {
                let items = self.profile.checkers.iter().map(|checker| {
                    let kind = match checker.kind {
                        CheckerKind::Command => "command",
                        CheckerKind::Lsp => "lsp",
                    };
                    let lint = if checker.lint { "  lint" } else { "" };
                    ListItem::new(vec![
                        Line::from(vec![
                            Span::styled(&checker.name, Style::default().fg(palette::TEXT).bold()),
                            Span::styled(format!("  {kind}"), Style::default().fg(palette::CYAN)),
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
                    ])
                });
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(palette::BORDER))
                            .title(" .programmer/diagnostics.toml "),
                    )
                    .highlight_style(Style::default().bg(palette::SURFACE))
                    .highlight_symbol("› ");
                let mut state = ListState::default()
                    .with_selected((!self.profile.checkers.is_empty()).then_some(self.selected));
                ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut state);

                let hint = if matches!(self.mode, Mode::ConfirmDelete(_)) {
                    Line::from(vec![
                        Span::styled("Delete checker?  ", Style::default().fg(palette::YELLOW)),
                        Span::styled("y", Style::default().fg(palette::GREEN).bold()),
                        Span::styled(" yes  ", Style::default().fg(palette::MUTED)),
                        Span::styled("n/Esc", Style::default().fg(palette::RED).bold()),
                        Span::styled(" cancel", Style::default().fg(palette::MUTED)),
                    ])
                } else {
                    Line::from("↑↓ navigate  a add  Enter/e edit  d delete  q/Esc close")
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
                        Line::from(vec![
                            Span::styled(format!("{label:>26}: "), style),
                            Span::styled(&form.fields[index], Style::default().fg(palette::TEXT)),
                            Span::styled(
                                if focused { "▌" } else { "" },
                                Style::default().fg(palette::BLUE),
                            ),
                        ])
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
                            .border_style(Style::default().fg(palette::BORDER))
                            .title(if form.original.is_some() {
                                " Edit checker "
                            } else {
                                " Add checker "
                            }),
                    )
                    .render(chunks[1], buf);
                Paragraph::new("Tab/↑↓ field  Enter save  Esc cancel").render(chunks[2], buf);
            }
        }
    }
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
