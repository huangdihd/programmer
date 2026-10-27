// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Full-screen security-profile management panel.

use crate::config::programmer_config::{ProgrammerConfig, validate_security_profile_name};
use crate::security::policy::{AccessKind, PermissionEffect, PermissionRule};
use crate::security::{SandboxMode, SecurityConfig};
use crate::ui::components::panel_search::{PanelSearch, SearchKey};
use crate::ui::interaction::InteractionAnimation;
use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use globset::Glob;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    None,
    Close,
    /// Profile metadata or an inactive profile changed.
    Saved,
    /// The active policy changed and must be installed before it is persisted.
    Apply,
}

#[derive(Debug)]
enum NameAction {
    Create { template: SecurityConfig },
    Rename { original: String },
}

#[derive(Debug)]
enum Mode {
    List,
    Name {
        action: NameAction,
        value: String,
        error: Option<String>,
    },
    ConfirmDelete(String),
    Edit {
        name: String,
        selected: usize,
    },
    Collection {
        name: String,
        kind: CollectionKind,
        selected: usize,
    },
    ValueForm {
        name: String,
        kind: CollectionKind,
        original: Option<usize>,
        value: String,
        error: Option<String>,
    },
    RuleForm {
        name: String,
        original: Option<usize>,
        operation: AccessKind,
        effect: PermissionEffect,
        pattern: String,
        focus: usize,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseTarget {
    Row(usize),
    Activate,
    Primary,
    Secondary,
    Tertiary,
    Quaternary,
    Search,
    Back,
    Save,
    Cancel,
    FormField(usize),
}

#[derive(Debug)]
pub struct SecurityPanel {
    mode: Mode,
    selected: usize,
    search: PanelSearch,
    mouse_hover: Option<MouseTarget>,
    mouse_pressed: Option<MouseTarget>,
    interaction_animation: InteractionAnimation,
}

#[derive(Debug, Clone, Copy)]
enum SecuritySetting {
    ProcessSandbox,
    Network,
    FilesystemPolicy,
    SystemReads,
    TemporaryWrites,
    FailClosed,
    FileProtection,
    OutsideReads,
    Rules,
    ReadablePaths,
    WritablePaths,
    DeniedReadPaths,
    DeniedEnvironment,
}

impl SecuritySetting {
    const ALL: [Self; 13] = [
        Self::ProcessSandbox,
        Self::Network,
        Self::FilesystemPolicy,
        Self::SystemReads,
        Self::TemporaryWrites,
        Self::FailClosed,
        Self::FileProtection,
        Self::OutsideReads,
        Self::Rules,
        Self::ReadablePaths,
        Self::WritablePaths,
        Self::DeniedReadPaths,
        Self::DeniedEnvironment,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::ProcessSandbox => "process sandbox",
            Self::Network => "network access",
            Self::FilesystemPolicy => "filesystem policy",
            Self::SystemReads => "system reads",
            Self::TemporaryWrites => "temporary writes",
            Self::FailClosed => "fail closed",
            Self::FileProtection => "file conflict protection",
            Self::OutsideReads => "reads outside workspace",
            Self::Rules => "permission rules",
            Self::ReadablePaths => "additional readable paths",
            Self::WritablePaths => "additional writable paths",
            Self::DeniedReadPaths => "denied read paths",
            Self::DeniedEnvironment => "denied environment",
        }
    }

    fn boolean_value(self, config: &SecurityConfig) -> Option<bool> {
        match self {
            Self::ProcessSandbox => Some(config.sandbox.enabled),
            Self::Network => Some(config.sandbox.network),
            Self::FilesystemPolicy => Some(config.enabled),
            Self::SystemReads => Some(config.sandbox.allow_system_read),
            Self::TemporaryWrites => Some(config.sandbox.allow_temp_write),
            Self::FailClosed => Some(config.sandbox.fail_closed),
            Self::FileProtection => Some(config.protect_file_changes),
            Self::OutsideReads => Some(config.allow_read_outside_workspace),
            Self::Rules
            | Self::ReadablePaths
            | Self::WritablePaths
            | Self::DeniedReadPaths
            | Self::DeniedEnvironment => None,
        }
    }

    fn toggle(self, config: &mut SecurityConfig) {
        let Some(enabled) = self.boolean_value(config).map(|enabled| !enabled) else {
            unreachable!("collection setting cannot be toggled");
        };
        match self {
            Self::ProcessSandbox => config.sandbox.enabled = enabled,
            Self::Network => config.sandbox.network = enabled,
            Self::FilesystemPolicy => config.enabled = enabled,
            Self::SystemReads => config.sandbox.allow_system_read = enabled,
            Self::TemporaryWrites => config.sandbox.allow_temp_write = enabled,
            Self::FailClosed => config.sandbox.fail_closed = enabled,
            Self::FileProtection => config.protect_file_changes = enabled,
            Self::OutsideReads => config.allow_read_outside_workspace = enabled,
            Self::Rules
            | Self::ReadablePaths
            | Self::WritablePaths
            | Self::DeniedReadPaths
            | Self::DeniedEnvironment => unreachable!("collection setting cannot be toggled"),
        }
    }

    fn collection(self) -> Option<CollectionKind> {
        match self {
            Self::Rules => Some(CollectionKind::Rules),
            Self::ReadablePaths => Some(CollectionKind::ReadablePaths),
            Self::WritablePaths => Some(CollectionKind::WritablePaths),
            Self::DeniedReadPaths => Some(CollectionKind::DeniedReadPaths),
            Self::DeniedEnvironment => Some(CollectionKind::DeniedEnvironment),
            _ => None,
        }
    }

    fn value(self, config: &SecurityConfig) -> String {
        if let Some(enabled) = self.boolean_value(config) {
            return state_label(enabled).to_string();
        }
        let count = self
            .collection()
            .expect("non-boolean setting must be a collection")
            .len(config);
        format!("{count} items")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectionKind {
    Rules,
    ReadablePaths,
    WritablePaths,
    DeniedReadPaths,
    DeniedEnvironment,
}

impl CollectionKind {
    fn title(self) -> &'static str {
        match self {
            Self::Rules => "Permission rules",
            Self::ReadablePaths => "Additional readable paths",
            Self::WritablePaths => "Additional writable paths",
            Self::DeniedReadPaths => "Denied read paths",
            Self::DeniedEnvironment => "Denied environment",
        }
    }

    fn singular(self) -> &'static str {
        match self {
            Self::Rules => "rule",
            Self::ReadablePaths | Self::WritablePaths | Self::DeniedReadPaths => "path",
            Self::DeniedEnvironment => "environment pattern",
        }
    }

    fn len(self, config: &SecurityConfig) -> usize {
        match self {
            Self::Rules => config.rules.len(),
            Self::ReadablePaths => config.sandbox.readable_paths.len(),
            Self::WritablePaths => config.sandbox.writable_paths.len(),
            Self::DeniedReadPaths => config.sandbox.denied_read_paths.len(),
            Self::DeniedEnvironment => config.sandbox.denied_environment.len(),
        }
    }

    fn values(self, config: &SecurityConfig) -> Vec<String> {
        match self {
            Self::Rules => config
                .rules
                .iter()
                .map(|rule| {
                    format!(
                        "{:<7} {:<7} {}",
                        effect_label(rule.effect),
                        access_label(rule.operation),
                        rule.pattern
                    )
                })
                .collect(),
            Self::ReadablePaths => display_paths(&config.sandbox.readable_paths),
            Self::WritablePaths => display_paths(&config.sandbox.writable_paths),
            Self::DeniedReadPaths => display_paths(&config.sandbox.denied_read_paths),
            Self::DeniedEnvironment => config.sandbox.denied_environment.clone(),
        }
    }

    fn string_value(self, config: &SecurityConfig, index: usize) -> Option<String> {
        match self {
            Self::Rules => None,
            Self::ReadablePaths => config
                .sandbox
                .readable_paths
                .get(index)
                .map(|path| path.to_string_lossy().into_owned()),
            Self::WritablePaths => config
                .sandbox
                .writable_paths
                .get(index)
                .map(|path| path.to_string_lossy().into_owned()),
            Self::DeniedReadPaths => config
                .sandbox
                .denied_read_paths
                .get(index)
                .map(|path| path.to_string_lossy().into_owned()),
            Self::DeniedEnvironment => config.sandbox.denied_environment.get(index).cloned(),
        }
    }

    fn set_string(self, config: &mut SecurityConfig, original: Option<usize>, value: String) {
        match self {
            Self::Rules => unreachable!("rules use a structured editor"),
            Self::ReadablePaths => {
                set_vec_item(&mut config.sandbox.readable_paths, original, value.into())
            }
            Self::WritablePaths => {
                set_vec_item(&mut config.sandbox.writable_paths, original, value.into())
            }
            Self::DeniedReadPaths => set_vec_item(
                &mut config.sandbox.denied_read_paths,
                original,
                value.into(),
            ),
            Self::DeniedEnvironment => {
                set_vec_item(&mut config.sandbox.denied_environment, original, value)
            }
        }
    }

    fn remove(self, config: &mut SecurityConfig, index: usize) {
        match self {
            Self::Rules => {
                config.rules.remove(index);
            }
            Self::ReadablePaths => {
                config.sandbox.readable_paths.remove(index);
            }
            Self::WritablePaths => {
                config.sandbox.writable_paths.remove(index);
            }
            Self::DeniedReadPaths => {
                config.sandbox.denied_read_paths.remove(index);
            }
            Self::DeniedEnvironment => {
                config.sandbox.denied_environment.remove(index);
            }
        }
    }
}

impl SecurityPanel {
    pub fn new() -> Self {
        Self {
            mode: Mode::List,
            selected: 0,
            search: PanelSearch::default(),
            mouse_hover: None,
            mouse_pressed: None,
            interaction_animation: InteractionAnimation::default(),
        }
    }

