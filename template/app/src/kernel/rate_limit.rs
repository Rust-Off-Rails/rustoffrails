//! In-process rate limiting (Rails 8 `rate_limit to:, within:`). The limit is per instance,
//! which is acceptable for this app (see docs/decisions.md).
//!
//! Each key carries its own window: a `moka::Expiry` expires an entry `ttl` after creation, so
//! every call site can use a different window. `check` counts a hit; `exhausted` only peeks,
//! which is what failure-only limits need (peek first, `check` after a failure).

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use moka::{Expiry, future::Cache};

use crate::config::AppEnv;
use crate::error::AppError;

/// Development replaces every configured limit with this one, so clicking through a flow while
/// building it never locks the developer out; test and production keep the real limits.
pub const DEV_LIMIT: u32 = 1000;

#[derive(Clone)]
struct Window {
    hits: Arc<AtomicU32>,
    ttl: Duration,
}

struct WindowExpiry;

impl Expiry<String, Window> for WindowExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &Window,
        _created_at: Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    windows: Cache<String, Window>,
    development: bool,
}

impl RateLimiter {
    pub fn new(env: AppEnv) -> Self {
        Self {
            windows: Cache::builder()
                .max_capacity(100_000)
                .expire_after(WindowExpiry)
                .build(),
            development: env == AppEnv::Development,
        }
    }

    fn limit(&self, configured: u32) -> u32 {
        if self.development {
            DEV_LIMIT.max(configured)
        } else {
            configured
        }
    }

    /// Counts one hit for `key`. `Err(TooManyRequests)` once the caller is past `limit` hits
    /// within `window` (the window starts at the first hit).
    pub async fn check(&self, key: &str, limit: u32, window: Duration) -> Result<(), AppError> {
        let w = self
            .windows
            .get_with(key.to_owned(), async move {
                Window {
                    hits: Arc::new(AtomicU32::new(0)),
                    ttl: window,
                }
            })
            .await;
        let hits = w.hits.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        if hits > self.limit(limit) {
            Err(AppError::TooManyRequests)
        } else {
            Ok(())
        }
    }

    /// `true` once `key` has reached `limit` hits in its current window. Doesn't count.
    pub async fn exhausted(&self, key: &str, limit: u32) -> bool {
        self.windows
            .get(key)
            .await
            .is_some_and(|w| w.hits.load(Ordering::Relaxed) >= self.limit(limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocks_after_the_limit_per_key() {
        let rl = RateLimiter::new(AppEnv::Test);
        let window = Duration::from_secs(3600);
        for _ in 0..3 {
            assert!(rl.check("login:1.2.3.4", 3, window).await.is_ok());
        }
        assert!(matches!(
            rl.check("login:1.2.3.4", 3, window).await,
            Err(AppError::TooManyRequests)
        ));
        assert!(rl.check("login:5.6.7.8", 3, window).await.is_ok());
    }

    #[tokio::test]
    async fn development_raises_the_limit_and_production_keeps_it() {
        let window = Duration::from_secs(60);
        let dev = RateLimiter::new(AppEnv::Development);
        for _ in 0..50 {
            dev.check("k", 10, window).await.unwrap();
        }
        let prod = RateLimiter::new(AppEnv::Production);
        for _ in 0..10 {
            prod.check("k", 10, window).await.unwrap();
        }
        assert!(prod.check("k", 10, window).await.is_err());
    }

    #[tokio::test]
    async fn exhausted_peeks_without_counting() {
        let rl = RateLimiter::new(AppEnv::Test);
        let window = Duration::from_secs(3600);
        assert!(!rl.exhausted("k", 2).await);
        rl.check("k", 2, window).await.unwrap();
        assert!(!rl.exhausted("k", 2).await);
        rl.check("k", 2, window).await.unwrap();
        assert!(rl.exhausted("k", 2).await);
        // Peeking never advanced the counter: the next hit is the third, not the fifth.
        assert!(rl.check("k", 2, window).await.is_err());
    }

    #[tokio::test]
    async fn each_key_gets_its_own_window() {
        let rl = RateLimiter::new(AppEnv::Test);
        rl.check("short", 1, Duration::from_millis(60))
            .await
            .unwrap();
        assert!(
            rl.check("short", 1, Duration::from_millis(60))
                .await
                .is_err()
        );
        rl.check("long", 1, Duration::from_secs(3600))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        // `short` expired on its own ttl; `long` is unaffected.
        assert!(
            rl.check("short", 1, Duration::from_millis(60))
                .await
                .is_ok()
        );
        assert!(
            rl.check("long", 1, Duration::from_secs(3600))
                .await
                .is_err()
        );
    }
}
