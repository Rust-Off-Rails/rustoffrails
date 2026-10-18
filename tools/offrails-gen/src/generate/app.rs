//! What a generator needs to know about the app it writes into: its crate name, its header
//! (mode, tenancy), and which parts are in (accounts, jobs, mailer, realtime, API).

use std::path::{Path, PathBuf};

use crate::util::{self, Fail, Result, rel};

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub dir: PathBuf,
    pub krate: String,
    pub mode: String,
    pub tenancy: String,
    /// auth-core: `controllers::auth::RequireUser`, `models::User`, `factories::user`, `sign_in_as`.
    pub auth: bool,
    pub jobs: bool,
    pub mailer: bool,
    pub dev_mailbox: bool,
    pub realtime: bool,
}

impl AppInfo {
    pub fn load(dir: &Path) -> Result<Self> {
        let krate = util::package_name(dir)?;
        let header = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap_or_default();
        let word = |key: &str| -> String {
            header
                .split_whitespace()
                .skip_while(|w| *w != format!("{key}:"))
                .nth(1)
                .unwrap_or("")
                .to_owned()
        };
        let has = |p: &str, needle: &str| {
            std::fs::read_to_string(rel(dir, p)).is_ok_and(|t| t.contains(needle))
        };
        let mode = match word("FRONTEND_MODE") {
            m if m.is_empty() => "htmx".to_owned(),
            m => m,
        };
        Ok(Self {
            krate,
            tenancy: word("TENANCY"),
            auth: has("src/controllers/auth.rs", "pub struct RequireUser"),
            jobs: has("src/jobs/mod.rs", "pub trait Worker"),
            mailer: has("src/mailers/mod.rs", "pub use crate::kernel::mailer"),
            dev_mailbox: rel(dir, "src/kernel/mailbox_web.rs").is_file(),
            realtime: has("src/channels/mod.rs", "impl Channels for AppState"),
            mode,
            dir: dir.to_path_buf(),
        })
    }

    pub fn path(&self, slash: &str) -> PathBuf {
        rel(&self.dir, slash)
    }

    pub fn exists(&self, slash: &str) -> bool {
        self.path(slash).exists()
    }

    pub fn read(&self, slash: &str) -> Option<String> {
        std::fs::read_to_string(self.path(slash)).ok()
    }

    /// A model's source, wherever it lives: `src/models/<x>/` (mod.rs, params.rs, queries.rs; what
    /// generators write) or `src/models/<x>.rs` (one file: parts, hand-written models, apps
    /// generated before 0.7.3). Returns (the file with the entity, all of its text).
    pub fn model_source(&self, singular: &str) -> Option<(String, String)> {
        let dir = format!("src/models/{singular}");
        if let Some(entity) = self.read(&format!("{dir}/mod.rs")) {
            let mut all = entity;
            for f in ["params.rs", "queries.rs"] {
                if let Some(t) = self.read(&format!("{dir}/{f}")) {
                    all.push('\n');
                    all.push_str(&t);
                }
            }
            return Some((format!("{dir}/mod.rs"), all));
        }
        let file = format!("{dir}.rs");
        self.read(&file).map(|t| (file, t))
    }

    /// htmx pages: the generators that write templates refuse the other modes for now.
    pub fn require_htmx(&self, what: &str) -> Result<()> {
        if self.mode == "htmx" {
            return Ok(());
        }
        Err(Fail::usage(format!(
            "{what} writes htmx pages, and this app is FRONTEND_MODE: {} (AGENTS.md). \
             For {} follow the mode's branch of .offrails/generators/scaffold.md by hand; \
             `generate model`, `migration`, `job`, `mailer` and `api` work in every mode",
            self.mode, self.mode
        )))
    }

    /// Whether the app uses the tenancy suites (`TENANCY` set and not `none`).
    pub fn tenancy_suites(&self) -> bool {
        !self.tenancy.is_empty() && self.tenancy != "none"
    }
}

/// Where an anchor lives, for the message when it's missing (generators/new.md#anchors).
pub fn anchor_hint(anchor: &str) -> &'static str {
    match anchor {
        "lib mods" => "src/lib.rs, after the `pub mod` lines",
        "model mods" => "src/models/mod.rs, after the `pub mod` lines (before `Errors`)",
        "model aliases" => "src/models/mod.rs, after the `pub type X = x::Model;` aliases",
        "controller mods" => "src/controllers/mod.rs, after the `pub mod` lines",
        "view mods" => "src/views/mod.rs, after the `pub mod` lines",
        "routes" => "src/routes.rs, after the last app `.route(…)` statement in `router`",
        "route mods" => {
            "src/routes.rs, after the `use` lines (one `pub mod <feature>;` per src/routes/<feature>.rs)"
        }
        "route tables" => {
            "src/routes.rs, the last line inside `TABLES: &[…]` (after `APP_ROUTES,`)"
        }
        "paths" => "src/routes.rs, the last line inside `pub mod paths { … }`",
        "routes table" => {
            "src/routes.rs, the last line inside `APP_ROUTES: &[…]` (`ROUTES` before 0.7.3)"
        }
        "nav links" => {
            "templates/shared/_nav.html, inside the links <div> ({# samples: nav links #})"
        }
        "sitemap paths" => "src/controllers/meta.rs, the last line of the `public_paths()` vec",
        "migration mods" => "migration/src/lib.rs, after the `mod m…;` lines",
        "migrations" => "migration/src/lib.rs, after the last `Box::new(…)` in `migrations()`",
        "test mods" => "tests/app/main.rs, after the `mod …;` lines",
        "test support" => "tests/app/support.rs, the last line inside `impl TestApp`",
        "factories" => "tests/app/factories.rs, at the end of the file",
        "mailer mods" => "src/mailers/mod.rs, after the `pub mod` lines",
        "previews" => "src/mailers/previews.rs, the last line inside `PREVIEWS: &[…]`",
        "channel mods" => "src/channels/mod.rs, after the `pub mod` lines",
        "task mods" => "src/tasks/mod.rs, after the `pub mod` lines",
        "tasks" => "src/tasks/mod.rs, the last arm before `other =>` in the task `match`",
        "relations" => "the model's `pub enum Relation { … }`, as its last line",
        "related" => "the model file, after its last `impl Related<…>` block",
        "model test mods" => "tests/app/models/mod.rs, after the `mod …;` lines",
        "page mods" | "page view mods" | "page test mods" => {
            "the shared pages module the first `generate page` wrote (src/controllers/pages/mod.rs, src/views/pages/mod.rs, tests/app/pages/mod.rs), at its end"
        }
        "api paths" | "api schemas" => {
            "src/controllers/api.rs (the first `generate api` wrote it): in ApiDoc's `paths(…)` and `schemas(…)`"
        }
        _ => "the spot .offrails/generators/new.md#anchors names",
    }
}
