//! Compact, opt-in peer work consent; ordinary tool questions keep their UI.
use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use unicode_width::UnicodeWidthChar;

use super::{AnswerAction, QuestionPanel};
use crate::{
    tools::ask_user::{Question, QuestionKind},
    ui::event::AnswerTx,
};

pub(super) struct Delegation {
    source: String,
    model: String,
    models: Vec<crate::commands::CompletionCandidate>,
    picker: Option<String>,
    selected_model: usize,
    busy: bool,
    accept: bool,
    expanded: bool,
    scroll: Cell<usize>,
    page: Cell<usize>,
    max_scroll: Cell<usize>,
}

impl QuestionPanel {
    /// Peer consent returns exactly `Yes` or `No`; rejection is selected initially.
    /// Busy acceptance queues work rather than interrupting the current turn.
    pub fn delegation(
        source: impl Into<String>,
        body: impl Into<String>,
        busy: bool,
        tx: AnswerTx,
    ) -> Self {
        let mut panel = Self::new(
            Question {
                text: body.into(),
                kind: QuestionKind::Choice {
                    options: vec!["No".into(), "Yes".into()],
                    other_index: usize::MAX,
                },
            },
            tx,
        );
        panel.delegation = Some(Delegation {
            source: source.into(),
            model: String::new(),
            models: Vec::new(),
            picker: None,
            selected_model: 0,
            busy,
            accept: false,
            expanded: false,
            scroll: Cell::new(0),
            page: Cell::new(1),
            max_scroll: Cell::new(0),
        });
        panel
    }

    /// Refresh from the same live catalog used by `/model` completion.
    pub fn set_delegation_models(
        &mut self,
        model: &str,
        models: Vec<crate::commands::CompletionCandidate>,
    ) {
        if let Some(state) = &mut self.delegation {
            state.model = model.to_string();
            state.models = models;
            let count = state.filtered_models().len();
            state.selected_model = state.selected_model.min(count.saturating_sub(1));
        }
    }

    /// Width-aware sizing for callers that know the terminal width. Delegation
    /// stays between six and thirteen rows; the legacy method remains supported.
    pub fn needed_height_for_width(&self, width: u16) -> u16 {
        match &self.delegation {
            Some(state) if state.picker.is_some() => state.height(),
            Some(state) if state.expanded => {
                (wrap(&self.question.text, width).len().min(8) as u16 + 5).max(6)
            }
            Some(_) => 6,
            None => self.needed_height(),
        }
    }
}

impl Delegation {
    pub(super) fn height(&self) -> u16 {
        if self.picker.is_some() {
            10
        } else if self.expanded {
            13
        } else {
            6
        }
    }

    pub(super) fn paste(&mut self, text: &str) {
        if let Some(query) = &mut self.picker {
            query.extend(text.chars().filter(|ch| !ch.is_control()));
            self.selected_model = 0;
        }
    }

