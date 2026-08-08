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
use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) struct AgentPanel {
    pub(crate) id: u64,
    name: String,
    conversation: ConversationPanel,
}

impl AgentPanel {
    pub(crate) fn new(id: u64, name: String, conversation: Arc<Mutex<Conversation>>) -> Self {
        Self {
            id,
            name,
            conversation: ConversationPanel::from_shared(conversation),
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
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
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(
                " Sub-agent #{} — {}  [q/Esc close] ",
                self.id, self.name
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.conversation.render(inner, buf);
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.conversation.scroll_up_by(3),
            MouseEventKind::ScrollDown => self.conversation.scroll_down_by(3),
            MouseEventKind::Up(MouseButton::Left) => {
                self.conversation.handle_click(mouse.column, mouse.row)
            }
            _ => {}
        }
    }
}
