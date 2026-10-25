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
use crate::mcp::types::McpServerConfig;
use crate::ui::components::panel_search::{PanelSearch, SearchKey};
use crate::ui::interaction::InteractionAnimation;
use crate::ui::markdown_theme::palette;
use crate::ui::text::truncate_to_width;
use crate::ui::theme::role;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};
use std::cell::{Cell, RefCell};
use unicode_width::UnicodeWidthStr;

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
const FORM_LABELS: [&str; 5] = [
    "name",
    "command (stdio)",
    "url (remote http server)",
    "args (space-separated)",
    "env K=V (http: headers)",
];

#[derive(Debug, Default)]
struct Form {
    /// `Some(original_name)` when editing an existing server.
    original: Option<String>,
    /// name, command, url, args, env.
    fields: [String; 5],
    focus: usize,
    error: Option<String>,
}

#[derive(Debug)]
enum Mode {
    List,
    ConfirmDelete(String),
    Form(Form),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseTarget {
    Server(usize),
    Add,
    Edit,
    Search,
    Delete,
    Close,
    ConfirmDelete(bool),
    FormField(usize),
    FormSave,
    FormCancel,
}

#[derive(Debug, Clone, Copy)]
struct HitRegion {
    area: Rect,
    target: MouseTarget,
}

#[derive(Debug)]
pub struct McpPanel {
    mode: Mode,
    selected: usize,
    search: PanelSearch,
    hit_regions: RefCell<Vec<HitRegion>>,
    list_area: Cell<Option<Rect>>,
    hovered: Cell<Option<MouseTarget>>,
    pressed: Cell<Option<MouseTarget>>,
    interaction_animation: InteractionAnimation,
}

impl McpPanel {
    pub fn new() -> Self {
        McpPanel {
            mode: Mode::List,
            selected: 0,
            search: PanelSearch::default(),
            hit_regions: RefCell::new(Vec::new()),
            list_area: Cell::new(None),
            hovered: Cell::new(None),
            pressed: Cell::new(None),
            interaction_animation: InteractionAnimation::default(),
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

    /// Handle a pointer event using hit regions from the most recent render.
    pub fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        config: &mut ProgrammerConfig,
    ) -> PanelAction {
        let target = {
            self.hit_regions
                .borrow()
                .iter()
                .rev()
                .find(|hit| hit.area.contains((mouse.column, mouse.row).into()))
                .map(|hit| hit.target)
        };
        match mouse.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
                self.set_mouse_hover(target);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_mouse_hover(target);
                self.pressed.set(target);
                self.select_mouse_target(target);
                self.interaction_animation.cancel();
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.set_mouse_hover(target);
                let activate = self
                    .pressed
                    .replace(None)
                    .filter(|pressed| Some(*pressed) == target);
                if let Some(target) = activate {
                    self.interaction_animation.restart();
                    return self.activate_mouse_target(target, config);
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if matches!(self.mode, Mode::List) && self.pointer_over_list(mouse) =>
            {
                let code = if mouse.kind == MouseEventKind::ScrollUp {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                return self.handle_key(KeyEvent::new(code, mouse.modifiers), config);
            }
            _ => {}
        }
        PanelAction::None
    }

    /// Whether pointer feedback still needs animation frames.
    pub fn interaction_animation_active(&self) -> bool {
        self.interaction_animation.active()
    }

    /// Drop hover and press capture when the terminal loses focus.
    pub fn cancel_pointer_gesture(&mut self) {
        self.hovered.set(None);
        self.pressed.set(None);
        self.interaction_animation.cancel();
    }

    fn pointer_over_list(&self, mouse: MouseEvent) -> bool {
        self.list_area
            .get()
            .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
    }

    fn set_mouse_hover(&mut self, target: Option<MouseTarget>) {
        if self.hovered.get() != target {
            self.hovered.set(target);
            if target.is_some() {
                self.interaction_animation.restart();
            } else {
                self.interaction_animation.cancel();
            }
        }
    }

    fn select_mouse_target(&mut self, target: Option<MouseTarget>) {
        match target {
            Some(MouseTarget::Server(index)) => self.selected = index,
            Some(MouseTarget::FormField(index)) => {
                if let Mode::Form(form) = &mut self.mode {
                    form.focus = index;
                }
            }
            _ => {}
        }
    }

    fn activate_mouse_target(
        &mut self,
        target: MouseTarget,
        config: &mut ProgrammerConfig,
    ) -> PanelAction {
        self.select_mouse_target(Some(target));
        if matches!(self.mode, Mode::List) {
            match target {
                MouseTarget::Search => {
                    self.search.focus();
                    return PanelAction::None;
                }
                MouseTarget::Close => return PanelAction::Close,
                _ => self.search.blur(),
            }
        }
        let code = match target {
            MouseTarget::Server(_) | MouseTarget::Edit => KeyCode::Char('e'),
            MouseTarget::Add => KeyCode::Char('a'),
            MouseTarget::Search => KeyCode::Char('/'),
            MouseTarget::Delete => KeyCode::Char('d'),
            MouseTarget::Close | MouseTarget::FormCancel => KeyCode::Esc,
            MouseTarget::ConfirmDelete(true) => KeyCode::Char('y'),
            MouseTarget::ConfirmDelete(false) => KeyCode::Char('n'),
            MouseTarget::FormSave => KeyCode::Enter,
            MouseTarget::FormField(_) => return PanelAction::None,
        };
        self.handle_key(KeyEvent::new(code, KeyModifiers::NONE), config)
    }

    fn pointer_style(&self, target: MouseTarget) -> Style {
        if self.pressed.get() == Some(target) && self.hovered.get() == Some(target) {
            Style::default().bg(role::INTERACTION_PRESSED_BG)
        } else if self.hovered.get() == Some(target) {
            Style::default().bg(self.interaction_animation.hover_background())
        } else {
            Style::default()
        }
    }

    fn push_hit_region(&self, x: u16, y: u16, width: u16, target: MouseTarget) {
        if width > 0 {
            self.hit_regions.borrow_mut().push(HitRegion {
                area: Rect {
                    x,
                    y,
                    width,
                    height: 1,
                },
                target,
            });
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
                if let Some(name) = names.get(self.selected)
                    && let Some(cfg) = config.mcp_servers.iter().find(|s| &s.name == name)
                {
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
        if !(url.is_empty() || url.starts_with("http://") || url.starts_with("https://")) {
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
            .filter_map(|pair| {
                pair.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect();
        let new_cfg = McpServerConfig {
            name: name.clone(),
            command,
            args,
            env,
            url: (!url.is_empty()).then_some(url),
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
        self.hit_regions.borrow_mut().clear();
        self.list_area.set(None);
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
        if matches!(self.mode, Mode::List) {
            self.list_area
                .set(Some(Block::default().borders(Borders::ALL).inner(chunks[1])));
        }

        // -- Title --
        Paragraph::new(Line::from(vec![
            Span::styled(
                "🔗  MCP servers",
                Style::default().fg(role::SECTION_MCP).bold(),
            ),
            Span::styled(
                format!("  ({} configured)", config.mcp_servers.len()),
                Style::default().fg(palette::MUTED).italic(),
            ),
        ]))
        .render(chunks[0], buf);

        let mut list_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(role::BORDER));
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
                        Style::default().fg(palette::MUTED),
                    )),
                    Line::from(Span::styled(
                        "  Example — filesystem: command 'npx', \
                         args '-y @modelcontextprotocol/server-filesystem /path'",
                        Style::default().fg(palette::FAINT),
                    )),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  No MCP servers match the search.",
                        Style::default().fg(palette::MUTED),
                    )),
                ]
            };
            Paragraph::new(message)
                .block(list_block)
                .render(chunks[1], buf);
        } else {
            let items: Vec<ListItem> = filtered
                .iter()
                .enumerate()
                .map(|(index, s)| {
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
                        ("(restart to apply)", Style::default().fg(palette::FAINT))
                    } else if tool_count > 0 {
                        ("● connected", Style::default().fg(palette::GREEN))
                    } else {
                        ("○ not connected", Style::default().fg(palette::RED))
                    };

                    let first = Line::from(vec![
                        Span::styled(s.name.clone(), Style::default().fg(palette::TEXT).bold()),
                        Span::raw("  "),
                        Span::styled(status, status_style),
                        Span::styled(
                            format!("  · {tool_count} tools"),
                            Style::default().fg(palette::FAINT),
                        ),
                    ]);
                    let second = Line::from(Span::styled(
                        format!("  {}", truncate_to_width(&Self::cmdline(s), 90)),
                        Style::default().fg(palette::MUTED),
                    ));
                    ListItem::new(vec![first, second])
                        .style(self.pointer_style(MouseTarget::Server(index)))
                })
                .collect();
            let selected = self.selected.min(filtered.len() - 1);
            let selected_target = MouseTarget::Server(selected);
            let selected_background = if self.hovered.get() == Some(selected_target)
                && self.pressed.get() == Some(selected_target)
            {
                role::INTERACTION_PRESSED_BG
            } else if self.hovered.get() == Some(selected_target) {
                self.interaction_animation.hover_background()
            } else {
                role::SELECTION_BG
            };
            let list = List::new(items)
                .block(list_block)
                .highlight_style(
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(selected_background)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("❯ ");
            let mut list_state = ListState::default();
            list_state.select(Some(selected));
            ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut list_state);

            let inner = Rect {
                x: chunks[1].x.saturating_add(1),
                y: chunks[1].y.saturating_add(1),
                width: chunks[1].width.saturating_sub(2),
                height: chunks[1].height.saturating_sub(2),
            };
            let offset = list_state.offset();
            let mut regions = self.hit_regions.borrow_mut();
            for index in offset..filtered.len() {
                let row = (index - offset) as u16 * 2;
                if row >= inner.height {
                    break;
                }
                regions.push(HitRegion {
                    area: Rect {
                        x: inner.x,
                        y: inner.y.saturating_add(row),
                        width: inner.width,
                        height: 2.min(inner.height.saturating_sub(row)),
                    },
                    target: MouseTarget::Server(index),
                });
            }
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
                    Style::default().fg(palette::FAINT).italic(),
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
                            Style::default().fg(palette::MUTED),
                        ))
                    })
                    .collect()
            };
            Paragraph::new(log_lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(role::BORDER))
                        .title(format!(" {selected_name} · stderr ")),
                )
                .render(log_area, buf);
        }

        // -- Bottom bar: help, confirmation, or form --
        match &self.mode {
            Mode::List => {
                let mut help = vec![
                    Span::styled(" ↑↓", Style::default().fg(palette::BLUE).bold()),
                    Span::styled(" navigate  ", Style::default().fg(palette::MUTED)),
                    Span::styled(
                        "a",
                        Style::default()
                            .fg(palette::BLUE)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::Add)),
                    ),
                    Span::styled(
                        " add  ",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::Add)),
                    ),
                    Span::styled(
                        "e",
                        Style::default()
                            .fg(palette::BLUE)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::Edit)),
                    ),
                    Span::styled(
                        " edit  ",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::Edit)),
                    ),
                ];
                help.extend([
                    Span::styled(
                        "/",
                        Style::default()
                            .fg(palette::BLUE)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::Search)),
                    ),
                    Span::styled(
                        " search  ",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::Search)),
                    ),
                ]);
                help.extend([
                    Span::styled(
                        "d",
                        Style::default()
                            .fg(palette::RED)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::Delete)),
                    ),
                    Span::styled(
                        " delete  ",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::Delete)),
                    ),
                    Span::styled(
                        "q/Esc",
                        Style::default()
                            .fg(palette::BLUE)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::Close)),
                    ),
                    Span::styled(
                        " close",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::Close)),
                    ),
                ]);
                Paragraph::new(Line::from(help)).render(bottom, buf);
                let mut x = bottom.x.saturating_add(" ↑↓ navigate  ".width() as u16);
                self.push_hit_region(x, bottom.y, "a add  ".width() as u16, MouseTarget::Add);
                x = x.saturating_add("a add  ".width() as u16);
                self.push_hit_region(x, bottom.y, "e edit  ".width() as u16, MouseTarget::Edit);
                x = x.saturating_add("e edit  ".width() as u16);
                self.push_hit_region(
                    x,
                    bottom.y,
                    "/ search  ".width() as u16,
                    MouseTarget::Search,
                );
                x = x.saturating_add("/ search  ".width() as u16);
                self.push_hit_region(
                    x,
                    bottom.y,
                    "d delete  ".width() as u16,
                    MouseTarget::Delete,
                );
                x = x.saturating_add("d delete  ".width() as u16);
                self.push_hit_region(
                    x,
                    bottom.y,
                    "q/Esc close".width() as u16,
                    MouseTarget::Close,
                );
            }
            Mode::ConfirmDelete(name) => {
                let confirm = Line::from(vec![
                    Span::styled(
                        format!(" Delete MCP server '{name}'?  "),
                        Style::default().fg(palette::YELLOW).bold(),
                    ),
                    Span::styled(
                        "y",
                        Style::default()
                            .fg(palette::GREEN)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::ConfirmDelete(true))),
                    ),
                    Span::styled(
                        " yes  ",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::ConfirmDelete(true))),
                    ),
                    Span::styled(
                        "n",
                        Style::default()
                            .fg(palette::RED)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::ConfirmDelete(false))),
                    ),
                    Span::styled(
                        " cancel",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::ConfirmDelete(false))),
                    ),
                ]);
                Paragraph::new(confirm).render(bottom, buf);
                let prefix = format!(" Delete MCP server '{name}'?  ");
                let yes_x = bottom.x.saturating_add(prefix.width() as u16);
                self.push_hit_region(
                    yes_x,
                    bottom.y,
                    "y yes  ".width() as u16,
                    MouseTarget::ConfirmDelete(true),
                );
                self.push_hit_region(
                    yes_x.saturating_add("y yes  ".width() as u16),
                    bottom.y,
                    "n cancel".width() as u16,
                    MouseTarget::ConfirmDelete(false),
                );
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
                            Style::default().fg(palette::BLUE).bold()
                        } else {
                            Style::default().fg(palette::MUTED)
                        };
                        let value = if form.fields[i].is_empty() {
                            Span::styled("(empty)", Style::default().fg(palette::FAINT).italic())
                        } else {
                            Span::styled(form.fields[i].clone(), Style::default().fg(palette::TEXT))
                        };
                        let cursor = if focused { "▏" } else { "" };
                        Line::from(vec![
                            Span::styled(format!("{marker}{label:>22}: "), label_style),
                            value,
                            Span::styled(cursor, Style::default().fg(palette::BLUE)),
                        ])
                        .style(self.pointer_style(MouseTarget::FormField(i)))
                    })
                    .collect();
                if let Some(err) = &form.error {
                    lines.push(Line::from(Span::styled(
                        format!("  ⚠ {err}"),
                        Style::default().fg(palette::RED),
                    )));
                }
                lines.push(Line::from(vec![
                    Span::styled("  Tab/↑↓", Style::default().fg(palette::BLUE).bold()),
                    Span::styled(" next field  ", Style::default().fg(palette::MUTED)),
                    Span::styled(
                        "Enter",
                        Style::default()
                            .fg(palette::BLUE)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::FormSave)),
                    ),
                    Span::styled(
                        " save  ",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::FormSave)),
                    ),
                    Span::styled(
                        "Esc",
                        Style::default()
                            .fg(palette::BLUE)
                            .bold()
                            .patch(self.pointer_style(MouseTarget::FormCancel)),
                    ),
                    Span::styled(
                        " cancel",
                        Style::default()
                            .fg(palette::MUTED)
                            .patch(self.pointer_style(MouseTarget::FormCancel)),
                    ),
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
                            .border_style(Style::default().fg(role::FOCUS))
                            .title_style(Style::default().fg(role::FOCUS).bold())
                            .title(title),
                    )
                    .render(form_area, buf);

                let inner_x = form_area.x.saturating_add(1);
                let inner_y = form_area.y.saturating_add(1);
                let inner_width = form_area.width.saturating_sub(2);
                let inner_height = form_area.height.saturating_sub(2);
                for index in 0..FORM_LABELS.len().min(inner_height as usize) {
                    self.hit_regions.borrow_mut().push(HitRegion {
                        area: Rect {
                            x: inner_x,
                            y: inner_y.saturating_add(index as u16),
                            width: inner_width,
                            height: 1,
                        },
                        target: MouseTarget::FormField(index),
                    });
                }
                let hint_row = (lines.len() - 1) as u16;
                if hint_row < inner_height {
                    let hint_y = inner_y.saturating_add(hint_row);
                    let save_x = inner_x.saturating_add("  Tab/↑↓ next field  ".width() as u16);
                    self.push_hit_region(
                        save_x,
                        hint_y,
                        "Enter save  ".width() as u16,
                        MouseTarget::FormSave,
                    );
                    self.push_hit_region(
                        save_x.saturating_add("Enter save  ".width() as u16),
                        hint_y,
                        "Esc cancel".width() as u16,
                        MouseTarget::FormCancel,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            command: "cmd".into(),
            args: vec![],
            env: Default::default(),
            url: None,
        }
    }

    fn render(panel: &McpPanel, config: &ProgrammerConfig) {
        let area = Rect::new(0, 0, 100, 20);
        let mut buffer = Buffer::empty(area);
        panel.render(config, None, area, &mut buffer);
    }

    fn target_point(panel: &McpPanel, target: MouseTarget) -> (u16, u16) {
        let hit = panel
            .hit_regions
            .borrow()
            .iter()
            .find(|hit| hit.target == target)
            .copied()
            .expect("target should have a hit region");
        (hit.area.x, hit.area.y)
    }

    fn click_target(
        panel: &mut McpPanel,
        target: MouseTarget,
        config: &mut ProgrammerConfig,
    ) -> PanelAction {
        let (column, row) = target_point(panel, target);
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), column, row),
                config,
            ),
            PanelAction::None
        );
        panel.handle_mouse(
            mouse(MouseEventKind::Up(MouseButton::Left), column, row),
            config,
        )
    }

    #[test]
    fn mouse_list_rows_buttons_wheel_and_matching_release() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        config.mcp_servers = vec![server("one"), server("two")];
        render(&panel, &config);

        let (add_x, add_y) = target_point(&panel, MouseTarget::Add);
        let (edit_x, edit_y) = target_point(&panel, MouseTarget::Edit);
        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), add_x, add_y),
            &mut config,
        );
        panel.handle_mouse(
            mouse(MouseEventKind::Up(MouseButton::Left), edit_x, edit_y),
            &mut config,
        );
        assert!(matches!(panel.mode, Mode::List));

        panel.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0), &mut config);
        assert_eq!(panel.selected, 0);
        let (list_x, list_y) = target_point(&panel, MouseTarget::Server(0));
        panel.handle_mouse(
            mouse(MouseEventKind::ScrollDown, list_x, list_y),
            &mut config,
        );
        assert_eq!(panel.selected, 1);
        assert_eq!(
            click_target(&mut panel, MouseTarget::Edit, &mut config),
            PanelAction::None
        );
        assert!(matches!(
            &panel.mode,
            Mode::Form(Form {
                original: Some(name),
                ..
            }) if name == "two"
        ));

        panel.handle_key(key(KeyCode::Esc), &mut config);
        render(&panel, &config);
        assert_eq!(
            click_target(&mut panel, MouseTarget::Server(0), &mut config),
            PanelAction::None
        );
        assert!(matches!(
            &panel.mode,
            Mode::Form(Form {
                original: Some(name),
                ..
            }) if name == "one"
        ));

        panel.handle_key(key(KeyCode::Esc), &mut config);
        render(&panel, &config);
        assert_eq!(
            click_target(&mut panel, MouseTarget::Add, &mut config),
            PanelAction::None
        );
        assert!(matches!(
            panel.mode,
            Mode::Form(Form { original: None, .. })
        ));
    }

    #[test]
    fn mouse_confirm_form_fields_save_cancel_and_animation() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        config.mcp_servers.push(server("old"));
        render(&panel, &config);

        let (delete_x, delete_y) = target_point(&panel, MouseTarget::Delete);
        panel.handle_mouse(
            mouse(MouseEventKind::Moved, delete_x, delete_y),
            &mut config,
        );
        assert_eq!(panel.hovered.get(), Some(MouseTarget::Delete));
        assert!(panel.interaction_animation_active());
        panel.handle_mouse(mouse(MouseEventKind::Moved, 99, 19), &mut config);
        assert_eq!(panel.hovered.get(), None);
        assert!(!panel.interaction_animation_active());

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), delete_x, delete_y),
            &mut config,
        );
        panel.cancel_pointer_gesture();
        assert_eq!(panel.hovered.get(), None);
        assert_eq!(panel.pressed.get(), None);

        click_target(&mut panel, MouseTarget::Delete, &mut config);
        render(&panel, &config);
        assert_eq!(
            click_target(&mut panel, MouseTarget::ConfirmDelete(false), &mut config,),
            PanelAction::None
        );
        assert_eq!(config.mcp_servers.len(), 1);

        panel.handle_key(ch('a'), &mut config);
        render(&panel, &config);
        click_target(&mut panel, MouseTarget::FormField(1), &mut config);
        assert!(matches!(&panel.mode, Mode::Form(form) if form.focus == 1));
        for c in "cmd".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        click_target(&mut panel, MouseTarget::FormField(0), &mut config);
        for c in "new".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        assert_eq!(
            click_target(&mut panel, MouseTarget::FormSave, &mut config),
            PanelAction::Saved
        );
        assert!(config.mcp_servers.iter().any(|server| server.name == "new"));

        panel.handle_key(ch('a'), &mut config);
        render(&panel, &config);
        assert_eq!(
            click_target(&mut panel, MouseTarget::FormCancel, &mut config),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::List));

        panel.handle_key(ch('d'), &mut config);
        render(&panel, &config);
        assert_eq!(
            click_target(&mut panel, MouseTarget::ConfirmDelete(true), &mut config,),
            PanelAction::Saved
        );
        assert_eq!(config.mcp_servers.len(), 1);
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
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter), &mut config),
            PanelAction::Saved
        );
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
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter), &mut config),
            PanelAction::Saved
        );
        assert_eq!(
            config.mcp_servers[0].url.as_deref(),
            Some("https://mcp.exa.ai/mcp")
        );
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
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter), &mut config),
            PanelAction::None
        );
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn form_requires_name_and_command() {
        let mut panel = McpPanel::new();
        let mut config = ProgrammerConfig::default();
        panel.handle_key(ch('a'), &mut config);
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter), &mut config),
            PanelAction::None
        );
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
        for c in "srv".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config);
        for c in "cmd".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        panel.handle_key(key(KeyCode::Tab), &mut config); // url
        panel.handle_key(key(KeyCode::Tab), &mut config); // args
        panel.handle_key(key(KeyCode::Tab), &mut config); // env
        for c in "API_KEY=secret".chars() {
            panel.handle_key(ch(c), &mut config);
        }
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter), &mut config),
            PanelAction::Saved
        );
        assert_eq!(config.mcp_servers[0].env.get("API_KEY").unwrap(), "secret");
    }
}
