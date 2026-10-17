//! Ask the running app, don't guess (.offrails/guides/dev-tools.md#introspection): the snapshot behind
//! `GET /dev/introspect.json` and `app introspect`, and the version `GET /dev/reload` hands to
//! `static/dev-reload.js`. Development only: the routes live in `controllers::dev::router()`, which
//! production and test never mount, and `app introspect` refuses outside development.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;
use std::time::SystemTime;

use migration::{MigrationStatus, Migrator, MigratorTrait};
use serde_json::{Value, json};

use crate::kernel::css;
use crate::kernel::ui::BOOTED_AT;
use crate::routes::ROUTES;
use crate::state::AppState;

/// One id per process: a restart changes it, so open tabs reload.
pub static BOOT_ID: LazyLock<String> =
    LazyLock::new(|| format!("{}-{}", std::process::id(), BOOTED_AT.timestamp_micros()));

/// The most missing classes listed (the count is always exact).
const MAX_MISSING: usize = 50;

/// `BOOT_ID:<newest mtime under static/ in ms>:<file count>`. Mtimes, not digests: every open tab
/// asks once a second.
pub fn reload_version() -> String {
    let (mut newest, mut count) = (0u128, 0usize);
    walk(Path::new("static"), &mut newest, &mut count);
    format!("{}:{newest}:{count}", *BOOT_ID)
}

fn walk(dir: &Path, newest: &mut u128, count: &mut usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, newest, count);
        } else if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            *count += 1;
            let ms = modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            *newest = (*newest).max(ms);
        }
    }
}

/// The app's state as JSON. `live` (the HTTP handler) adds what only a serving process knows.
pub async fn snapshot(state: &AppState, frontend_mode: &str, live: bool) -> anyhow::Result<Value> {
    let c = &state.config;
    let tls = c.tls.is_some();
    let migrations = Migrator::get_migration_with_status(&state.db).await?;
    let applied = migrations
        .iter()
        .filter(|m| matches!(m.status(), MigrationStatus::Applied))
        .count();
    let pending: Vec<String> = migrations
        .iter()
        .filter(|m| matches!(m.status(), MigrationStatus::Pending))
        .map(|m| m.name().to_owned())
        .collect();
    let css = match css::missing_classes() {
        None => {
            json!({ "built": false, "stale": false, "missing_count": 0, "missing_classes": [] })
        }
        Some(missing) => json!({
            "built": true,
            "stale": !missing.is_empty(),
            "missing_count": missing.len(),
            "missing_classes": missing.iter().take(MAX_MISSING).collect::<Vec<_>>(),
        }),
    };
    let assets = state.assets.assets();
    #[allow(unused_mut)] // samples add their sections below
    let mut out = json!({
        "app": {
            "name": crate::config::APP_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "framework": fs::read_to_string(".offrails/VERSION").ok().and_then(|v| v.lines().next().map(str::to_owned)),
            "frontend_mode": frontend_mode,
            "env": format!("{:?}", c.env).to_lowercase(),
            "boot_id": *BOOT_ID,
            "booted_at": BOOTED_AT.to_rfc3339(),
        },
        "url": {
            "app_url": c.app_url,
            "tls": tls,
            "dev_http_port": c.dev_http_port,
            "protocol": if tls { "h2" } else { "http/1.1" },
            "note": if tls {
                "HTTPS with ALPN h2 (a locally trusted certificate)"
            } else {
                "plain HTTP/1.1: browsers hold ~6 connections per origin, so live pages stall past ~6 tabs"
            },
        },
        "config": format!("{c:?}"), // Config's hand-written Debug redacts credentials (config.md)
        "routes": ROUTES.iter().map(|(method, path, handler, access)| json!({
            "method": method, "path": path, "handler": handler, "access": format!("{access:?}"),
        })).collect::<Vec<_>>(),
        "migrations": { "applied": applied, "pending": pending, "current": pending.is_empty() },
        "realtime": Value::Null,
        "live": live,
        "assets": { "css": assets.css, "app_js": assets.app_js, "htmx": assets.htmx, "htmx_sse": assets.htmx_sse },
        "css": css,
    });
    // samples: introspect sections
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_version_starts_with_the_boot_id() {
        let v = reload_version();
        assert!(v.starts_with(&format!("{}:", *BOOT_ID)), "{v}");
        assert_eq!(v.split(':').count(), 3, "{v}");
    }
}
