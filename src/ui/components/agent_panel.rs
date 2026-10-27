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

//! Full-screen, read-only view of a sub-agent's live conversation.

use crate::conversation::Conversation;
use crate::ui::components::conversation_panel::conversation_panel::{
    ConversationPanel, SelectionEnd,
};
use crate::ui::markdown_theme::palette;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Widget};
use std::sync::{Arc, Mutex};

pub(crate) struct AgentPanel {
    pub(crate) id: u64,
    name: String,
    conversation: ConversationPanel,
    image_viewer: Option<crate::ui::image_preview::ImageViewer>,
    click_consumed: bool,
}

impl AgentPanel {
    pub(crate) fn new(
        tasks: crate::tasks::TaskManager,
        id: u64,
        name: String,
        conversation: Arc<Mutex<Conversation>>,
    ) -> Self {
        let mut conversation = ConversationPanel::from_shared(conversation);
        conversation.tasks = tasks;
        Self {
            id,
            name,
            conversation,
            image_viewer: None,
            click_consumed: false,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if let Some(viewer) = self.image_viewer.as_mut() {
            match key.code {
                KeyCode::Esc => self.image_viewer = None,
                KeyCode::Char('+' | '=') => viewer.zoom_in(),
                KeyCode::Char('-' | '_') => viewer.zoom_out(),
                KeyCode::Char('0') => viewer.reset(),
                KeyCode::Left => viewer.pan_by(-2, 0),
                KeyCode::Right => viewer.pan_by(2, 0),
                KeyCode::Up => viewer.pan_by(0, -1),
                KeyCode::Down => viewer.pan_by(0, 1),
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Up | KeyCode::Char('k') => self.conversation.scroll_up(),
            KeyCode::Down | KeyCode::Char('j') => self.conversation.scroll_down(),
            KeyCode::PageUp => self.conversation.scroll_up_by(10),
            KeyCode::PageDown => self.conversation.scroll_down_by(10),
            KeyCode::Home => self.conversation.scroll_to_top(),
            KeyCode::End => self.conversation.scroll_to_bottom(),
            _ => {}
        }
        false
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BLUE))
            .title(format!(
                " Sub-agent #{} — {}  [q/Esc close] ",
                self.id, self.name
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.conversation.render(inner, buf);
        if let Some(viewer) = self.image_viewer.as_mut() {
            if viewer.requires_exclusive_render() {
                // Kitty upload chunks must not be interleaved with protocol
                // images from the underlying child conversation.
                Clear.render(area, buf);
            }
            viewer.render(area, buf);
        }
    }

    pub(crate) fn interaction_animation_active(&self) -> bool {
        self.conversation.interaction_animation_active()
            || self
                .image_viewer
                .as_ref()
                .is_some_and(|viewer| viewer.render_pending())
    }

    pub(crate) fn image_pointer_key(&self) -> Option<Option<usize>> {
        if self.image_viewer.is_some() {
            Some(None)
        } else {
            self.conversation.hovered_image_id().map(Some)
        }
    }

    pub(crate) fn cancel_pointer_gesture(&mut self) {
        self.click_consumed = false;
        self.conversation.cancel_pointer_gesture();
        if let Some(viewer) = self.image_viewer.as_mut() {
            viewer.end_drag();
        }
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<(), String> {
        if let Some(viewer) = self.image_viewer.as_mut() {
            match mouse.kind {
                MouseEventKind::ScrollUp => viewer.zoom_in(),
                MouseEventKind::ScrollDown => viewer.zoom_out(),
                MouseEventKind::Down(MouseButton::Left) => {
                    viewer.begin_drag(mouse.column, mouse.row)
                }
                MouseEventKind::Drag(MouseButton::Left) => viewer.drag_to(mouse.column, mouse.row),
                MouseEventKind::Up(MouseButton::Left) => viewer.end_drag(),
                _ => {}
            }
            return Ok(());
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.conversation.scroll_up_by(3),
            MouseEventKind::ScrollDown => self.conversation.scroll_down_by(3),
            MouseEventKind::Moved => self.conversation.pointer_moved(mouse.column, mouse.row),
            MouseEventKind::Down(MouseButton::Left) => {
                self.conversation.pointer_down(mouse.column, mouse.row);
                if self
                    .conversation
                    .begin_scrollbar_drag(mouse.column, mouse.row)
                {
                    self.click_consumed = true;
                } else {
                    self.click_consumed =
                        self.conversation.jump_button_hit(mouse.column, mouse.row);
                    if !self.click_consumed {
                        self.conversation.selection_begin(mouse.column, mouse.row);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.conversation.scrollbar_dragging() => {
                self.conversation.drag_scrollbar(mouse.row);
            }
            MouseEventKind::Drag(MouseButton::Left) if !self.click_consumed => {
                self.conversation.pointer_dragged(mouse.column, mouse.row);
                self.conversation.selection_drag(mouse.column, mouse.row);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.conversation.end_scrollbar_drag() {
                    self.conversation.pointer_up(mouse.column, mouse.row);
                    self.click_consumed = false;
                    return Ok(());
                }
                if self.click_consumed {
                    let jump = self
                        .conversation
                        .jump_button_activates_at(mouse.column, mouse.row);
                    self.conversation.pointer_up(mouse.column, mouse.row);
                    self.click_consumed = false;
                    if jump {
                        self.conversation.scroll_to_bottom();
                    }
                    return Ok(());
                }
                let selection_end = self.conversation.selection_end(mouse.column, mouse.row);
                self.conversation.pointer_up(mouse.column, mouse.row);
                match selection_end {
                    SelectionEnd::Click { column, row } => self
                        .conversation
                        .handle_buffer_click(column, row)
                        .map_err(str::to_string)?,
                    SelectionEnd::Link(url) => {
                        crate::ui::components::conversation_panel::links::open(&url)
                            .map_err(|error| format!("failed to open link: {error}"))?;
                    }
                    SelectionEnd::Copied(text) => {
                        if !crate::clipboard::copy(&text) {
                            return Err("failed to copy selection to clipboard".into());
                        }
                        self.conversation.show_copied();
                    }
                    SelectionEnd::Image(image_id) => {
                        self.image_viewer = self.conversation.image_viewer(image_id);
                        self.conversation.cancel_pointer_gesture();
                    }
                    SelectionEnd::Ignored => {}
                }
            }
            _ => {}
        }
        Ok(())
    }
}
