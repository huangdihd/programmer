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

//! Foreground operation transitions. Cancellation retains ownership until the
//! terminal event arrives; dropping ownership early could start overlapping turns.

use super::CancelState;
use crate::cancel::{CancellationToken, OperationId};

impl CancelState {
    pub(crate) fn begin(&mut self, conversation_cutoff: Option<usize>) -> OperationId {
        assert!(
            self.active_id.is_none(),
            "cannot replace an active operation"
        );
        self.next_id = self.next_id.checked_add(1).expect("operation id exhausted");
        let operation_id = OperationId(self.next_id);
        self.active = CancellationToken::new();
        self.active_id = Some(operation_id);
        self.turn_conversation_cutoff = conversation_cutoff;
        self.response_started = false;
        self.activity = None;
        self.active_user_request = None;
        operation_id
    }

    pub(crate) fn is_current(&self, operation_id: OperationId) -> bool {
        operation_id.is_current(self.active_id)
    }

    pub(crate) fn is_live(&self, operation_id: OperationId) -> bool {
        operation_id.is_live(self.active_id, self.active.is_cancelled())
    }

    pub(crate) fn cancel_current(&self) -> Option<OperationId> {
        let operation_id = self.active_id?;
        self.active.cancel();
        Some(operation_id)
    }

    /// Preserve the legacy untagged-event contract while rejecting old owners.
    pub(crate) fn finish(&mut self, operation_id: OperationId) -> bool {
        if !self.is_current(operation_id) {
            return false;
        }
        self.clear();
        true
    }

    /// Also used when setup fails before a runner can send a terminal event.
    pub(crate) fn clear(&mut self) {
        self.active_id = None;
        self.activity = None;
        self.turn_conversation_cutoff = None;
        self.active_user_request = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};

    fn lifecycle() -> CancelState {
        CancelState {
            active: CancellationToken::new(),
            next_id: 0,
            active_id: None,
            activity: None,
            turn_conversation_cutoff: None,
            stream_retrying: Arc::new(AtomicBool::new(false)),
            response_started: false,
            active_user_request: None,
        }
    }

    #[test]
    fn cancelled_owner_accepts_only_terminal_events_until_finished() {
        let mut state = lifecycle();
        let operation = state.begin(Some(4));
        let child = state.active.child();
        assert!(state.is_live(operation));
        assert_eq!(state.cancel_current(), Some(operation));
        assert!(child.is_cancelled());
        assert!(state.is_current(operation));
        assert!(!state.is_live(operation));
        assert!(!state.finish(OperationId(99)));
        assert_eq!(state.active_id, Some(operation));
        assert!(state.finish(operation));
        assert_eq!(state.turn_conversation_cutoff, None);
        assert!(!state.finish(operation));
        assert_eq!(state.cancel_current(), None);
        let successor = state.begin(Some(8));
        assert_ne!(successor, operation);
        assert!(!state.active.is_cancelled());
        assert!(!state.is_live(operation));
        assert!(state.is_live(successor));
    }

    #[test]
    #[should_panic(expected = "cannot replace an active operation")]
    fn begin_cannot_replace_cancelling_owner() {
        let mut state = lifecycle();
        state.begin(None);
        state.cancel_current();
        state.begin(None);
    }

    #[test]
    fn untagged_events_keep_compatibility_without_reusing_zero() {
        let mut state = lifecycle();
        assert!(state.is_live(OperationId::UNTAGGED));
        assert_ne!(state.begin(None), OperationId::UNTAGGED);
        state.cancel_current();
        assert!(state.is_live(OperationId::UNTAGGED));
    }
}
