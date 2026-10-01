// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed user input and pure startup policy. Queue ownership and durable peer delivery stay with their sources.
use async_openai::types::responses::InputImageContent;

use super::App;

pub(crate) struct UserRequest {
    pub(crate) text: String,
    pub(crate) images: Vec<InputImageContent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkSource {
    Queued,
    Peer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartDecision {
    Wait,
    Start,
    CompactPeer,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct StartupState {
    pub(crate) active_turn: bool,
    pub(crate) blocking_surface: bool,
    pub(crate) text_draft: bool,
    pub(crate) pending_images: bool,
    pub(crate) queued_user: bool,
    pub(crate) active_compaction: bool,
    pub(crate) mandatory_waiting: bool,
    pub(crate) mandatory_compaction_due: bool,
}

impl StartupState {
    pub(crate) fn from_app(app: &App<'_>) -> Self {
        Self {
            active_turn: app.cancel.active_id.is_some(),
            blocking_surface: super::events::has_blocking_surface(app)
                || app.peers.consent.is_some(),
            text_draft: !app.input_panel.get_content().is_empty(),
            pending_images: !app.pending_images.is_empty(),
            queued_user: app.conversation_panel.pending_message.is_some(),
            active_compaction: app.auto_compact.active_id.is_some(),
            mandatory_waiting: app.auto_compact.mandatory_waiting,
            mandatory_compaction_due: app.mandatory_compact_tokens().is_some_and(|limit| {
                app.auto_compact
                    .last_input_tokens
                    .is_some_and(|tokens| tokens >= limit)
            }),
        }
    }

    pub(crate) fn decide(self, source: WorkSource) -> StartDecision {
        if self.active_turn || self.blocking_surface || self.text_draft {
            return StartDecision::Wait;
        }
        // Queued images belong to the user request, not an independent draft.
        // Its command path retains responsibility for mandatory compaction;
        // runtime updates retain their existing developer-role startup path.
        if source == WorkSource::Queued {
            return StartDecision::Start;
        }
        if self.active_compaction || self.pending_images || self.queued_user {
            return StartDecision::Wait;
        }
        // Never route peer input through the user-role mandatory queue.
        if self.mandatory_compaction_due {
            return StartDecision::CompactPeer;
        }
        if self.mandatory_waiting {
            return StartDecision::Wait;
        }
        StartDecision::Start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_policy_preserves_all_legacy_blocker_combinations() {
        for bits in 0..256 {
            let state = StartupState {
                active_turn: bits & 1 != 0,
                blocking_surface: bits & 2 != 0,
                text_draft: bits & 4 != 0,
                pending_images: bits & 8 != 0,
                queued_user: bits & 16 != 0,
                active_compaction: bits & 32 != 0,
                mandatory_waiting: bits & 64 != 0,
                mandatory_compaction_due: bits & 128 != 0,
            };
            let queued_ready = !state.active_turn && !state.blocking_surface && !state.text_draft;
            assert_eq!(
                state.decide(WorkSource::Queued) == StartDecision::Start,
                queued_ready
            );
            let peer_idle = queued_ready
                && !state.active_compaction
                && !state.pending_images
                && !state.queued_user;
            let expected_peer = if peer_idle && state.mandatory_compaction_due {
                StartDecision::CompactPeer
            } else if peer_idle && !state.mandatory_waiting {
                StartDecision::Start
            } else {
                StartDecision::Wait
            };
            assert_eq!(
                state.decide(WorkSource::Peer),
                expected_peer,
                "blockers: {bits}"
            );
        }
    }

    #[test]
    fn drafts_approvals_and_active_turns_block_every_source() {
        for state in [
            StartupState {
                active_turn: true,
                ..Default::default()
            },
            StartupState {
                blocking_surface: true,
                ..Default::default()
            },
            StartupState {
                text_draft: true,
                ..Default::default()
            },
        ] {
            for source in [WorkSource::Queued, WorkSource::Peer] {
                assert_eq!(state.decide(source), StartDecision::Wait);
            }
        }
    }

    #[test]
    fn queued_user_and_images_have_priority_over_peers() {
        for state in [
            StartupState {
                queued_user: true,
                ..Default::default()
            },
            StartupState {
                pending_images: true,
                ..Default::default()
            },
            StartupState {
                active_compaction: true,
                ..Default::default()
            },
        ] {
            assert_eq!(state.decide(WorkSource::Peer), StartDecision::Wait);
            assert_eq!(state.decide(WorkSource::Queued), StartDecision::Start);
        }
    }

    #[test]
    fn peer_compaction_never_converts_peer_input_to_user_input() {
        let state = StartupState {
            mandatory_compaction_due: true,
            ..Default::default()
        };
        assert_eq!(state.decide(WorkSource::Peer), StartDecision::CompactPeer);
        assert_eq!(state.decide(WorkSource::Queued), StartDecision::Start);
        assert_eq!(
            StartupState {
                mandatory_waiting: true,
                ..Default::default()
            }
            .decide(WorkSource::Peer),
            StartDecision::Wait
        );
        assert_eq!(
            StartupState::default().decide(WorkSource::Peer),
            StartDecision::Start
        );
    }
}
