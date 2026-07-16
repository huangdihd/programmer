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

//! Full-screen MCP (Model Context Protocol) server management panel.
//!
//! Opened with `/mcp manage` inside the app. Lets the user add, edit, and
//! delete MCP server entries. Changes are written into `config.mcp_servers`
//! and reported via [`PanelAction::Saved`] so the app can persist the config
//! and re-spawn the servers.

use crate::config::programmer_config::ProgrammerConfig;
use crate::mcp::McpManager;
use crate::mcp::types::{McpPolicy, McpServerConfig};
use crate::ui::components::panel_search::{PanelSearch, SearchKey};
use crate::ui::text::truncate_to_width;
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
    /// `config.mcp_servers` changed: persist the config and re-spawn servers.
    Saved,
}

/// Editable form fields, in focus order.
const FORM_LABELS: [&str; 6] = [
    "name",
    "command (stdio)",
    "url (remote http server)",
    "args (space-separated)",
    "env K=V (http: headers)",
    "policy (trusted|review)",
];
/// Index of the policy field, which toggles instead of taking text.
const POLICY_FIELD: usize = 5;

#[derive(Debug, Default)]
struct Form {
    /// `Some(original_name)` when editing an existing server.
    original: Option<String>,
    /// name, command, url, args, env, policy.
    fields: [String; 6],
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
pub struct McpPanel {
    mode: Mode,
    selected: usize,
    search: PanelSearch,
}

impl McpPanel {
    pub fn new() -> Self {
        McpPanel {
            mode: Mode::List,
            selected: 0,
            search: PanelSearch::default(),
        }
    }

    /// One-line invocation summary for a server: its URL for remote servers,
    /// otherwise the command line.
    fn cmdline(s: &McpServerConfig) -> String {
        match &s.url {
            Some(url) => url.clone(),
            None if s.args.is_empty() => s.command.clone(),
            None => format!("{} {}", s.command, s.args.join(" ")),
        }
    }

    /// Server names passing the current search filter (name, command line, or
    /// URL), in a stable display order (config order).
    fn filtered_names(&self, config: &ProgrammerConfig) -> Vec<String> {
        config
            .mcp_servers
            .iter()
            .filter(|s| self.search.matches(&[s.name.as_str(), &Self::cmdline(s)]))
            .map(|s| s.name.clone())
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        match &mut self.mode {
            Mode::List => self.handle_list_key(key, config),
            Mode::ConfirmDelete(_) => self.handle_confirm_key(key, config),
            Mode::Form(_) => self.handle_form_key(key, config),
        }
    }

