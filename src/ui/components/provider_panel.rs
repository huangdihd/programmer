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

//! Full-screen provider management panel.
//!
//! Opened with `--providers` on the command line or `/providers manage` inside
//! the app. Lets the user add, edit, and delete providers and pick the default
//! one. Every change is reported to the caller via [`PanelAction::Saved`] so
//! the app can persist the config and rebuild the provider manager.

use crate::config::programmer_config::{ProgrammerConfig, ProviderConfig};
use crate::providers::ProviderManager;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};

/// What the app should do after the panel handled a key.
#[derive(Debug, PartialEq)]
pub enum PanelAction {
    /// Nothing to do; the panel only updated its own state.
    None,
    /// Close the panel.
    Close,
    /// The config was modified: persist it and rebuild the provider manager.
    Saved,
}

/// Editable fields of the add/edit form, in focus order.
const FORM_LABELS: [&str; 4] = ["name", "base_url", "api_key", "default_model"];

#[derive(Debug, Default)]
struct Form {
    /// `Some(original_name)` when editing an existing provider.
    original: Option<String>,
    /// name, base_url, api_key, default_model.
    fields: [String; 4],
    focus: usize,
    error: Option<String>,
}

#[derive(Debug)]
enum Mode {
    List,
    ConfirmDelete(String),
    Form(Form),
}

#[derive(Debug)]
pub struct ProviderPanel {
    mode: Mode,
    selected: usize,
}

impl ProviderPanel {
    pub fn new() -> Self {
        ProviderPanel {
            mode: Mode::List,
            selected: 0,
        }
    }

    /// Provider names in a stable display order.
    fn sorted_names(config: &ProgrammerConfig) -> Vec<String> {
        let mut names: Vec<String> = config.providers.keys().cloned().collect();
        names.sort();
        names
    }