    pub(crate) fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        config: &mut ProgrammerConfig,
    ) -> PanelAction {
        let target = self.mouse_target(mouse.column, mouse.row, area, config);
        if self.mouse_hover != target {
            self.mouse_hover = target;
            if target.is_some() {
                self.interaction_animation.restart();
            } else {
                self.interaction_animation.cancel();
            }
        }
        match mouse.kind {
            MouseEventKind::ScrollUp if self.pointer_over_list(mouse.column, mouse.row, area) => {
                return self.handle_key(KeyEvent::new(KeyCode::Up, mouse.modifiers), config);
            }
            MouseEventKind::ScrollDown if self.pointer_over_list(mouse.column, mouse.row, area) => {
                return self.handle_key(KeyEvent::new(KeyCode::Down, mouse.modifiers), config);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.mouse_pressed = target;
                self.select_mouse_target(target);
                self.interaction_animation.cancel();
            }
            MouseEventKind::Drag(MouseButton::Left) => {}
            MouseEventKind::Up(MouseButton::Left) => {
                let activate = self
                    .mouse_pressed
                    .take()
                    .filter(|pressed| Some(*pressed) == target);
                if let Some(target) = activate {
                    self.interaction_animation.restart();
                    return self.activate_mouse_target(target, config);
                }
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

    fn select_mouse_target(&mut self, target: Option<MouseTarget>) {
        let Some(MouseTarget::Row(index)) = target else {
            if let Some(MouseTarget::FormField(index)) = target
                && let Mode::RuleForm { focus, .. } = &mut self.mode
            {
                *focus = index;
            }
            return;
        };
        match &mut self.mode {
            Mode::List => self.selected = index,
            Mode::Edit { selected, .. } | Mode::Collection { selected, .. } => *selected = index,
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
                MouseTarget::Back => return PanelAction::Close,
                _ => self.search.blur(),
            }
        }
        let code = match (&self.mode, target) {
            (_, MouseTarget::Row(_)) | (Mode::List, MouseTarget::Activate) => KeyCode::Enter,
            (Mode::List, MouseTarget::Primary) => KeyCode::Char('a'),
            (Mode::List, MouseTarget::Secondary) => KeyCode::Char('e'),
            (Mode::List, MouseTarget::Tertiary) => KeyCode::Char('d'),
            (Mode::List, MouseTarget::Quaternary) => KeyCode::Char('r'),
            (Mode::List, MouseTarget::Search) => KeyCode::Char('/'),
            (Mode::Edit { .. }, MouseTarget::Primary) => KeyCode::Enter,
            (Mode::Collection { .. }, MouseTarget::Primary) => KeyCode::Char('a'),
            (Mode::Collection { .. }, MouseTarget::Secondary) => KeyCode::Char('e'),
            (Mode::Collection { .. }, MouseTarget::Tertiary) => KeyCode::Char('d'),
            (Mode::ConfirmDelete(_), MouseTarget::Save) => KeyCode::Char('y'),
            (_, MouseTarget::Save) => KeyCode::Enter,
            (_, MouseTarget::Back | MouseTarget::Cancel) => KeyCode::Esc,
            (_, MouseTarget::FormField(_)) => return PanelAction::None,
            _ => return PanelAction::None,
        };
        self.handle_key(KeyEvent::new(code, KeyModifiers::NONE), config)
    }

    fn mouse_target(
        &self,
        column: u16,
        row: u16,
        area: Rect,
        config: &ProgrammerConfig,
    ) -> Option<MouseTarget> {
        if !area.contains((column, row).into()) {
            return None;
        }

        match &self.mode {
            Mode::Name { error, .. } => {
                let overlay = security_overlay(area, 2 + usize::from(error.is_some()));
                let action_row = overlay.y + 2;
                return (row == action_row).then(|| {
                    hit_horizontal_target(
                        column,
                        overlay.x.saturating_add(1),
                        &[
                            ("Enter save  ", Some(MouseTarget::Save)),
                            ("Esc cancel", Some(MouseTarget::Cancel)),
                        ],
                    )
                })?;
            }
            Mode::ValueForm { error, .. } => {
                let overlay = security_overlay(area, 2 + usize::from(error.is_some()));
                let action_row = overlay.y + 2 + u16::from(error.is_some());
                return (row == action_row).then(|| {
                    hit_horizontal_target(
                        column,
                        overlay.x.saturating_add(1),
                        &[
                            ("Enter save  ", Some(MouseTarget::Save)),
                            ("Esc cancel", Some(MouseTarget::Cancel)),
                        ],
                    )
                })?;
            }
            Mode::RuleForm { error, .. } => {
                let line_count = 4 + usize::from(error.is_some());
                let overlay = security_overlay(area, line_count);
                let inner = Block::default().borders(Borders::ALL).inner(overlay);
                if inner.contains((column, row).into()) && row < inner.y.saturating_add(3) {
                    return Some(MouseTarget::FormField(usize::from(row - inner.y)));
                }
                let action_row = inner.y + 3 + u16::from(error.is_some());
                return (row == action_row).then(|| {
                    hit_horizontal_target(
                        column,
                        inner.x,
                        &[
                            ("Tab/↑↓ field  ←→/Space change  ", None),
                            ("Enter save  ", Some(MouseTarget::Save)),
                            ("Esc cancel", Some(MouseTarget::Cancel)),
                        ],
                    )
                })?;
            }
            Mode::ConfirmDelete(name) => {
                let overlay = security_overlay(area, 2);
                let action_row = overlay.y + 2;
                if row != action_row {
                    return None;
                }
                let blocked =
                    config.security_profiles.len() == 1 || config.active_security_profile == *name;
                return if blocked {
                    hit_horizontal_target(
                        column,
                        overlay.x.saturating_add(1),
                        &[("Esc back", Some(MouseTarget::Cancel))],
                    )
                } else {
                    hit_horizontal_target(
                        column,
                        overlay.x.saturating_add(1),
                        &[
                            ("y yes  ", Some(MouseTarget::Save)),
                            ("n no", Some(MouseTarget::Cancel)),
                        ],
                    )
                };
            }
            _ => {}
        }

        let chunks = panel_chunks(area);
        let inner = Block::default().borders(Borders::ALL).inner(chunks[1]);
        if inner.contains((column, row).into()) {
            let visible = usize::from(inner.height);
            if visible == 0 {
                return None;
            }
            let row = usize::from(row - inner.y);
            return match &self.mode {
                Mode::List => {
                    let names = self.filtered_names(config);
                    let offset = self.selected.saturating_add(1).saturating_sub(visible);
                    let index = offset + row;
                    (index < names.len()).then_some(MouseTarget::Row(index))
                }
                Mode::Edit { selected, .. } => {
                    let offset = selected.saturating_add(1).saturating_sub(visible);
                    let index = offset + row;
                    (index < SecuritySetting::ALL.len()).then_some(MouseTarget::Row(index))
                }
                Mode::Collection {
                    name,
                    kind,
                    selected,
                } => {
                    let offset = selected.saturating_add(1).saturating_sub(visible);
                    let index = offset + row;
                    (index < kind.len(&config.security_profiles[name]))
                        .then_some(MouseTarget::Row(index))
                }
                _ => None,
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
                    ("Enter activate  ", Some(MouseTarget::Activate)),
                    ("a clone  ", Some(MouseTarget::Primary)),
                    ("e edit  ", Some(MouseTarget::Secondary)),
                    ("r rename  ", Some(MouseTarget::Quaternary)),
                    ("d delete  ", Some(MouseTarget::Tertiary)),
                    ("/ search  ", Some(MouseTarget::Search)),
                    ("q/Esc close", Some(MouseTarget::Back)),
                ],
            ),
            Mode::Edit { .. } => hit_horizontal_target(
                column,
                chunks[2].x,
                &[
                    ("↑↓ select  ", None),
                    ("←→/Space/Enter change/open  ", Some(MouseTarget::Primary)),
                    ("Esc back", Some(MouseTarget::Back)),
                ],
            ),
            Mode::Collection { .. } => hit_horizontal_target(
                column,
                chunks[2].x,
                &[
                    ("↑↓ select  ", None),
                    ("a add  ", Some(MouseTarget::Primary)),
                    ("e/Enter edit  ", Some(MouseTarget::Secondary)),
                    ("d delete  ", Some(MouseTarget::Tertiary)),
                    ("Esc back", Some(MouseTarget::Back)),
                ],
            ),
            _ => None,
        }
    }

    fn pointer_over_list(&self, column: u16, row: u16, area: Rect) -> bool {
        matches!(
            self.mode,
            Mode::List | Mode::Edit { .. } | Mode::Collection { .. }
        ) && Block::default()
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
                    .fg(palette::TEXT)
                    .bg(self.interaction_animation.hover_background())
                    .bold(),
            )
        } else {
            None
        }
    }

    fn action_key(&self, label: &'static str, target: MouseTarget) -> Span<'static> {
        Span::styled(
            label,
            Style::default()
                .fg(palette::BLUE)
                .bold()
                .patch(self.mouse_style(target).unwrap_or_default()),
        )
    }

    fn action_hint(&self, label: &'static str, target: MouseTarget) -> Span<'static> {
        Span::styled(
            label,
            Style::default()
                .fg(palette::MUTED)
                .patch(self.mouse_style(target).unwrap_or_default()),
        )
    }

    fn filtered_names(&self, config: &ProgrammerConfig) -> Vec<String> {
        config
            .security_profiles
            .keys()
            .filter(|name| self.search.matches(&[name.as_str()]))
            .cloned()
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        match &mut self.mode {
            Mode::List => self.handle_list_key(key, config),
            Mode::Name { .. } => self.handle_name_key(key, config),
            Mode::ConfirmDelete(_) => self.handle_delete_key(key, config),
            Mode::Edit { .. } => self.handle_edit_key(key, config),
            Mode::Collection { .. } => self.handle_collection_key(key, config),
            Mode::ValueForm { .. } => self.handle_value_form_key(key, config),
            Mode::RuleForm { .. } => self.handle_rule_form_key(key, config),
        }
    }

    pub fn handle_paste(&mut self, data: &str) {
        let value = data
            .chars()
            .filter(|character| *character != '\n' && *character != '\r');
        match &mut self.mode {
            Mode::Name {
                value: target,
                error,
                ..
            }
            | Mode::ValueForm {
                value: target,
                error,
                ..
            } => {
                target.extend(value);
                *error = None;
            }
            Mode::RuleForm {
                pattern,
                focus,
                error,
                ..
            } if *focus == 2 => {
                pattern.extend(value);
                *error = None;
            }
            _ => {}
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
            KeyCode::Char('q') | KeyCode::Esc => PanelAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                PanelAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(names.len().saturating_sub(1));
                PanelAction::None
            }
            KeyCode::Enter => {
                let Some(name) = names.get(self.selected) else {
                    return PanelAction::None;
                };
                if config.active_security_profile == *name {
                    return PanelAction::None;
                }
                config
                    .activate_security_profile(name)
                    .expect("listed security profile must exist");
                PanelAction::Apply
            }
            KeyCode::Char('a') => {
                let template = names
                    .get(self.selected)
                    .and_then(|name| config.security_profiles.get(name))
                    .cloned()
                    .unwrap_or_default();
                self.mode = Mode::Name {
                    action: NameAction::Create { template },
                    value: String::new(),
                    error: None,
                };
                PanelAction::None
            }
            KeyCode::Char('r') => {
                let Some(name) = names.get(self.selected) else {
                    return PanelAction::None;
                };
                self.mode = Mode::Name {
                    action: NameAction::Rename {
                        original: name.clone(),
                    },
                    value: name.clone(),
                    error: None,
                };
                PanelAction::None
            }
            KeyCode::Char('e') => {
                let Some(name) = names.get(self.selected) else {
                    return PanelAction::None;
                };
                self.mode = Mode::Edit {
                    name: name.clone(),
                    selected: 0,
                };
                PanelAction::None
            }
            KeyCode::Char('d') => {
                let Some(name) = names.get(self.selected) else {
                    return PanelAction::None;
                };
                self.mode = Mode::ConfirmDelete(name.clone());
                PanelAction::None
            }
            _ => PanelAction::None,
        }
    }

    fn handle_name_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        let Mode::Name {
            action,
            value,
            error,
        } = &mut self.mode
        else {
            unreachable!("name handler called outside name mode");
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                PanelAction::None
            }
            KeyCode::Backspace => {
                value.pop();
                *error = None;
                PanelAction::None
            }
            KeyCode::Char(character) => {
                value.push(character);
                *error = None;
                PanelAction::None
            }
            KeyCode::Enter => {
                let name = value.trim();
                if let Err(message) = validate_security_profile_name(name) {
                    *error = Some(message);
                    return PanelAction::None;
                }
                if config.security_profiles.contains_key(name)
                    && !matches!(
                        action,
                        NameAction::Rename { original } if original == name
                    )
                {
                    *error = Some(format!("profile '{name}' already exists"));
                    return PanelAction::None;
                }

                match action {
                    NameAction::Create { template } => {
                        config
                            .security_profiles
                            .insert(name.to_string(), template.clone());
                    }
                    NameAction::Rename { original } => {
                        if original == name {
                            self.mode = Mode::List;
                            return PanelAction::None;
                        }
                        let profile = config
                            .security_profiles
                            .remove(original)
                            .expect("renamed security profile must exist");
                        let was_active = config.active_security_profile == *original;
                        config.security_profiles.insert(name.to_string(), profile);
                        if was_active {
                            config.active_security_profile = name.to_string();
                        }
                    }
                }
                self.selected = config
                    .security_profiles
                    .keys()
                    .position(|candidate| candidate == name)
                    .unwrap_or(0);
                self.mode = Mode::List;
                PanelAction::Saved
            }
            _ => PanelAction::None,
        }
    }

    fn handle_delete_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        let Mode::ConfirmDelete(name) = &self.mode else {
            unreachable!("delete handler called outside confirmation mode");
        };
        match key.code {
            KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('d') => {
                self.mode = Mode::List;
                PanelAction::None
            }
            KeyCode::Char('y') => {
                if config.security_profiles.len() == 1 || config.active_security_profile == *name {
                    self.mode = Mode::List;
                    return PanelAction::None;
                }
                config.security_profiles.remove(name);
                self.selected = self
                    .selected
                    .min(config.security_profiles.len().saturating_sub(1));
                self.mode = Mode::List;
                PanelAction::Saved
            }
            _ => PanelAction::None,
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent, config: &mut ProgrammerConfig) -> PanelAction {
        let Mode::Edit { name, selected } = &mut self.mode else {
            unreachable!("edit handler called outside edit mode");
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::List;
                return PanelAction::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                return PanelAction::None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(SecuritySetting::ALL.len() - 1);
                return PanelAction::None;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if SecuritySetting::ALL[*selected]
                    .boolean_value(&config.security_profiles[name])
                    .is_none()
                {
                    return PanelAction::None;
                }
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') => {}
            _ => return PanelAction::None,
        }

        let setting = SecuritySetting::ALL[*selected];
        if let Some(kind) = setting.collection() {
            self.mode = Mode::Collection {
                name: name.clone(),
                kind,
                selected: 0,
            };
            return PanelAction::None;
        }
        let profile = config
            .security_profiles
            .get_mut(name)
            .expect("edited security profile must exist");
        setting.toggle(profile);
        action_after_profile_change(config, name)
    }

    fn handle_collection_key(
        &mut self,
        key: KeyEvent,
        config: &mut ProgrammerConfig,
    ) -> PanelAction {
        let Mode::Collection {
            name,
            kind,
            selected,
        } = &mut self.mode
        else {
            unreachable!("collection handler called outside collection mode");
        };
        let profile = &config.security_profiles[name];
        let len = kind.len(profile);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Edit {
                    name: name.clone(),
                    selected: SecuritySetting::ALL
                        .iter()
                        .position(|setting| setting.collection() == Some(*kind))
                        .expect("collection must have a parent setting"),
                };
                PanelAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                PanelAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(len.saturating_sub(1));
                PanelAction::None
            }
            KeyCode::Char('a') => {
                self.mode = Self::collection_form(name.clone(), *kind, None, config);
                PanelAction::None
            }
            KeyCode::Enter | KeyCode::Char('e') if len > 0 => {
                self.mode = Self::collection_form(name.clone(), *kind, Some(*selected), config);
                PanelAction::None
            }
            KeyCode::Char('d') if len > 0 => {
                let profile = config
                    .security_profiles
                    .get_mut(name)
                    .expect("edited security profile must exist");
                kind.remove(profile, *selected);
                *selected = (*selected).min(kind.len(profile).saturating_sub(1));
                action_after_profile_change(config, name)
            }
            _ => PanelAction::None,
        }
    }

    fn collection_form(
        name: String,
        kind: CollectionKind,
        original: Option<usize>,
        config: &ProgrammerConfig,
    ) -> Mode {
        let profile = &config.security_profiles[&name];
        if kind == CollectionKind::Rules {
            let rule = original.and_then(|index| profile.rules.get(index));
            Mode::RuleForm {
                name,
                original,
                operation: rule.map_or(AccessKind::Read, |rule| rule.operation),
                effect: rule.map_or(PermissionEffect::Allow, |rule| rule.effect),
                pattern: rule.map_or_else(String::new, |rule| rule.pattern.clone()),
                focus: 0,
                error: None,
            }
        } else {
            Mode::ValueForm {
                name,
                kind,
                original,
                value: original
                    .and_then(|index| kind.string_value(profile, index))
                    .unwrap_or_default(),
                error: None,
            }
        }
    }

    fn handle_value_form_key(
        &mut self,
        key: KeyEvent,
        config: &mut ProgrammerConfig,
    ) -> PanelAction {
        let Mode::ValueForm {
            name,
            kind,
            original,
            value,
            error,
        } = &mut self.mode
        else {
            unreachable!("value form handler called outside value form mode");
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Collection {
                    name: name.clone(),
                    kind: *kind,
                    selected: original.unwrap_or(0),
                };
                PanelAction::None
            }
            KeyCode::Backspace => {
                value.pop();
                *error = None;
                PanelAction::None
            }
            KeyCode::Char(character) => {
                value.push(character);
                *error = None;
                PanelAction::None
            }
            KeyCode::Enter => {
                let normalized = value.trim();
                if normalized.is_empty() {
                    *error = Some(format!("{} cannot be empty", kind.singular()));
                    return PanelAction::None;
                }
                if *kind == CollectionKind::DeniedEnvironment
                    && let Err(parse_error) = Glob::new(normalized)
                {
                    *error = Some(format!("invalid glob: {parse_error}"));
                    return PanelAction::None;
                }
                let profile = config
                    .security_profiles
                    .get_mut(name)
                    .expect("edited security profile must exist");
                kind.set_string(profile, *original, normalized.to_string());
                let selected = original.unwrap_or_else(|| kind.len(profile).saturating_sub(1));
                let name = name.clone();
                let kind = *kind;
                self.mode = Mode::Collection {
                    name: name.clone(),
                    kind,
                    selected,
                };
                action_after_profile_change(config, &name)
            }
            _ => PanelAction::None,
        }
    }

    fn handle_rule_form_key(
        &mut self,
        key: KeyEvent,
        config: &mut ProgrammerConfig,
    ) -> PanelAction {
        let Mode::RuleForm {
            name,
            original,
            operation,
            effect,
            pattern,
            focus,
            error,
        } = &mut self.mode
        else {
            unreachable!("rule form handler called outside rule form mode");
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Collection {
                    name: name.clone(),
                    kind: CollectionKind::Rules,
                    selected: original.unwrap_or(0),
                };
                PanelAction::None
            }
            KeyCode::Tab | KeyCode::Down => {
                *focus = (*focus + 1) % 3;
                PanelAction::None
            }
            KeyCode::BackTab | KeyCode::Up => {
                *focus = (*focus + 2) % 3;
                PanelAction::None
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if *focus == 0 => {
                *operation = next_access(*operation, matches!(key.code, KeyCode::Left));
                PanelAction::None
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if *focus == 1 => {
                *effect = match effect {
                    PermissionEffect::Allow => PermissionEffect::Deny,
                    PermissionEffect::Deny => PermissionEffect::Allow,
                };
                PanelAction::None
            }
            KeyCode::Backspace if *focus == 2 => {
                pattern.pop();
                *error = None;
                PanelAction::None
            }
            KeyCode::Char(character) if *focus == 2 => {
                pattern.push(character);
                *error = None;
                PanelAction::None
            }
            KeyCode::Enter => {
                let normalized = pattern.trim();
                if normalized.is_empty() {
                    *error = Some("pattern cannot be empty".to_string());
                    return PanelAction::None;
                }
                if let Err(parse_error) = Glob::new(normalized) {
                    *error = Some(format!("invalid glob: {parse_error}"));
                    return PanelAction::None;
                }
                let rule = PermissionRule {
                    operation: *operation,
                    pattern: normalized.to_string(),
                    effect: *effect,
                };
                let profile = config
                    .security_profiles
                    .get_mut(name)
                    .expect("edited security profile must exist");
                set_vec_item(&mut profile.rules, *original, rule);
                let selected = original.unwrap_or_else(|| profile.rules.len().saturating_sub(1));
                let name = name.clone();
                self.mode = Mode::Collection {
                    name: name.clone(),
                    kind: CollectionKind::Rules,
                    selected,
                };
                action_after_profile_change(config, &name)
            }
            _ => PanelAction::None,
        }
    }

    pub fn render(&self, config: &ProgrammerConfig, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let chunks = panel_chunks(area);

        Paragraph::new(Line::from(vec![
            Span::styled(
                " Security profiles ",
                Style::default().fg(role::HEADING).bold(),
            ),
            Span::styled(
                format!(
                    "{} configured · active: {}",
                    config.security_profiles.len(),
                    config.active_security_profile
                ),
                Style::default().fg(palette::FAINT),
            ),
        ]))
        .render(chunks[0], buf);

        match &self.mode {
            Mode::List | Mode::Name { .. } | Mode::ConfirmDelete(_) => {
                self.render_list(config, chunks[1], buf)
            }
            Mode::Edit { name, selected } => {
                self.render_editor(config, name, *selected, chunks[1], buf)
            }
            Mode::Collection {
                name,
                kind,
                selected,
            } => self.render_collection(config, name, *kind, *selected, chunks[1], buf),
            Mode::ValueForm {
                name,
                kind,
                original,
                ..
            } => self.render_collection(config, name, *kind, original.unwrap_or(0), chunks[1], buf),
            Mode::RuleForm { name, original, .. } => self.render_collection(
                config,
                name,
                CollectionKind::Rules,
                original.unwrap_or(0),
                chunks[1],
                buf,
            ),
        }
        self.render_help(config, chunks[2], area, buf);
    }

    fn render_list(&self, config: &ProgrammerConfig, area: Rect, buf: &mut Buffer) {
        let names = self.filtered_names(config);
        let items = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let profile = &config.security_profiles[name];
                let active = config.active_security_profile == *name;
                let marker = if active { "●" } else { " " };
                let marker_style = if active {
                    Style::default().fg(palette::GREEN)
                } else {
                    Style::default()
                };
                let item = ListItem::new(Line::from(vec![
                    Span::styled(format!(" {marker} "), marker_style),
                    Span::styled(
                        format!("{name:<24}"),
                        Style::default().fg(palette::TEXT).bold(),
                    ),
                    Span::styled(
                        format!(
                            "{:<12} filesystem {} · {} rules",
                            SandboxMode::from_config(&profile.sandbox).label(),
                            state_label(profile.enabled),
                            profile.rules.len()
                        ),
                        Style::default().fg(palette::MUTED),
                    ),
                ]));
                if let Some(style) = self.mouse_style(MouseTarget::Row(index)) {
                    item.style(style)
                } else {
                    item
                }
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.selected.min(items.len() - 1)));
        }
        ratatui::widgets::StatefulWidget::render(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(role::BORDER)),
                )
                .highlight_symbol("❯")
                .highlight_style(
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(role::SELECTION_BG)
                        .add_modifier(Modifier::BOLD),
                ),
            area,
            buf,
            &mut state,
        );
    }

    fn render_editor(
        &self,
        config: &ProgrammerConfig,
        name: &str,
        selected: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let profile = &config.security_profiles[name];
        let items = SecuritySetting::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, setting)| {
                let style = if index == selected {
                    Style::default().fg(palette::BLUE).bold()
                } else {
                    Style::default().fg(palette::TEXT)
                };
                let value = setting.value(profile);
                let value_style = setting
                    .boolean_value(profile)
                    .map(setting_style)
                    .unwrap_or_else(|| Style::default().fg(palette::MUTED));
                let item = ListItem::new(Line::from(vec![
                    Span::styled(format!(" {:<24}", setting.label()), style),
                    Span::styled(value, value_style),
                ]));
                if let Some(style) = self.mouse_style(MouseTarget::Row(index)) {
                    item.style(style)
                } else {
                    item
                }
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(Some(selected));
        ratatui::widgets::StatefulWidget::render(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(role::BORDER))
                        .title(format!(" Edit {name} ")),
                )
                .highlight_symbol("❯")
                .highlight_style(
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(role::SELECTION_BG)
                        .add_modifier(Modifier::BOLD),
                ),
            area,
            buf,
            &mut state,
        );
    }

    fn render_collection(
        &self,
        config: &ProgrammerConfig,
        name: &str,
        kind: CollectionKind,
        selected: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let profile = &config.security_profiles[name];
        let values = kind.values(profile);
        let items = if values.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                format!(" No {} entries. Press 'a' to add one.", kind.singular()),
                Style::default().fg(palette::FAINT).italic(),
            )))]
        } else {
            values
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    let item = ListItem::new(Line::from(Span::styled(
                        format!(" {value}"),
                        Style::default().fg(palette::TEXT),
                    )));
                    if let Some(style) = self.mouse_style(MouseTarget::Row(index)) {
                        item.style(style)
                    } else {
                        item
                    }
                })
                .collect()
        };
        let mut state = ListState::default();
        if kind.len(profile) > 0 {
            state.select(Some(selected.min(kind.len(profile) - 1)));
        }
        ratatui::widgets::StatefulWidget::render(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(role::BORDER))
                        .title(format!(" {} · {name} ", kind.title())),
                )
                .highlight_symbol("❯")
                .highlight_style(
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(role::SELECTION_BG)
                        .add_modifier(Modifier::BOLD),
                ),
            area,
            buf,
            &mut state,
        );
    }

    fn render_help(
        &self,
        config: &ProgrammerConfig,
        bottom: Rect,
        full_area: Rect,
        buf: &mut Buffer,
    ) {
        match &self.mode {
            Mode::List => {
                Paragraph::new(Line::from(vec![
                    self.action_key("Enter", MouseTarget::Activate),
                    self.action_hint(" activate  ", MouseTarget::Activate),
                    self.action_key("a", MouseTarget::Primary),
                    self.action_hint(" clone  ", MouseTarget::Primary),
                    self.action_key("e", MouseTarget::Secondary),
                    self.action_hint(" edit  ", MouseTarget::Secondary),
                    self.action_key("r", MouseTarget::Quaternary),
                    self.action_hint(" rename  ", MouseTarget::Quaternary),
                    self.action_key("d", MouseTarget::Tertiary),
                    self.action_hint(" delete  ", MouseTarget::Tertiary),
                    self.action_key("/", MouseTarget::Search),
                    self.action_hint(" search  ", MouseTarget::Search),
                    self.action_key("q/Esc", MouseTarget::Back),
                    self.action_hint(" close", MouseTarget::Back),
                ]))
                .render(bottom, buf);
            }
            Mode::Edit { .. } => {
                Paragraph::new(Line::from(vec![
                    key("↑↓"),
                    hint(" select  "),
                    self.action_key("←→/Space/Enter", MouseTarget::Primary),
                    self.action_hint(" change/open  ", MouseTarget::Primary),
                    self.action_key("Esc", MouseTarget::Back),
                    self.action_hint(" back", MouseTarget::Back),
                ]))
                .render(bottom, buf);
            }
            Mode::Collection { .. } => {
                Paragraph::new(Line::from(vec![
                    key("↑↓"),
                    hint(" select  "),
                    self.action_key("a", MouseTarget::Primary),
                    self.action_hint(" add  ", MouseTarget::Primary),
                    self.action_key("e/Enter", MouseTarget::Secondary),
                    self.action_hint(" edit  ", MouseTarget::Secondary),
                    self.action_key("d", MouseTarget::Tertiary),
                    self.action_hint(" delete  ", MouseTarget::Tertiary),
                    self.action_key("Esc", MouseTarget::Back),
                    self.action_hint(" back", MouseTarget::Back),
                ]))
                .render(bottom, buf);
            }
            Mode::ValueForm {
                kind,
                original,
                value,
                error,
                ..
            } => {
                let action = if original.is_some() { "Edit" } else { "Add" };
                let mut lines = vec![Line::from(vec![
                    Span::styled(
                        format!(" {}: ", capitalize(kind.singular())),
                        Style::default().fg(palette::MUTED),
                    ),
                    Span::styled(value, Style::default().fg(palette::TEXT)),
                    Span::styled("▏", Style::default().fg(palette::BLUE)),
                ])];
                if let Some(error) = error {
                    lines.push(Line::from(Span::styled(
                        format!(" {error}"),
                        Style::default().fg(palette::RED),
                    )));
                }
                lines.push(Line::from(vec![
                    self.action_key("Enter", MouseTarget::Save),
                    self.action_hint(" save  ", MouseTarget::Save),
                    self.action_key("Esc", MouseTarget::Cancel),
                    self.action_hint(" cancel", MouseTarget::Cancel),
                ]));
                render_overlay(
                    &format!(" {action} {} ", kind.singular()),
                    lines,
                    full_area,
                    buf,
                );
            }
            Mode::RuleForm {
                original,
                operation,
                effect,
                pattern,
                focus,
                error,
                ..
            } => {
                let action = if original.is_some() { "Edit" } else { "Add" };
                let fields = [
                    ("Operation", access_label(*operation).to_string()),
                    ("Effect", effect_label(*effect).to_string()),
                    ("Pattern", pattern.clone()),
                ];
                let mut lines = fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, value))| {
                        let selected = index == *focus;
                        let line = Line::from(vec![
                            Span::styled(
                                if selected { "❯ " } else { "  " },
                                Style::default().fg(palette::BLUE),
                            ),
                            Span::styled(
                                format!("{label:<10}: "),
                                if selected {
                                    Style::default().fg(palette::BLUE).bold()
                                } else {
                                    Style::default().fg(palette::MUTED)
                                },
                            ),
                            Span::styled(
                                if value.is_empty() {
                                    "(empty)".to_string()
                                } else {
                                    value
                                },
                                Style::default().fg(palette::TEXT),
                            ),
                            Span::styled(
                                if selected && index == 2 { "▏" } else { "" },
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
                if let Some(error) = error {
                    lines.push(Line::from(Span::styled(
                        format!(" {error}"),
                        Style::default().fg(palette::RED),
                    )));
                }
                lines.push(Line::from(vec![
                    key("Tab/↑↓"),
                    hint(" field  "),
                    key("←→/Space"),
                    hint(" change  "),
                    self.action_key("Enter", MouseTarget::Save),
                    self.action_hint(" save  ", MouseTarget::Save),
                    self.action_key("Esc", MouseTarget::Cancel),
                    self.action_hint(" cancel", MouseTarget::Cancel),
                ]));
                render_overlay(
                    &format!(" {action} permission rule "),
                    lines,
                    full_area,
                    buf,
                );
            }
            Mode::Name {
                action,
                value,
                error,
            } => {
                let title = match action {
                    NameAction::Create { .. } => " Clone security profile ",
                    NameAction::Rename { .. } => " Rename security profile ",
                };
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(" Name: ", Style::default().fg(palette::MUTED)),
                        Span::styled(value, Style::default().fg(palette::TEXT)),
                        Span::styled("▏", Style::default().fg(palette::BLUE)),
                    ]),
                    Line::from(vec![
                        self.action_key("Enter", MouseTarget::Save),
                        self.action_hint(" save  ", MouseTarget::Save),
                        self.action_key("Esc", MouseTarget::Cancel),
                        self.action_hint(" cancel", MouseTarget::Cancel),
                    ]),
                ];
                if let Some(error) = error {
                    lines.push(Line::from(Span::styled(
                        format!(" {error}"),
                        Style::default().fg(palette::RED),
                    )));
                }
                render_overlay(title, lines, full_area, buf);
            }
            Mode::ConfirmDelete(name) => {
                let blocked =
                    config.security_profiles.len() == 1 || config.active_security_profile == *name;
                let message = if config.security_profiles.len() == 1 {
                    "The last profile cannot be deleted.".to_string()
                } else if config.active_security_profile == *name {
                    "Activate another profile before deleting this one.".to_string()
                } else {
                    format!("Delete security profile '{name}'?")
                };
                let mut lines = vec![Line::from(Span::styled(
                    message,
                    Style::default().fg(palette::YELLOW).bold(),
                ))];
                if !blocked {
                    lines.push(Line::from(vec![
                        self.action_key("y", MouseTarget::Save),
                        self.action_hint(" yes  ", MouseTarget::Save),
                        self.action_key("n", MouseTarget::Cancel),
                        self.action_hint(" no", MouseTarget::Cancel),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        self.action_key("Esc", MouseTarget::Cancel),
                        self.action_hint(" back", MouseTarget::Cancel),
                    ]));
                }
                render_overlay(" Delete security profile ", lines, full_area, buf);
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
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area)
}

