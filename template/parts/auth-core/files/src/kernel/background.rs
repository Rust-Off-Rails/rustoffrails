//! Fire-and-forget work for auth emails: sign-in links, resets, verification and security
//! notices. Spawned after the response is decided, so a request never waits on SMTP.
//!
//! GUIDE: .offrails/guides/mailers.md (tokens are minted inside the job, never in its args)
//!
//! WHY: the samples need no job queue. Each task mints its own token, so no secret ever sits in
//! a queue row; with the `jobs` sample installed, an app can move these into jobs that carry
//! only ids (README, "Adapting it").
//! WHY: spawning (not awaiting) keeps known and unknown emails equally fast: the response time
//! can't reveal whether an address has an account.

use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::task::JoinSet;

#[derive(Clone, Default)]
pub struct Background {
    tasks: Arc<Mutex<JoinSet<()>>>,
}

impl Background {
    /// Runs `work` in the background. A failure is logged with `what` (never with the token or
    /// the email body).
    // SECURITY: `what` is a fixed label such as "login link email"; never put a token in it
    // (test: session_tokens_never_reach_the_logs)
    pub fn spawn<F>(&self, what: &'static str, work: F)
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Ok(mut tasks) = self.tasks.lock() else {
            tracing::error!(what, "background task set poisoned; work dropped");
            return;
        };
        // Reap finished tasks so the set doesn't grow without bound.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            if let Err(e) = work.await {
                tracing::error!(what, error = %e, "background auth work failed");
            }
        });
    }

    /// Waits until every task spawned so far has finished. Tests call it (`app.settle()`) before
    /// reading the mailbox; shutdown can call it to let pending mail go out.
    pub async fn idle(&self) {
        loop {
            let mut batch = match self.tasks.lock() {
                Ok(mut tasks) => std::mem::take(&mut *tasks),
                Err(_) => return,
            };
            if batch.is_empty() {
                return;
            }
            while batch.join_next().await.is_some() {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn idle_waits_for_spawned_work() {
        let bg = Background::default();
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..5 {
            let done = done.clone();
            bg.spawn("test", async move {
                tokio::task::yield_now().await;
                done.fetch_add(1, Ordering::Relaxed);
                Ok(())
            });
        }
        bg.idle().await;
        assert_eq!(done.load(Ordering::Relaxed), 5);
    }
}
