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

//! A cancellation token shared across a request's lifecycle, with both
//! synchronous polling and awaitable notification.
//!
//! One root token is created per turn and held on the `App`; each phase
//! (stream, classification, tool execution, diagnostics) runs against a child
//! derived from it. Cancelling the parent — what pressing Esc does — propagates
//! to every child, so a single [`CancellationToken::cancel`] stops whatever
//! phase is currently in flight.  Callers that need to *block* until
//! cancellation (e.g. a retry sleep, a long-running classification, a review
//! wait) can `token.wait().await` instead of a bare `sleep`, cutting the wait
//! short as soon as Esc is pressed.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// A cheap, cloneable cancellation flag with optional parent linkage and
/// awaitable notification.
///
/// Clones share the same underlying flag, so cancelling any clone cancels them
/// all. A token created with [`CancellationToken::child`] additionally reports
/// itself cancelled whenever an ancestor is, letting one turn-level token stop
/// every phase derived from it.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
    parent: Option<Arc<CancellationToken>>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// A fresh, un-cancelled root token.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            parent: None,
        }
    }

    /// Request cancellation. Visible to every clone of this token and to any
    /// children derived from it. Wakes every task currently blocked on
    /// [`CancellationToken::wait`].
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    /// Whether cancellation has been requested on this token or any ancestor.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed) || self.parent.as_deref().is_some_and(Self::is_cancelled)
    }

    /// Derive a child token. The child is cancelled when this token is, but can
    /// also be cancelled on its own without affecting the parent.  The child
    /// shares the parent's [`Notify`] so a single parent-cancel wakes every
    /// descendant blocked on `wait()`.
    pub fn child(&self) -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: self.notify.clone(),
            parent: Some(Arc::new(self.clone())),
        }
    }

    /// Resolves once this token (or any ancestor) is cancelled. If the token is
    /// already cancelled the future resolves immediately.
    ///
    /// The shared [`Notify`] means a sibling child's `cancel()` will wake us
    /// spuriously — we loop back and re-check `is_cancelled()` each time so a
    /// sibling cancel never resolves a peer's `wait()`.
    pub async fn wait(&self) {
        // We must hold a clone of our own Arc<Notify> across the await so the
        // notified() future doesn't outlive self.
        let notify = self.notify.clone();
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = notify.notified();
            // Pin the future so it stays put.
            tokio::pin!(notified);
            // Race: check once more after registering the waker but before
            // awaiting, in case cancel() ran between our initial check and the
            // Notified registration.
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// Returns a future that races the given future against cancellation: it
    /// resolves to `Some(T)` if `fut` completes first, or `None` if the token
    /// is cancelled before `fut` finishes. The inner future is dropped on
    /// cancellation, so any resources it holds are released promptly.
    pub async fn wait_or<T>(&self, fut: impl Future<Output = T>) -> Option<T> {
        tokio::select! {
            biased;
            _ = self.wait() => None,
            val = fut => Some(val),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn new_token_is_not_cancelled() {
        assert!(!CancellationToken::new().is_cancelled());
    }

    #[test]
    fn cancel_sets_flag() {
        let t = CancellationToken::new();
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn clone_shares_flag() {
        let t = CancellationToken::new();
        let t2 = t.clone();
        t.cancel();
        assert!(t2.is_cancelled());
    }

    #[test]
    fn child_sees_parent_cancel() {
        let parent = CancellationToken::new();
        let child = parent.child();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_cancel_does_not_affect_parent() {
        let parent = CancellationToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[tokio::test]
    async fn wait_resolves_immediately_when_already_cancelled() {
        let t = CancellationToken::new();
        t.cancel();
        tokio::time::timeout(Duration::from_millis(10), t.wait())
            .await
            .expect("wait should resolve immediately");
    }

    #[tokio::test]
    async fn wait_blocks_until_cancelled() {
        let t = CancellationToken::new();
        let t2 = t.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            t2.cancel();
        });
        tokio::time::timeout(Duration::from_secs(1), t.wait())
            .await
            .expect("wait should be woken by cancel");
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn child_wait_woken_by_parent_cancel() {
        let parent = CancellationToken::new();
        let child = parent.child();
        let p = parent.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            p.cancel();
        });
        tokio::time::timeout(Duration::from_secs(1), child.wait())
            .await
            .expect("child wait should be woken by parent cancel");
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn wait_or_returns_some_when_future_completes_first() {
        let t = CancellationToken::new();
        let result = t.wait_or(async { 42 }).await;
        assert_eq!(result, Some(42));
    }

    #[tokio::test]
    async fn wait_or_returns_none_when_cancelled_first() {
        let t = CancellationToken::new();
        let t2 = t.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            t2.cancel();
        });
        let result = t.wait_or(tokio::time::sleep(Duration::from_secs(5))).await;
        assert!(result.is_none(), "should be cancelled first");
    }

    #[tokio::test]
    async fn sibling_cancel_does_not_wake_peer_wait() {
        // Cancel child A: child B must keep blocking.
        let parent = CancellationToken::new();
        let child_a = parent.child();
        let child_b = parent.child();

        // Cancel child A after a short delay.
        let a = child_a.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            a.cancel();
        });

        // child B's wait() must NOT resolve from child A's cancel.
        let result = tokio::time::timeout(Duration::from_millis(200), child_b.wait()).await;
        assert!(
            result.is_err(),
            "child B should still be waiting — sibling cancel woke it spuriously"
        );

        // Now cancel the parent — child B must resolve.
        parent.cancel();
        tokio::time::timeout(Duration::from_millis(100), child_b.wait())
            .await
            .expect("child B should resolve after parent cancel");
    }

    #[tokio::test]
    async fn sibling_cancel_does_not_affect_peer_wait_or() {
        // Cancel child A: child B's wait_or must keep waiting for its future.
        let parent = CancellationToken::new();
        let child_a = parent.child();
        let child_b = parent.child();

        // Cancel child A after a short delay.
        let a = child_a.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            a.cancel();
        });

        // child B's wait_or should NOT resolve when child A cancels — it
        // should keep waiting for its future (a slow sleep).
        let result = child_b
            .wait_or(tokio::time::sleep(Duration::from_millis(300)))
            .await;
        assert!(
            result.is_some(),
            "child B's wait_or should return Some (future completed), not be \
             cancelled by sibling",
        );

        // Now cancel parent and verify child B's wait_or returns None.
        let child_b2 = parent.child();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            parent.cancel();
        });
        let result = child_b2
            .wait_or(tokio::time::sleep(Duration::from_secs(5)))
            .await;
        assert!(result.is_none(), "child B2 should be cancelled by parent");
    }
}