    /// Append pasted text to the focused form field.
    pub fn handle_paste(&mut self, data: &str) {
        if let Mode::Form(form) = &mut self.mode {
            let clean: String = data.chars().filter(|c| *c != '\n' && *c != '\r').collect();
            form.fields[form.focus].push_str(&clean);
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        if let SearchKey::Consumed { changed } = self.search.handle_key(key) {
            if changed {
                self.selected = 0;
            }
            return PanelAction::None;
        }
        let names = self.filtered_names(config);
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
                    if let Some(cfg) = config.mcp_servers.iter().find(|s| &s.name == name) {
                        let policy_str = match cfg.auto_approve {
                            McpPolicy::Trusted => "trusted",
                            McpPolicy::Review => "review",
                        };
                        self.mode = Mode::Form(Form {
                            original: Some(cfg.name.clone()),
                            fields: [
                                cfg.name.clone(),
                                cfg.command.clone(),
                                cfg.url.clone().unwrap_or_default(),
                                cfg.args.join(" "),
                                cfg.env
                                    .iter()
                                    .map(|(k, v)| format!("{k}={v}"))
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                policy_str.to_string(),
                            ],
                            focus: 0,
                            error: None,
                        });
                    }
                }
            }
            KeyCode::Char('d') => {
                if let Some(name) = names.get(self.selected) {
                    self.mode = Mode::ConfirmDelete(name.clone());
                }
            }
            _ => {}
        }
        PanelAction::None
    }

    fn handle_confirm_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Mode::ConfirmDelete(name) = &self.mode {
                    let name = name.clone();
                    config.mcp_servers.retain(|s| s.name != name);
                    self.selected = self.selected.saturating_sub(1);
                    self.mode = Mode::List;
                    return PanelAction::Saved;
                }
                self.mode = Mode::List;
                PanelAction::None
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::List;
                PanelAction::None
            }
            _ => PanelAction::None,
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        let Mode::Form(form) = &mut self.mode else {
            return PanelAction::None;
        };
        // The policy field toggles with Space/Enter; other keys are ignored
        // while it's focused (it's not a free-text field).
        if form.focus == POLICY_FIELD {
            match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::List;
                }
                KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
                    form.focus = 0; // wrap to first field
                }
                KeyCode::BackTab | KeyCode::Up | KeyCode::Left => {
                    form.focus = POLICY_FIELD - 1; // prev field
                }
                KeyCode::Char(' ') | KeyCode::Enter => {
                    // Toggle: trusted ↔ review
                    form.fields[POLICY_FIELD] =
                        if form.fields[POLICY_FIELD].trim() == "review" {
                            "trusted".to_string()
                        } else {
                            "review".to_string()
                        };
                }
                _ => {}
            }
            return PanelAction::None;
        }

        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
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
                return self.submit_form(config);
            }
            _ => {}
        }
        PanelAction::None
    }

    /// Validate and commit the form into `config.mcp_servers`.
    fn submit_form(&mut self, config: &mut ProgrammerConfig) -> PanelAction {
        let Mode::Form(form) = &mut self.mode else {
            return PanelAction::None;
        };
        let name = form.fields[0].trim().to_string();
        let command = form.fields[1].trim().to_string();
        let url = form.fields[2].trim().to_string();
        if name.is_empty() {
            form.error = Some("name is required".to_string());
            return PanelAction::None;
        }
        if command.is_empty() && url.is_empty() {
            form.error = Some("either command (stdio) or url (http) is required".to_string());
            return PanelAction::None;
        }
        if !url.is_empty() && !(url.starts_with("http://") || url.starts_with("https://")) {
            form.error = Some("url must start with http:// or https://".to_string());
            return PanelAction::None;
        }
        // A rename/add must not collide with a different existing server.
        let collides = config
            .mcp_servers
            .iter()
            .any(|s| s.name == name && form.original.as_deref() != Some(s.name.as_str()));
        if collides {
            form.error = Some(format!("a server named '{name}' already exists"));
            return PanelAction::None;
        }

        let args: Vec<String> = form.fields[3]
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let env: std::collections::HashMap<String, String> = form.fields[4]
            .split_whitespace()
            .filter_map(|pair| pair.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();
        let auto_approve = match form.fields[POLICY_FIELD].trim() {
            "review" => McpPolicy::Review,
            _ => McpPolicy::Trusted,
        };

        let new_cfg = McpServerConfig {
            name: name.clone(),
            command,
            args,
            env,
            url: (!url.is_empty()).then_some(url),
            auto_approve,
        };

        match &form.original {
            Some(orig) => {
                let orig = orig.clone();
                if let Some(slot) = config.mcp_servers.iter_mut().find(|s| s.name == orig) {
                    *slot = new_cfg;
                }
            }
            None => config.mcp_servers.push(new_cfg),
        }
        self.mode = Mode::List;
        PanelAction::Saved
    }

    pub fn render(
        &self,
        config: &ProgrammerConfig,
        mcp: Option<&McpManager>,
        area: Rect,
        buf: &mut Buffer,
    ) {
        Clear.render(area, buf);
        let filtered: Vec<&McpServerConfig> = config
            .mcp_servers
            .iter()
            .filter(|s| self.search.matches(&[s.name.as_str(), &Self::cmdline(s)]))
            .collect();
        // In list mode, connected servers get a stderr log pane for the
        // selected entry (useful when a server misbehaves).
        let show_logs = matches!(self.mode, Mode::List)
            && !filtered.is_empty()
            && mcp.is_some()
            && area.height > 16;
        let constraints: Vec<Constraint> = if show_logs {
            vec![
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(8),
                Constraint::Length(2),
            ]
        } else {
            vec![
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ]
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);
        let bottom = chunks[chunks.len() - 1];

        // -- Title --
        Paragraph::new(Line::from(vec![
            Span::styled("🔗  MCP servers", Style::default().fg(Color::Cyan).bold()),
            Span::styled(
                format!("  ({} configured)", config.mcp_servers.len()),
                Style::default().fg(Color::Gray).italic(),
            ),
        ]))
        .render(chunks[0], buf);

        let mut list_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        if let Some(title) = self
            .search
            .block_title(filtered.len(), config.mcp_servers.len())
        {
            list_block = list_block.title(title);
        }

        // -- Server list --
        if filtered.is_empty() {
            let message = if config.mcp_servers.is_empty() {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  No MCP servers configured. Press 'a' to add one.",
                        Style::default().fg(Color::Gray),
                    )),
                    Line::from(Span::styled(
                        "  Example — filesystem: command 'npx', \
                         args '-y @modelcontextprotocol/server-filesystem /path'",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  No MCP servers match the search.",
                        Style::default().fg(Color::Gray),
                    )),
                ]
            };
            Paragraph::new(message).block(list_block).render(chunks[1], buf);
        } else {
            let items: Vec<ListItem> = filtered
                .iter()
                .map(|s| {
                    // Runtime status: connected + tool count, if the manager is up.
                    let tool_count = mcp
                        .map(|m| {
                            m.all_tools()
                                .iter()
                                .filter(|(fqn, _)| {
                                    fqn.strip_prefix("mcp__")
                                        .and_then(|r| r.split_once("__"))
                                        .map(|(srv, _)| srv == s.name)
                                        .unwrap_or(false)
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    let (status, status_style) = if mcp.is_none() {
                        ("(restart to apply)", Style::default().fg(Color::DarkGray))
                    } else if tool_count > 0 {
                        ("● connected", Style::default().fg(Color::Green))
                    } else {
                        ("○ not connected", Style::default().fg(Color::Red))
                    };

                    let first = Line::from(vec![
                        Span::styled(s.name.clone(), Style::default().fg(Color::White).bold()),
                        Span::raw("  "),
                        Span::styled(status, status_style),
                        Span::styled(
                            format!("  · {tool_count} tools"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]);
                    let second = Line::from(Span::styled(
                        format!("  {}", truncate_to_width(&Self::cmdline(s), 90)),
                        Style::default().fg(Color::Gray),
                    ));
                    ListItem::new(vec![first, second])
                })
                .collect();
            let list = List::new(items)
                .block(list_block)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("❯ ");
            let mut list_state = ListState::default();
            list_state.select(Some(self.selected.min(filtered.len() - 1)));
            ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut list_state);
        }

        // -- Selected server's recent stderr --
        if show_logs {
            let selected_name = filtered
                .get(self.selected.min(filtered.len() - 1))
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let stderr = mcp
                .and_then(|m| m.server_stderr(&selected_name))
                .unwrap_or_default();
            let log_area = chunks[2];
            let visible = log_area.height.saturating_sub(2) as usize;
            let log_lines: Vec<Line> = if stderr.is_empty() {
                vec![Line::from(Span::styled(
                    "  (no stderr output)",
                    Style::default().fg(Color::DarkGray).italic(),
                ))]
            } else {
                stderr
                    .iter()
                    .rev()
                    .take(visible.max(1))
                    .rev()
                    .map(|l| {
                        Line::from(Span::styled(
                            truncate_to_width(l, (area.width.saturating_sub(4) as usize).max(8)),
                            Style::default().fg(Color::Gray),
                        ))
                    })
                    .collect()
            };
            Paragraph::new(log_lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray))
                        .title(format!(" {selected_name} · stderr ")),
                )
                .render(log_area, buf);
        }

        // -- Bottom bar: help, confirmation, or form --
        match &self.mode {
            Mode::List => {
                let mut help = vec![
                    Span::styled(" ↑↓", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" navigate  ", Style::default().fg(Color::Gray)),
                    Span::styled("a", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" add  ", Style::default().fg(Color::Gray)),
                    Span::styled("e", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" edit  ", Style::default().fg(Color::Gray)),
                ];
                help.extend(PanelSearch::help_spans());
                help.extend([
                    Span::styled("d", Style::default().fg(Color::Red).bold()),
                    Span::styled(" delete  ", Style::default().fg(Color::Gray)),
                    Span::styled("q/Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" close", Style::default().fg(Color::Gray)),
                ]);
                Paragraph::new(Line::from(help)).render(bottom, buf);
            }
            Mode::ConfirmDelete(name) => {
                let confirm = Line::from(vec![
                    Span::styled(
                        format!(" Delete MCP server '{name}'?  "),
                        Style::default().fg(Color::Yellow).bold(),
                    ),
                    Span::styled("y", Style::default().fg(Color::Green).bold()),
                    Span::styled(" yes  ", Style::default().fg(Color::Gray)),
                    Span::styled("n", Style::default().fg(Color::Red).bold()),
                    Span::styled(" cancel", Style::default().fg(Color::Gray)),
                ]);
                Paragraph::new(confirm).render(bottom, buf);
            }
            Mode::Form(form) => {
                let title = if form.original.is_some() {
                    " Edit MCP server "
                } else {
                    " Add MCP server "
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
                        let value = if form.fields[i].is_empty() {
                            Span::styled(
                                "(empty)",
                                Style::default().fg(Color::DarkGray).italic(),
                            )
                        } else {
                            Span::styled(
                                form.fields[i].clone(),
                                Style::default().fg(Color::White),
                            )
                        };
                        let cursor = if focused { "▏" } else { "" };
                        Line::from(vec![
                            Span::styled(format!("{marker}{label:>22}: "), label_style),
                            value,
                            Span::styled(cursor, Style::default().fg(Color::Cyan)),
                        ])
                    })
                    .collect();
                if let Some(err) = &form.error {
                    lines.push(Line::from(Span::styled(
                        format!("  ⚠ {err}"),
                        Style::default().fg(Color::Red),
                    )));
                }
                lines.push(Line::from(vec![
                    Span::styled("  Tab/↑↓", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" next field  ", Style::default().fg(Color::Gray)),
                    Span::styled("Space", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" toggle policy  ", Style::default().fg(Color::Gray)),
                    Span::styled("Enter", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" save  ", Style::default().fg(Color::Gray)),
                    Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" cancel", Style::default().fg(Color::Gray)),
                ]));

                // Float the form up from the bottom, sized to its content —
                // the fixed 2-row help slot can't hold every field.
                let height = (lines.len() as u16 + 2).min(area.height);
                let form_area = Rect {
                    x: area.x,
                    y: area.bottom().saturating_sub(height),
                    width: area.width,
                    height,
                };
                Clear.render(form_area, buf);
                Paragraph::new(lines.as_slice())
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

/// Truncate a command line for single-line display.

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    #[test]
    fn add_server_via_form() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        assert_eq!(panel.handle_key(ch('a'), &mut config), PanelAction::None);
        // name
        for c in "fs".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config);
        // command
        for c in "npx".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config); // url (left empty)
        panel.handle_key(key(KeyCode::Tab), &mut config);
        // args
        for c in "-y server".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::Saved);
        assert_eq!(config.mcp_servers.len(), 1);
        assert_eq!(config.mcp_servers[0].name, "fs");
        assert_eq!(config.mcp_servers[0].command, "npx");
        assert_eq!(config.mcp_servers[0].args, vec!["-y", "server"]);
        assert!(config.mcp_servers[0].url.is_none());
    }

    #[test]
    fn add_http_server_via_form_url_only() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        panel.handle_key(ch('a'), &mut config);
        for c in "exa".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config); // command (left empty)
        panel.handle_key(key(KeyCode::Tab), &mut config); // url
        for c in "https://mcp.exa.ai/mcp".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::Saved);
        assert_eq!(config.mcp_servers[0].url.as_deref(), Some("https://mcp.exa.ai/mcp"));
        assert!(config.mcp_servers[0].command.is_empty());
    }

    #[test]
    fn form_rejects_non_http_url() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        panel.handle_key(ch('a'), &mut config);
        for c in "bad".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config); // command
        panel.handle_key(key(KeyCode::Tab), &mut config); // url
        for c in "ftp://x".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::None);
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn form_requires_name_and_command() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        panel.handle_key(ch('a'), &mut config);
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::None);
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn delete_server() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        config.mcp_servers.push(McpServerConfig {
            name: "fs".into(),
            command: "npx".into(),
            args: vec![],
            env: Default::default(),
            url: None,
            auto_approve: Default::default(),
        });
        panel.handle_key(ch('d'), &mut config);
        assert_eq!(panel.handle_key(ch('y'), &mut config), PanelAction::Saved);
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn env_parsed_into_map() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        panel.handle_key(ch('a'), &mut config);
        for c in "srv".chars() { panel.handle_key(ch(c), &mut config); }
        panel.handle_key(key(KeyCode::Tab), &mut config);
        for c in "cmd".chars() { panel.handle_key(ch(c), &mut config); }
        panel.handle_key(key(KeyCode::Tab), &mut config); // url
        panel.handle_key(key(KeyCode::Tab), &mut config); // args
        panel.handle_key(key(KeyCode::Tab), &mut config); // env
        for c in "API_KEY=secret".chars() { panel.handle_key(ch(c), &mut config); }
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::Saved);
        assert_eq!(config.mcp_servers[0].env.get("API_KEY").unwrap(), "secret");
    }

    #[test]
    fn policy_toggle() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        panel.handle_key(ch('a'), &mut config);
        // name
        for c in "srv".chars() { panel.handle_key(ch(c), &mut config); }
        panel.handle_key(key(KeyCode::Tab), &mut config);
        // command
        for c in "cmd".chars() { panel.handle_key(ch(c), &mut config); }
        panel.handle_key(key(KeyCode::Tab), &mut config); // url
        panel.handle_key(key(KeyCode::Tab), &mut config); // args
        panel.handle_key(key(KeyCode::Tab), &mut config); // env
        panel.handle_key(key(KeyCode::Tab), &mut config); // policy (default: trusted)
        // Toggle to review.
        panel.handle_key(ch(' '), &mut config);
        // Tab wraps to name, then Enter saves.
        panel.handle_key(key(KeyCode::Tab), &mut config);
        assert_eq!(panel.handle_key(key(KeyCode::Enter), &mut config), PanelAction::Saved);
        assert_eq!(config.mcp_servers[0].name, "srv");
        assert!(matches!(
            config.mcp_servers[0].auto_approve,
            McpPolicy::Review
        ));
    }
}