    /// Handle a key event, possibly mutating `config`.
    pub fn handle_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        match &mut self.mode {
            Mode::List => self.handle_list_key(key, config),
            Mode::ConfirmDelete(_) => self.handle_confirm_key(key, config),
            Mode::Form(_) => self.handle_form_key(key, config),
        }
    }

    /// Append pasted text to the focused form field (e.g. pasting an API key).
    pub fn handle_paste(&mut self, data: &str) {
        if let Mode::Form(form) = &mut self.mode {
            // Config values are single-line; strip newlines from the paste.
            let clean: String = data.chars().filter(|c| *c != '\n' && *c != '\r').collect();
            form.fields[form.focus].push_str(&clean);
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        let names = Self::sorted_names(config);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return PanelAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < names.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Char('a') => {
                self.mode = Mode::Form(Form::default());
            }
            KeyCode::Char('e') => {
                if let Some(name) = names.get(self.selected) {
                    let p = &config.providers[name];
                    self.mode = Mode::Form(Form {
                        original: Some(name.clone()),
                        fields: [
                            name.clone(),
                            p.base_url.clone(),
                            p.api_key.clone(),
                            p.default_model.clone().unwrap_or_default(),
                        ],
                        focus: 0,
                        error: None,
                    });
                }
            }
            KeyCode::Char('d') => {
                if let Some(name) = names.get(self.selected) {
                    self.mode = Mode::ConfirmDelete(name.clone());
                }
            }
            KeyCode::Enter => {
                if let Some(name) = names.get(self.selected) {
                    if config.default_provider != *name {
                        config.default_provider = name.clone();
                        return PanelAction::Saved;
                    }
                }
            }
            _ => {}
        }
        PanelAction::None
    }

    fn handle_confirm_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        let Mode::ConfirmDelete(name) = &self.mode else {
            unreachable!("handle_confirm_key called outside ConfirmDelete mode");
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let name = name.clone();
                config.providers.remove(&name);
                // Keep default_provider pointing at something that exists.
                if config.default_provider == name {
                    config.default_provider = Self::sorted_names(config)
                        .first()
                        .cloned()
                        .unwrap_or_default();
                }
                let count = config.providers.len();
                self.selected = self.selected.min(count.saturating_sub(1));
                self.mode = Mode::List;
                PanelAction::Saved
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('d') => {
                self.mode = Mode::List;
                PanelAction::None
            }
            _ => PanelAction::None,
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        let Mode::Form(form) = &mut self.mode else {
            unreachable!("handle_form_key called outside Form mode");
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                return PanelAction::None;
            }
            KeyCode::Tab | KeyCode::Down => {
                form.focus = (form.focus + 1) % FORM_LABELS.len();
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.focus = (form.focus + FORM_LABELS.len() - 1) % FORM_LABELS.len();
            }
            KeyCode::Backspace => {
                form.fields[form.focus].pop();
            }
            KeyCode::Char(c) => {
                form.fields[form.focus].push(c);
            }
            KeyCode::Enter => {
                let [name, base_url, api_key, default_model] =
                    form.fields.clone().map(|f| f.trim().to_string());
                if name.is_empty() || base_url.is_empty() || api_key.is_empty() {
                    form.error = Some("name, base_url and api_key are required".to_string());
                    return PanelAction::None;
                }
                // Renaming must not silently overwrite another provider.
                if form.original.as_deref() != Some(name.as_str())
                    && config.providers.contains_key(&name)
                {
                    form.error = Some(format!("provider '{name}' already exists"));
                    return PanelAction::None;
                }
                if let Some(original) = &form.original {
                    if *original != name {
                        config.providers.remove(original);
                        if config.default_provider == *original {
                            config.default_provider = name.clone();
                        }
                    }
                }
                config.providers.insert(
                    name.clone(),
                    ProviderConfig {
                        base_url,
                        api_key,
                        models: None,
                        default_model: (!default_model.is_empty()).then_some(default_model),
                    },
                );
                // First provider ever: make it the default.
                if config.default_provider.is_empty() {
                    config.default_provider = name.clone();
                }
                self.selected = Self::sorted_names(config)
                    .iter()
                    .position(|n| *n == name)
                    .unwrap_or(0);
                self.mode = Mode::List;
                return PanelAction::Saved;
            }
            _ => {}
        }
        PanelAction::None
    }

    /// Render the panel over the full app area.
    pub fn render(
        &self,
        config: &ProgrammerConfig,
        pm: &ProviderManager,
        area: Rect,
        buf: &mut Buffer,
    ) {
        Clear.render(area, buf);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(area);

        // -- Title --
        let names = Self::sorted_names(config);
        Paragraph::new(Line::from(vec![
            Span::styled("🔌  Providers", Style::default().fg(Color::Cyan).bold()),
            Span::styled(
                format!("  ({} configured)", names.len()),
                Style::default().fg(Color::Gray).italic(),
            ),
        ]))
        .render(chunks[0], buf);

        // -- Provider list --
        let items: Vec<ListItem> = names
            .iter()
            .map(|name| {
                let p = &config.providers[name];
                let is_default = config.default_provider == *name;
                let mut first = vec![Span::styled(
                    name.clone(),
                    Style::default().fg(Color::White).bold(),
                )];
                if is_default {
                    first.push(Span::styled(
                        "  [default]",
                        Style::default().fg(Color::Green),
                    ));
                }
                let model_count = pm.models_for(name).len();
                let second = Line::from(vec![
                    Span::styled(
                        format!("  {}", p.base_url),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::styled(
                        format!(
                            " · default_model: {}",
                            p.default_model.as_deref().unwrap_or("(first available)")
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!(" · {model_count} models"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                ListItem::new(vec![Line::from(first), second])
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("❯ ");
        let mut list_state = ListState::default();
        if !names.is_empty() {
            list_state.select(Some(self.selected.min(names.len() - 1)));
        }
        ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut list_state);

        // -- Bottom bar: help, confirmation, or the add/edit form --
        match &self.mode {
            Mode::List => {
                let help = Line::from(vec![
                    Span::styled(" ↑↓", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" navigate  ", Style::default().fg(Color::Gray)),
                    Span::styled("Enter", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" set default  ", Style::default().fg(Color::Gray)),
                    Span::styled("a", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" add  ", Style::default().fg(Color::Gray)),
                    Span::styled("e", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" edit  ", Style::default().fg(Color::Gray)),
                    Span::styled("d", Style::default().fg(Color::Red).bold()),
                    Span::styled(" delete  ", Style::default().fg(Color::Gray)),
                    Span::styled("q/Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" close", Style::default().fg(Color::Gray)),
                ]);
                Paragraph::new(help).render(chunks[2], buf);
            }
            Mode::ConfirmDelete(name) => {
                let confirm = Line::from(vec![
                    Span::styled(
                        format!(" Delete provider '{name}'?  "),
                        Style::default().fg(Color::Yellow).bold(),
                    ),
                    Span::styled("y", Style::default().fg(Color::Green).bold()),
                    Span::styled(" yes  ", Style::default().fg(Color::Gray)),
                    Span::styled("n", Style::default().fg(Color::Red).bold()),
                    Span::styled(" cancel", Style::default().fg(Color::Gray)),
                ]);
                Paragraph::new(confirm).render(chunks[2], buf);
            }
            Mode::Form(form) => {
                // The form replaces the list area with a bordered editor.
                let title = if form.original.is_some() {
                    " Edit provider "
                } else {
                    " Add provider "
                };
                let mut lines: Vec<Line> = FORM_LABELS
                    .iter()
                    .enumerate()
                    .map(|(i, label)| {
                        let focused = i == form.focus;
                        let marker = if focused { "❯ " } else { "  " };
                        let label_style = if focused {
                            Style::default().fg(Color::Cyan).bold()
                        } else {
                            Style::default().fg(Color::Gray)
                        };
                        let value = &form.fields[i];
                        let cursor = if focused { "▏" } else { "" };
                        Line::from(vec![
                            Span::styled(format!("{marker}{label:>14}: "), label_style),
                            Span::styled(value.clone(), Style::default().fg(Color::White)),
                            Span::styled(cursor, Style::default().fg(Color::Cyan)),
                        ])
                    })
                    .collect();
                if let Some(err) = &form.error {
                    lines.push(Line::from(Span::styled(
                        format!("  {err}"),
                        Style::default().fg(Color::Red),
                    )));
                }
                lines.push(Line::from(vec![
                    Span::styled("  Tab/↑↓", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" next field  ", Style::default().fg(Color::Gray)),
                    Span::styled("Enter", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" save  ", Style::default().fg(Color::Gray)),
                    Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" cancel", Style::default().fg(Color::Gray)),
                ]));

                let height = (lines.len() as u16 + 2).min(area.height);
                let form_area = Rect {
                    x: area.x,
                    y: area.bottom().saturating_sub(height),
                    width: area.width,
                    height,
                };
                Clear.render(form_area, buf);
                Paragraph::new(lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan))
                            .title(title),
                    )
                    .render(form_area, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use std::collections::HashMap;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn config_with(names: &[&str]) -> ProgrammerConfig {
        let mut providers = HashMap::new();
        for n in names {
            providers.insert(
                n.to_string(),
                ProviderConfig {
                    base_url: format!("https://{n}.example.com/v1"),
                    api_key: "k".into(),
                    models: None,
                    default_model: None,
                },
            );
        }
        ProgrammerConfig {
            default_provider: names.first().unwrap_or(&"").to_string(),
            providers,
            model: None,
            base_url: None,
            api_key: None,
        }
    }

    #[test]
    fn add_provider_via_form() {
        let mut config = config_with(&[]);
        let mut panel = ProviderPanel::new();

        assert_eq!(panel.handle_key(key(KeyCode::Char('a')), &mut config), PanelAction::None);
        // Type into name field, then move through the fields.
        for c in "zai".chars() {
            panel.handle_key(key(KeyCode::Char(c)), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config);
        for c in "https://api.z.ai/v1".chars() {
            panel.handle_key(key(KeyCode::Char(c)), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config);
        panel.handle_paste("sk-secret");
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::Saved);

        let p = &config.providers["zai"];
        assert_eq!(p.base_url, "https://api.z.ai/v1");
        assert_eq!(p.api_key, "sk-secret");
        assert_eq!(p.default_model, None);
        assert_eq!(config.default_provider, "zai", "first provider becomes default");
    }

    #[test]
    fn form_requires_mandatory_fields() {
        let mut config = config_with(&[]);
        let mut panel = ProviderPanel::new();
        panel.handle_key(key(KeyCode::Char('a')), &mut config);
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::None);
        assert!(matches!(&panel.mode, Mode::Form(f) if f.error.is_some()));
        assert!(config.providers.is_empty());
    }

    #[test]
    fn delete_provider_reassigns_default() {
        let mut config = config_with(&["alpha", "beta"]);
        config.default_provider = "alpha".into();
        let mut panel = ProviderPanel::new();

        // "alpha" sorts first and is selected; delete it and confirm.
        panel.handle_key(key(KeyCode::Char('d')), &mut config);
        assert_eq!(panel.handle_key(key(KeyCode::Char('y')), &mut config), PanelAction::Saved);
        assert!(!config.providers.contains_key("alpha"));
        assert_eq!(config.default_provider, "beta");
    }

    #[test]
    fn enter_sets_default_provider() {
        let mut config = config_with(&["alpha", "beta"]);
        config.default_provider = "alpha".into();
        let mut panel = ProviderPanel::new();
        panel.handle_key(key(KeyCode::Down), &mut config);
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::Saved);
        assert_eq!(config.default_provider, "beta");
    }

    #[test]
    fn rename_does_not_clobber_existing_provider() {
        let mut config = config_with(&["alpha", "beta"]);
        let mut panel = ProviderPanel::new();
        // Edit "alpha", rename it to "beta" — must be rejected.
        panel.handle_key(key(KeyCode::Char('e')), &mut config);
        if let Mode::Form(form) = &mut panel.mode {
            form.fields[0] = "beta".into();
        }
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::None);
        assert!(config.providers.contains_key("alpha"));
        assert!(matches!(&panel.mode, Mode::Form(f) if f.error.is_some()));
    }
}
