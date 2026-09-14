//! One-time global provider and model-role configuration guide.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::path::PathBuf;

pub(crate) struct SetupGuide {
    marker: PathBuf,
}

impl SetupGuide {
    pub(crate) fn first_launch() -> Option<Self> {
        if cfg!(test) {
            return None;
        }
        let marker = dirs::config_dir()?
            .join("programmer")
            .join("interactive-setup-done");
        (!marker.exists()).then_some(Self { marker })
    }

    pub(crate) fn is_complete(
        &self,
        config: &crate::config::programmer_config::ProgrammerConfig,
    ) -> bool {
        config
            .providers
            .get(&config.default_provider)
            .and_then(|provider| provider.default_model.as_ref())
            .is_some()
            && config.classifier_model.is_some()
            && config.compact_model.is_some()
            && config.memory_model.is_some()
            && config.title_model.is_some()
            && config.suggestion_model.is_some()
    }

    pub(crate) fn mark_done(&self) {
        if let Some(parent) = self.marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.marker);
    }

    pub(crate) fn handle(&mut self, key: KeyEvent) -> SetupAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('S') => {
                self.mark_done();
                SetupAction::Close
            }
            KeyCode::Enter | KeyCode::Char('p') | KeyCode::Char('P') => SetupAction::Providers,
            _ => SetupAction::None,
        }
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        let text = "Welcome to Programmer\n\nFirst configure one or more providers, then choose a model for each purpose:\nchat, classifier, compact, memory, title, and suggestion.\n\nIn provider management, open a provider's models and press [Enter] to assign roles.\nUnassigned roles fall back to the chat model.\n\n[Enter/p] Configure providers and model roles    [s/Esc] Skip setup";
        Clear.render(area, buf);
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .title(" First-time setup ")
                    .borders(Borders::ALL),
            )
            .render(area, buf);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SetupAction {
    None,
    Close,
    Providers,
}
