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
use crossterm::event::{KeyCode, KeyEvent};
use globset::Glob;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
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

#[derive(Debug)]
pub struct SecurityPanel {
    mode: Mode,
    selected: usize,
    search: PanelSearch,
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
        }
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(5),
                Constraint::Length(2),
            ])
            .split(area);

        Paragraph::new(Line::from(vec![
            Span::styled(
                " Security profiles ",
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::styled(
                format!(
                    "{} configured · active: {}",
                    config.security_profiles.len(),
                    config.active_security_profile
                ),
                Style::default().fg(Color::DarkGray),
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
            .map(|name| {
                let profile = &config.security_profiles[name];
                let active = config.active_security_profile == *name;
                let marker = if active { "●" } else { " " };
                let marker_style = if active {
                    Style::default().fg(Color::LightGreen)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {marker} "), marker_style),
                    Span::styled(
                        format!("{name:<24}"),
                        Style::default().fg(Color::White).bold(),
                    ),
                    Span::styled(
                        format!(
                            "{:<12} filesystem {} · {} rules",
                            SandboxMode::from_config(&profile.sandbox).label(),
                            state_label(profile.enabled),
                            profile.rules.len()
                        ),
                        Style::default().fg(Color::Gray),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.selected.min(items.len() - 1)));
        }
        ratatui::widgets::StatefulWidget::render(
            List::new(items)
                .block(Block::default().borders(Borders::ALL))
                .highlight_symbol("❯")
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
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
                    Style::default().fg(Color::Cyan).bold()
                } else {
                    Style::default().fg(Color::White)
                };
                let value = setting.value(profile);
                let value_style = setting
                    .boolean_value(profile)
                    .map(setting_style)
                    .unwrap_or_else(|| Style::default().fg(Color::Gray));
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {:<24}", setting.label()), style),
                    Span::styled(value, value_style),
                ]))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(Some(selected));
        ratatui::widgets::StatefulWidget::render(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" Edit {name} ")),
                )
                .highlight_symbol("❯"),
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
                Style::default().fg(Color::DarkGray).italic(),
            )))]
        } else {
            values
                .into_iter()
                .map(|value| {
                    ListItem::new(Line::from(Span::styled(
                        format!(" {value}"),
                        Style::default().fg(Color::White),
                    )))
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
                        .title(format!(" {} · {name} ", kind.title())),
                )
                .highlight_symbol("❯")
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
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
                let mut spans = vec![
                    key("Enter"),
                    hint(" activate  "),
                    key("a"),
                    hint(" clone  "),
                    key("e"),
                    hint(" edit  "),
                    key("r"),
                    hint(" rename  "),
                    key("d"),
                    hint(" delete  "),
                ];
                spans.extend(PanelSearch::help_spans());
                spans.extend([key("q/Esc"), hint(" close")]);
                Paragraph::new(Line::from(spans)).render(bottom, buf);
            }
            Mode::Edit { .. } => {
                Paragraph::new(Line::from(vec![
                    key("↑↓"),
                    hint(" select  "),
                    key("←→/Space/Enter"),
                    hint(" change/open  "),
                    key("Esc"),
                    hint(" back"),
                ]))
                .render(bottom, buf);
            }
            Mode::Collection { .. } => {
                Paragraph::new(Line::from(vec![
                    key("↑↓"),
                    hint(" select  "),
                    key("a"),
                    hint(" add  "),
                    key("e/Enter"),
                    hint(" edit  "),
                    key("d"),
                    hint(" delete  "),
                    key("Esc"),
                    hint(" back"),
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
                let lines = form_lines(
                    vec![Line::from(vec![
                        Span::styled(
                            format!(" {}: ", capitalize(kind.singular())),
                            Style::default().fg(Color::Gray),
                        ),
                        Span::styled(value, Style::default().fg(Color::White)),
                        Span::styled("▏", Style::default().fg(Color::Cyan)),
                    ])],
                    error.as_deref(),
                );
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
                        Line::from(vec![
                            Span::styled(
                                if selected { "❯ " } else { "  " },
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::styled(
                                format!("{label:<10}: "),
                                if selected {
                                    Style::default().fg(Color::Cyan).bold()
                                } else {
                                    Style::default().fg(Color::Gray)
                                },
                            ),
                            Span::styled(
                                if value.is_empty() {
                                    "(empty)".to_string()
                                } else {
                                    value
                                },
                                Style::default().fg(Color::White),
                            ),
                            Span::styled(
                                if selected && index == 2 { "▏" } else { "" },
                                Style::default().fg(Color::Cyan),
                            ),
                        ])
                    })
                    .collect::<Vec<_>>();
                if let Some(error) = error {
                    lines.push(Line::from(Span::styled(
                        format!(" {error}"),
                        Style::default().fg(Color::Red),
                    )));
                }
                lines.push(Line::from(vec![
                    key("Tab/↑↓"),
                    hint(" field  "),
                    key("←→/Space"),
                    hint(" change  "),
                    key("Enter"),
                    hint(" save  "),
                    key("Esc"),
                    hint(" cancel"),
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
                        Span::styled(" Name: ", Style::default().fg(Color::Gray)),
                        Span::styled(value, Style::default().fg(Color::White)),
                        Span::styled("▏", Style::default().fg(Color::Cyan)),
                    ]),
                    Line::from(vec![
                        key("Enter"),
                        hint(" save  "),
                        key("Esc"),
                        hint(" cancel"),
                    ]),
                ];
                if let Some(error) = error {
                    lines.push(Line::from(Span::styled(
                        format!(" {error}"),
                        Style::default().fg(Color::Red),
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
                    Style::default().fg(Color::Yellow).bold(),
                ))];
                if !blocked {
                    lines.push(Line::from(vec![
                        key("y"),
                        hint(" yes  "),
                        key("n"),
                        hint(" no"),
                    ]));
                } else {
                    lines.push(Line::from(vec![key("Esc"), hint(" back")]));
                }
                render_overlay(" Delete security profile ", lines, full_area, buf);
            }
        }
    }
}

fn state_label(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn setting_style(enabled: bool) -> Style {
    if enabled {
        Style::default().fg(Color::LightGreen)
    } else {
        Style::default().fg(Color::LightRed)
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

fn form_lines<'a>(mut lines: Vec<Line<'a>>, error: Option<&'a str>) -> Vec<Line<'a>> {
    if let Some(error) = error {
        lines.push(Line::from(Span::styled(
            format!(" {error}"),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(vec![
        key("Enter"),
        hint(" save  "),
        key("Esc"),
        hint(" cancel"),
    ]));
    lines
}

fn key(label: &'static str) -> Span<'static> {
    Span::styled(label, Style::default().fg(Color::Cyan).bold())
}

fn hint(label: &'static str) -> Span<'static> {
    Span::styled(label, Style::default().fg(Color::Gray))
}

fn render_overlay(title: &str, lines: Vec<Line<'_>>, area: Rect, buf: &mut Buffer) {
    let width = area.width.saturating_sub(4).min(64);
    let height = (lines.len() as u16 + 2).min(area.height);
    let overlay = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    Clear.render(overlay, buf);
    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .render(overlay, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key_event(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
            selected: 0,
            search: PanelSearch::default(),
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
            selected: 0,
            search: PanelSearch::default(),
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
