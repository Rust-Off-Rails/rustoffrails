//! Digested asset URLs (Propshaft's digests): every file under `static/` is hashed once at boot
//! (development rescans per call, so `--watch` output gets a new `?v=`). Spec:
//! .offrails/guides/frontend-htmx.md#assets--cache-busting-kernelassetsrs

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};

/// Digested URLs the layouts need. Built from an `AssetCatalog` (in `AppState`).
#[derive(Debug, Clone, Default)]
pub struct Assets {
    pub css: String,
    pub htmx: String,
    pub htmx_sse: String,
    pub app_js: String,
    pub admin_css: String,
    pub admin_js: String,
    pub dev_reload_js: String,
    pub favicon_svg: String,
    pub apple_touch_icon: String,
}

/// One key per `Assets` field, in field order. A key ending in `-` or `.` is a prefix for a file
/// whose name carries its version (`vendor/htmx-2.0.11.min.js`); every other key is an exact path.
/// `every_asset_key_matches_exactly_one_static_file` proves each matches one file.
pub const ASSET_KEYS: &[&str] = &[
    "app.css",
    "vendor/htmx-2.",
    "vendor/htmx-ext-sse-",
    "app.js",
    "kernel/admin.css",
    "kernel/admin.js",
    "dev-reload.js",
    "favicon.svg",
    "apple-touch-icon.png",
];

struct Inner {
    root: String,
    recompute: bool,
    digests: RwLock<Arc<BTreeMap<String, String>>>,
}

#[derive(Clone)]
pub struct AssetCatalog(Arc<Inner>);

impl AssetCatalog {
    /// `AppState::new`: `AssetCatalog::load("static", config.is_development())`.
    pub fn load(root: &str, recompute: bool) -> Self {
        Self(Arc::new(Inner {
            root: root.to_string(),
            recompute,
            digests: RwLock::new(Arc::new(scan(Path::new(root)))),
        }))
    }

    fn digests(&self) -> Arc<BTreeMap<String, String>> {
        if self.0.recompute {
            let fresh = Arc::new(scan(Path::new(&self.0.root)));
            if let Ok(mut slot) = self.0.digests.write() {
                *slot = fresh.clone();
            }
            fresh
        } else {
            self.0.digests.read().map(|d| d.clone()).unwrap_or_default()
        }
    }

    pub fn assets(&self) -> Assets {
        let d = self.digests();
        let p = |i: usize| path_for(&d, ASSET_KEYS[i]);
        Assets {
            css: p(0),
            htmx: p(1),
            htmx_sse: p(2),
            app_js: p(3),
            admin_css: p(4),
            admin_js: p(5),
            dev_reload_js: p(6),
            favicon_svg: p(7),
            apple_touch_icon: p(8),
        }
    }

    /// `/static/<file>?v=<hash>` for any other file a page needs (a kernel page's own stylesheet:
    /// `state.assets.path("kernel/mailbox.css")`). Same key rules as `ASSET_KEYS`.
    pub fn path(&self, key: &str) -> String {
        path_for(&self.digests(), key)
    }
}

fn is_prefix_key(key: &str) -> bool {
    key.ends_with('-') || key.ends_with('.')
}

/// Exact keys match one path; prefix keys match the first file in sorted order (never `HashMap`
/// order), and the uniqueness test keeps that to one.
fn path_for(digests: &BTreeMap<String, String>, key: &str) -> String {
    let hit = if is_prefix_key(key) {
        digests.iter().find(|(file, _)| file.starts_with(key))
    } else {
        digests.get_key_value(key)
    };
    match hit {
        Some((file, hash)) => format!("/static/{file}?v={hash}"),
        None => format!("/static/{key}"),
    }
}

fn scan(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    scan_dir(root, root, &mut out);
    out
}

fn scan_dir(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(root, &path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let digest = Sha256::digest(&bytes);
            let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
            out.insert(rel, hex);
        }
    }
}

/// `/static` is a nested router; this middleware wraps only static files.
pub async fn cache_headers(request: Request, next: Next) -> Response {
    let versioned = request.uri().query().is_some_and(|q| q.contains("v="));
    let mut response = next.run(request).await;
    // Only a file that exists, asked for by digest, is immutable; a 404 must never be cached.
    let value = if versioned && response.status().is_success() {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    response
}

/// Why `static/app.css` can't be served, if it can't: missing or empty. Never an mtime check:
/// Tailwind 4 doesn't rewrite unchanged output.
pub fn css_problem(path: &Path) -> Option<String> {
    let fix = "Run bin/dev, or bin/tailwindcss -i assets/app.css -o static/app.css";
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() == 0 => {
            Some(format!("CSS not built: {} is empty. {fix}", path.display()))
        }
        Ok(_) => None,
        Err(_) => Some(format!(
            "CSS not built: {} is missing. {fix}",
            path.display()
        )),
    }
}

pub fn app_css_problem() -> Option<String> {
    css_problem(Path::new("static/app.css"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_asset_key_matches_exactly_one_static_file() {
        let files = scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("static"));
        for key in ASSET_KEYS {
            let hits: Vec<_> = if is_prefix_key(key) {
                files.keys().filter(|f| f.starts_with(key)).collect()
            } else {
                files.keys().filter(|f| f == key).collect()
            };
            assert!(
                hits.len() <= 1,
                "asset key {key:?} matches {hits:?}: make it exact"
            );
            // app.css is built output (git-ignored), so it may be absent.
            assert!(
                *key == "app.css" || hits.len() == 1,
                "asset key {key:?} matches no file"
            );
        }
    }

    #[test]
    fn the_htmx_key_never_picks_the_sse_extension() {
        let mut d = BTreeMap::new();
        d.insert("vendor/htmx-ext-sse-2.2.4.min.js".to_string(), "a".into());
        d.insert("vendor/htmx-2.0.11.min.js".to_string(), "b".into());
        assert_eq!(
            path_for(&d, "vendor/htmx-2."),
            "/static/vendor/htmx-2.0.11.min.js?v=b"
        );
    }

    #[test]
    fn exact_keys_do_not_match_by_prefix() {
        let mut d = BTreeMap::new();
        d.insert("kernel/admin.css.map".to_string(), "a".into());
        assert_eq!(path_for(&d, "kernel/admin.css"), "/static/kernel/admin.css");
    }

    #[test]
    fn path_appends_digest_query() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.css"), b"body{}").unwrap();
        let catalog = AssetCatalog::load(dir.path().to_str().unwrap(), false);
        assert!(catalog.path("app.css").starts_with("/static/app.css?v="));
    }

    #[test]
    fn css_problem_reports_missing_empty_and_built() {
        let dir = tempfile::tempdir().unwrap();
        let css = dir.path().join("app.css");
        assert!(css_problem(&css).unwrap().contains("missing"));
        std::fs::write(&css, b"").unwrap();
        assert!(css_problem(&css).unwrap().contains("empty"));
        std::fs::write(&css, b"body{color:red}").unwrap();
        assert!(css_problem(&css).is_none());
    }
}
