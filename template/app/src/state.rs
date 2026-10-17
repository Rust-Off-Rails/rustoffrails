//! `AppState`: everything a handler or job needs, cheap to clone.

use std::sync::Arc;

use axum::extract::FromRef;
use sea_orm::DatabaseConnection;

use crate::config::Config;
use crate::kernel::assets::{AssetCatalog, Assets};
use crate::kernel::cache::Cache;
use crate::kernel::rate_limit::RateLimiter;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: DatabaseConnection,
    pub cache: Cache,
    pub assets: AssetCatalog,
    pub rate_limit: RateLimiter,
    // samples: state fields
}

impl AppState {
    /// `TestApp` builds state the same way, with `Config::test()` and its own database.
    pub fn new(config: Config, db: DatabaseConnection) -> anyhow::Result<Self> {
        // WHY: uptime counts from boot. `BOOTED_AT` is lazy, so without this it started at the
        // first read (the first /dev/system view), which then showed "0ms".
        std::sync::LazyLock::force(&crate::kernel::ui::BOOTED_AT);
        let assets = AssetCatalog::load("static", config.is_development());
        // samples: state build
        Ok(Self {
            cache: Cache::new(db.clone()),
            rate_limit: RateLimiter::new(config.env),
            assets,
            // samples: state init
            config: Arc::new(config),
            db,
        })
    }
}

// samples: state fns

// The kernel's routers are generic over any state that can hand out these.
impl FromRef<AppState> for DatabaseConnection {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

impl FromRef<AppState> for Assets {
    fn from_ref(state: &AppState) -> Self {
        state.assets.assets()
    }
}
