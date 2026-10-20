//! The one signal a running step watches, shared between the host that asks for a cancel and the
//! thread that performs it.
//!
//! The host owns every session's handle, and a session's thread spends most of a turn inside a
//! step, not reading its command channel. A token is what lets the host reach it there: the step
//! selects over the workload's own wait and this signal, and the next step reads it to stop
//! dispatching.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Whether one session's work has been asked to stop.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<State>);

#[derive(Debug, Default)]
struct State {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    /// A token no one has cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks everything watching this token to stop. Repeating it changes nothing.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    /// Whether a cancel has been asked for.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Resolves once a cancel has been asked for, which is now when one already has.
    ///
    /// `Notify` stores no permit for `notify_waiters`, so the flag is read after the waiter is
    /// registered: a cancel landing between those two steps is still seen.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::CancelToken;

    const PATIENCE: Duration = Duration::from_secs(1);

    #[tokio::test]
    async fn a_token_no_one_cancelled_does_not_answer() {
        let token = CancelToken::new();

        assert!(!token.is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), token.cancelled())
                .await
                .is_err(),
            "a waiter holds until someone cancels",
        );
    }

    #[tokio::test]
    async fn a_cancelled_token_answers_at_once_and_keeps_answering() {
        let token = CancelToken::new();
        token.cancel();

        assert!(token.is_cancelled());
        for _ in 0..2 {
            assert!(
                tokio::time::timeout(PATIENCE, token.cancelled())
                    .await
                    .is_ok(),
                "a cancelled token answers a waiter that arrives after the cancel",
            );
        }
    }

    #[tokio::test]
    async fn a_waiter_already_holding_wakes_when_the_cancel_lands() {
        let token = CancelToken::new();
        let waiting = token.clone();
        let (woke, mut watched) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            waiting.cancelled().await;
            let _ = woke.send(());
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut watched)
                .await
                .is_err(),
            "nothing wakes a waiter before the cancel",
        );

        token.cancel();

        assert!(
            tokio::time::timeout(PATIENCE, watched).await.is_ok(),
            "the waiter that was already holding wakes on the cancel",
        );
    }
}
