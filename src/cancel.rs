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

//! A lightweight cancellation token shared across a request's lifecycle.
//!
//! One root token is created per turn and held on the `App`; each phase
//! (stream, classification, tool execution, diagnostics) runs against a child
//! derived from it. Cancelling the parent — what pressing Esc does — propagates
//! to every child, so a single [`CancellationToken::cancel`] stops whatever
//! phase is currently in flight, replacing the previously scattered
//! `Arc<AtomicBool>` flags with independent lifetimes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A cheap, cloneable cancellation flag with optional parent linkage.
///
/// Clones share the same underlying flag, so cancelling any clone cancels them
/// all. A token created with [`CancellationToken::child`] additionally reports
/// itself cancelled whenever an ancestor is, letting one turn-level token stop
/// every phase derived from it.
#[derive(Clone, Default, Debug)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<CancellationToken>>,
}

impl CancellationToken {
    /// A fresh, un-cancelled root token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Visible to every clone of this token and to any
    /// children derived from it.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested on this token or any ancestor.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
            || self.parent.as_deref().is_some_and(Self::is_cancelled)
    }

    /// Derive a child token. The child is cancelled when this token is, but can
    /// also be cancelled on its own without affecting the parent.
    pub fn child(&self) -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            parent: Some(Arc::new(self.clone())),
        }
    }
}