    fn filtered_models(&self) -> Vec<&crate::commands::CompletionCandidate> {
        let query = self.picker.as_deref().unwrap_or_default().to_lowercase();
        self.models
            .iter()
            .filter(|m| m.label.to_lowercase().contains(&query))
            .collect()
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> AnswerAction {
        if self.picker.is_some() {
            match key.code {
                KeyCode::Esc => self.picker = None,
                KeyCode::Enter => {
                    if let Some(model) = self.filtered_models().get(self.selected_model) {
                        let model = model.value.clone();
                        self.picker = None;
                        return AnswerAction::SelectModel(model);
                    }
                }
                KeyCode::Up => self.selected_model = self.selected_model.saturating_sub(1),
                KeyCode::Down => {
                    self.selected_model = (self.selected_model + 1)
                        .min(self.filtered_models().len().saturating_sub(1))
                }
                KeyCode::Backspace => {
                    self.picker.as_mut().unwrap().pop();
                    self.selected_model = 0;
                }
                KeyCode::Char(ch)
                    if !key.modifiers.intersects(
                        crossterm::event::KeyModifiers::CONTROL
                            | crossterm::event::KeyModifiers::ALT,
                    ) =>
                {
                    self.picker.as_mut().unwrap().push(ch);
                    self.selected_model = 0;
                }
                _ => {}
            }
            return AnswerAction::None;
        }
        match key.code {
            KeyCode::Char('m' | 'M') => {
                self.picker = Some(String::new());
                self.selected_model = self
                    .models
                    .iter()
                    .position(|m| m.value == self.model)
                    .unwrap_or(0);
            }
            KeyCode::Left => self.accept = false,
            KeyCode::Right => self.accept = true,
            KeyCode::Enter => {
                return AnswerAction::Answer(if self.accept { "Yes" } else { "No" }.into());
            }
            KeyCode::Esc => return AnswerAction::Answer("No".into()),
            KeyCode::Char('d' | 'D') => {
                self.expanded = !self.expanded;
                self.scroll.set(0);
            }
            KeyCode::PageDown if self.expanded => self.scroll.set(
                self.scroll
                    .get()
                    .saturating_add(self.page.get())
                    .min(self.max_scroll.get()),
            ),
            KeyCode::PageUp if self.expanded => self
                .scroll
                .set(self.scroll.get().saturating_sub(self.page.get())),
            _ => {}
        }
        AnswerAction::None
    }

    pub(super) fn render(&self, body: &str, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.is_empty() {
            return;
        }
        if let Some(query) = &self.picker {
            let row = |y| Rect::new(inner.x, y, inner.width, 1);
            Paragraph::new(format!("Session model / {query}")).render(row(inner.y), buf);
            if inner.height > 1 {
                Paragraph::new("↑↓ select  Enter apply  Esc back")
                    .render(row(inner.bottom() - 1), buf);
            }
            let list = Rect::new(
                inner.x,
                inner.y.saturating_add(1),
                inner.width,
                inner.height.saturating_sub(2),
            );
            let models = self.filtered_models();
            if models.is_empty() {
                if !list.is_empty() {
                    Paragraph::new("No matching models").render(list, buf);
                }
            } else {
                let visible = usize::from(list.height).max(1);
                let popup = crate::ui::components::completion_popup::CompletionPopup {
                    candidates: &models,
                    label: |model| model.label.as_str(),
                    selected: self.selected_model,
                    scroll_offset: self.selected_model.saturating_sub(visible - 1),
                };
                (&popup).render(list, buf);
            }
            return;
        }
        // Preserve the decision row even when the terminal is unusually short.
        let choices_y = inner
            .bottom()
            .saturating_sub(if inner.height >= 3 { 2 } else { 1 });
        let row = |y| Rect::new(inner.x, y, inner.width, 1);
        if choices_y > inner.y {
            Paragraph::new(format!(
                "Peer delegation · {}",
                self.source.replace(['\n', '\r'], " ")
            ))
            .style(Style::default().fg(Color::Cyan).bold())
            .render(row(inner.y), buf);
        }
        let body_y = inner.y.saturating_add(1);
        let model_y = choices_y.saturating_sub(1);
        if model_y > inner.y {
            Paragraph::new(format!("Model: {} (session)", self.model)).render(row(model_y), buf);
        }
        let body_height = model_y.saturating_sub(body_y);
        if body_height > 0 {
            let lines = wrap(body, inner.width);
            let max = lines.len().saturating_sub(body_height as usize);
            self.max_scroll.set(max);
            self.page.set(body_height as usize);
            let offset = if self.expanded {
                self.scroll.get().min(max)
            } else {
                0
            };
            self.scroll.set(offset);
            let mut visible: Vec<Line> = lines
                .into_iter()
                .skip(offset)
                .take(body_height as usize)
                .map(Line::from)
                .collect();
            if !self.expanded
                && max > 0
                && let Some(last) = visible.last_mut()
            {
                // Replace the last cell-width of text, never split UTF-8.
                let text = last.to_string();
                *last = Line::from(format!("{}…", clip(&text, inner.width.saturating_sub(1))));
            }
            Paragraph::new(visible)
                .render(Rect::new(inner.x, body_y, inner.width, body_height), buf);
        }
        let selected = Style::default().fg(Color::Black).bg(Color::Cyan).bold();
        let normal = Style::default().fg(Color::Gray);
        let yes = if self.busy {
            " Yes · queue "
        } else {
            " Yes · run tools "
        };
        Paragraph::new(Line::from(vec![
            Span::styled(" No ", if self.accept { normal } else { selected }),
            Span::raw("  "),
            Span::styled(yes, if self.accept { selected } else { normal }),
        ]))
        .render(row(choices_y), buf);
        if inner.height >= 3 {
            let hint = if inner.width < 48 {
                if self.expanded {
                    "←→ Enter Esc D M:model PgUp/Dn"
                } else {
                    "←→ Enter Esc D:more M:model"
                }
            } else if self.expanded {
                "←→ select  Enter confirm  Esc deny  D collapse  M model  PgUp/PgDn scroll"
            } else {
                "←→ select  Enter confirm  Esc deny  D expand  M model"
            };
            Paragraph::new(hint)
                .style(Style::default().fg(Color::DarkGray))
                .render(row(inner.bottom() - 1), buf);
        }
    }
}

// Character-width wrapping keeps CJK text readable even without word spaces.
// No byte-length casts, and no fixed-width assumptions in rendering.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let mut current = String::new();
        let mut used = 0;
        for ch in line.chars().filter(|ch| !ch.is_control()) {
            let cells = ch.width().unwrap_or(0);
            if used + cells > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push(ch);
            used += cells;
        }
        lines.push(current);
    }
    lines
}

fn clip(text: &str, width: u16) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|ch| {
            used += ch.width().unwrap_or(0);
            used <= usize::from(width)
        })
        .collect()
}