fn security_overlay(area: Rect, line_count: usize) -> Rect {
    let width = area.width.saturating_sub(4).min(64);
    let height = (line_count as u16 + 2).min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn state_label(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn setting_style(enabled: bool) -> Style {
    if enabled {
        Style::default().fg(palette::GREEN)
    } else {
        Style::default().fg(palette::RED)
    }
}

fn action_after_profile_change(config: &mut ProgrammerConfig, name: &str) -> PanelAction {
    if config.active_security_profile == name {
        config.security = config.security_profiles[name].clone();
        PanelAction::Apply
    } else {
        PanelAction::Saved
    }
}

fn display_paths(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn set_vec_item<T>(values: &mut Vec<T>, original: Option<usize>, value: T) {
    if let Some(index) = original {
        values[index] = value;
    } else {
        values.push(value);
    }
}

fn access_label(access: AccessKind) -> &'static str {
    match access {
        AccessKind::Read => "read",
        AccessKind::Write => "write",
        AccessKind::Execute => "execute",
        AccessKind::Network => "network",
    }
}

fn next_access(access: AccessKind, reverse: bool) -> AccessKind {
    match (access, reverse) {
        (AccessKind::Read, false) | (AccessKind::Execute, true) => AccessKind::Write,
        (AccessKind::Write, false) | (AccessKind::Network, true) => AccessKind::Execute,
        (AccessKind::Execute, false) | (AccessKind::Read, true) => AccessKind::Network,
        (AccessKind::Network, false) | (AccessKind::Write, true) => AccessKind::Read,
    }
}

fn effect_label(effect: PermissionEffect) -> &'static str {
    match effect {
        PermissionEffect::Allow => "allow",
        PermissionEffect::Deny => "deny",
    }
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn key(label: &'static str) -> Span<'static> {
    Span::styled(label, Style::default().fg(palette::BLUE).bold())
}

fn hint(label: &'static str) -> Span<'static> {
    Span::styled(label, Style::default().fg(palette::MUTED))
}

fn render_overlay(title: &str, lines: Vec<Line<'_>>, area: Rect, buf: &mut Buffer) {
    let overlay = security_overlay(area, lines.len());
    Clear.render(overlay, buf);
    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(role::FOCUS))
                .title_style(Style::default().fg(role::FOCUS).bold())
                .title(title),
        )
        .render(overlay, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use unicode_width::UnicodeWidthStr;

    fn key_event(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_requires_matching_release_and_scopes_wheel_to_list() {
        let area = Rect::new(0, 0, 120, 12);
        let chunks = panel_chunks(area);
        let list = Block::default().borders(Borders::ALL).inner(chunks[1]);
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        config
            .security_profiles
            .insert("network".to_string(), SecurityConfig::default());
        let mut panel = SecurityPanel::new();

        assert_eq!(
            panel.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0), area, &mut config),
            PanelAction::None
        );
        assert_eq!(panel.selected, 0);
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::ScrollDown, list.x, list.y),
                area,
                &mut config,
            ),
            PanelAction::None
        );
        assert_eq!(panel.selected, 1);

        let footer = chunks[2];
        let clone_x = footer.x + "Enter activate  ".width() as u16;
        let edit_x = clone_x + "a clone  ".width() as u16;
        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), clone_x, footer.y),
            area,
            &mut config,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), edit_x, footer.y),
                area,
                &mut config,
            ),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::List));
    }

    #[test]
    fn mouse_delete_only_confirms_on_matching_button_release() {
        let area = Rect::new(0, 0, 120, 12);
        let chunks = panel_chunks(area);
        let footer = chunks[2];
        let delete_x = footer.x
            + ("Enter activate  ".width()
                + "a clone  ".width()
                + "e edit  ".width()
                + "r rename  ".width()) as u16;
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        config
            .security_profiles
            .insert("network".to_string(), SecurityConfig::default());
        let mut panel = SecurityPanel::new();
        panel.selected = 1;

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), delete_x, footer.y),
            area,
            &mut config,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), delete_x, footer.y),
                area,
                &mut config,
            ),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::ConfirmDelete(_)));

        let overlay = security_overlay(area, 2);
        let yes_x = overlay.x + 1;
        let no_x = yes_x + "y yes  ".width() as u16;
        let action_y = overlay.y + 2;
        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), yes_x, action_y),
            area,
            &mut config,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), no_x, action_y),
                area,
                &mut config,
            ),
            PanelAction::None
        );
        assert!(config.security_profiles.contains_key("network"));
        assert!(matches!(panel.mode, Mode::ConfirmDelete(_)));

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), yes_x, action_y),
            area,
            &mut config,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), yes_x, action_y),
                area,
                &mut config,
            ),
            PanelAction::Saved
        );
        assert!(!config.security_profiles.contains_key("network"));
    }

    #[test]
    fn value_form_error_keeps_mouse_actions_on_rendered_row() {
        let area = Rect::new(0, 0, 80, 12);
        let config = ProgrammerConfig::default();
        let panel = SecurityPanel {
            mode: Mode::ValueForm {
                name: "default".to_string(),
                kind: CollectionKind::DeniedEnvironment,
                original: None,
                value: "[".to_string(),
                error: Some("invalid pattern".to_string()),
            },
            ..SecurityPanel::new()
        };
        let overlay = security_overlay(area, 3);
        assert_eq!(
            panel.mouse_target(overlay.x + 1, overlay.y + 3, area, &config),
            Some(MouseTarget::Save)
        );
        assert_eq!(
            panel.mouse_target(
                overlay.x + 1 + "Enter save  ".width() as u16,
                overlay.y + 3,
                area,
                &config,
            ),
            Some(MouseTarget::Cancel)
        );
    }

    #[test]
    fn enter_activates_selected_profile() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        let mut network = config.security.clone();
        network.sandbox.network = true;
        config
            .security_profiles
            .insert("network".to_string(), network.clone());
        let mut panel = SecurityPanel::new();
        panel.selected = 1;

        let action = panel.handle_key(key_event(KeyCode::Enter), &mut config);

        assert_eq!(action, PanelAction::Apply);
        assert_eq!(config.active_security_profile, "network");
        assert_eq!(config.security, network);
    }

    #[test]
    fn active_profile_cannot_be_deleted() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        config
            .security_profiles
            .insert("network".to_string(), SecurityConfig::default());
        let mut panel = SecurityPanel::new();

        panel.handle_key(key_event(KeyCode::Char('d')), &mut config);
        let action = panel.handle_key(key_event(KeyCode::Char('y')), &mut config);

        assert_eq!(action, PanelAction::None);
        assert!(config.security_profiles.contains_key("default"));
    }

    #[test]
    fn editing_active_profile_updates_runtime_copy() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        let mut panel = SecurityPanel::new();
        let previous = config.security.sandbox.enabled;

        panel.handle_key(key_event(KeyCode::Char('e')), &mut config);
        let action = panel.handle_key(key_event(KeyCode::Char(' ')), &mut config);

        assert_eq!(action, PanelAction::Apply);
        assert_ne!(config.security.sandbox.enabled, previous);
        assert_eq!(
            config.security_profiles["default"].sandbox.enabled,
            config.security.sandbox.enabled
        );
    }

    #[test]
    fn collection_editors_cover_every_list_config_field() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        config.security.rules.clear();
        config.security.sandbox.readable_paths.clear();
        config.security.sandbox.writable_paths.clear();
        config.security.sandbox.denied_read_paths.clear();
        config.security.sandbox.denied_environment.clear();
        config
            .security_profiles
            .insert("default".to_string(), config.security.clone());
        let mut panel = SecurityPanel::new();
        let entries = [
            (CollectionKind::ReadablePaths, "/opt/read-only"),
            (CollectionKind::WritablePaths, "/tmp/writable"),
            (CollectionKind::DeniedReadPaths, "~/.secrets"),
            (CollectionKind::DeniedEnvironment, "BUILD_*"),
        ];

        for (kind, value) in entries {
            panel.mode = Mode::ValueForm {
                name: "default".to_string(),
                kind,
                original: None,
                value: value.to_string(),
                error: None,
            };
            assert_eq!(
                panel.handle_key(key_event(KeyCode::Enter), &mut config),
                PanelAction::Apply
            );
        }
        panel.mode = Mode::RuleForm {
            name: "default".to_string(),
            original: None,
            operation: AccessKind::Write,
            effect: PermissionEffect::Deny,
            pattern: "**/*.pem".to_string(),
            focus: 2,
            error: None,
        };
        assert_eq!(
            panel.handle_key(key_event(KeyCode::Enter), &mut config),
            PanelAction::Apply
        );

        let security = &config.security;
        assert_eq!(
            security.sandbox.readable_paths,
            vec![PathBuf::from("/opt/read-only")]
        );
        assert_eq!(
            security.sandbox.writable_paths,
            vec![PathBuf::from("/tmp/writable")]
        );
        assert_eq!(
            security.sandbox.denied_read_paths,
            vec![PathBuf::from("~/.secrets")]
        );
        assert_eq!(security.sandbox.denied_environment, vec!["BUILD_*"]);
        assert_eq!(
            security.rules,
            vec![PermissionRule {
                operation: AccessKind::Write,
                pattern: "**/*.pem".to_string(),
                effect: PermissionEffect::Deny,
            }]
        );
        assert_eq!(config.security_profiles["default"], config.security);
    }

    #[test]
    fn invalid_globs_stay_in_the_editor_and_do_not_change_config() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        let original = config.security.clone();
        let mut panel = SecurityPanel {
            mode: Mode::ValueForm {
                name: "default".to_string(),
                kind: CollectionKind::DeniedEnvironment,
                original: None,
                value: "[".to_string(),
                error: None,
            },
            ..SecurityPanel::new()
        };

        assert_eq!(
            panel.handle_key(key_event(KeyCode::Enter), &mut config),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::ValueForm { error: Some(_), .. }));
        assert_eq!(config.security, original);

        panel.mode = Mode::RuleForm {
            name: "default".to_string(),
            original: None,
            operation: AccessKind::Read,
            effect: PermissionEffect::Allow,
            pattern: "[".to_string(),
            focus: 2,
            error: None,
        };
        assert_eq!(
            panel.handle_key(key_event(KeyCode::Enter), &mut config),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::RuleForm { error: Some(_), .. }));
        assert_eq!(config.security, original);
    }

    #[test]
    fn collection_items_can_be_edited_and_deleted() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        config.security.sandbox.writable_paths = vec![PathBuf::from("/old")];
        config
            .security_profiles
            .insert("default".to_string(), config.security.clone());
        let mut panel = SecurityPanel {
            mode: Mode::ValueForm {
                name: "default".to_string(),
                kind: CollectionKind::WritablePaths,
                original: Some(0),
                value: "/new".to_string(),
                error: None,
            },
            ..SecurityPanel::new()
        };

        assert_eq!(
            panel.handle_key(key_event(KeyCode::Enter), &mut config),
            PanelAction::Apply
        );
        assert_eq!(
            config.security.sandbox.writable_paths,
            vec![PathBuf::from("/new")]
        );
        assert_eq!(
            panel.handle_key(key_event(KeyCode::Char('d')), &mut config),
            PanelAction::Apply
        );
        assert!(config.security.sandbox.writable_paths.is_empty());
    }

    #[test]
    fn panel_render_identifies_the_active_profile() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        let panel = SecurityPanel::new();
        let area = Rect::new(0, 0, 80, 12);
        let mut buffer = Buffer::empty(area);

        panel.render(&config, area, &mut buffer);

        let rendered = buffer
            .content
            .chunks(area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Security profiles"));
        assert!(rendered.contains("active: default"));
        assert!(rendered.contains("●"));
    }
}
