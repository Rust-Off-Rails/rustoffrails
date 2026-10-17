//! Optional overview panels from other parts of the app (the job queue, realtime), through one
//! small trait. A panel that returns `None` is hidden, so the dashboard never depends on a sample
//! being installed.
//!
//! APP: register yours in `providers()`. A realtime panel, with the realtime sample, looks like:
//!
//! ```ignore
//! struct Realtime;
//! impl PanelProvider for Realtime {
//!     fn panel<'a>(&'a self, _state: &'a AppState) -> PanelFuture<'a> {
//!         Box::pin(async move {
//!             let stats = crate::kernel::channels::stats(); // whatever your hub exposes
//!             Some(Panel::new("Realtime", Some("/dev/sockets"))
//!                 .figure("Sockets", stats.sockets.to_string(), "info")
//!                 .figure("Topics", stats.topics.to_string(), "muted"))
//!         })
//!     }
//! }
//! ```

use std::future::Future;
use std::pin::Pin;

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::state::AppState;

pub struct Figure {
    pub label: String,
    pub value: String,
    /// `ok`, `warn`, `err`, `info` or `muted`.
    pub tone: &'static str,
}

pub struct Panel {
    pub title: String,
    pub href: Option<String>,
    pub figures: Vec<Figure>,
}

impl Panel {
    pub fn new(title: &str, href: Option<&str>) -> Self {
        Self {
            title: title.to_owned(),
            href: href.map(str::to_owned),
            figures: Vec::new(),
        }
    }

    pub fn figure(mut self, label: &str, value: String, tone: &'static str) -> Self {
        self.figures.push(Figure {
            label: label.to_owned(),
            value,
            tone,
        });
        self
    }
}

pub type PanelFuture<'a> = Pin<Box<dyn Future<Output = Option<Panel>> + Send + 'a>>;

pub trait PanelProvider: Send + Sync {
    /// The panel now, or `None` to hide it.
    fn panel<'a>(&'a self, state: &'a AppState) -> PanelFuture<'a>;
}

/// APP: the registration point. Order here is order on the overview.
pub fn providers() -> Vec<Box<dyn PanelProvider>> {
    vec![Box::new(JobsTable)]
}

pub async fn panels(state: &AppState) -> Vec<Panel> {
    let mut out = Vec::new();
    for p in providers() {
        if let Some(panel) = p.panel(state).await {
            out.push(panel);
        }
    }
    out
}

/// Whether a table exists (a sample's migration ran).
pub async fn table_exists(db: &impl ConnectionTrait, table: &str) -> bool {
    let backend = db.get_database_backend();
    let stmt = match backend {
        DbBackend::Postgres => Statement::from_sql_and_values(
            backend,
            "SELECT 1 AS one FROM information_schema.tables WHERE table_name = $1",
            [table.into()],
        ),
        _ => Statement::from_sql_and_values(
            backend,
            "SELECT 1 AS one FROM sqlite_master WHERE type = 'table' AND name = ?",
            [table.into()],
        ),
    };
    matches!(db.query_one_raw(stmt).await, Ok(Some(_)))
}

/// The job queue (the jobs sample), by its table: counts per state, without depending on its code.
pub struct JobsTable;

impl PanelProvider for JobsTable {
    fn panel<'a>(&'a self, state: &'a AppState) -> PanelFuture<'a> {
        Box::pin(async move {
            let db = &state.db;
            if !table_exists(db, "jobs").await {
                return None;
            }
            let backend = db.get_database_backend();
            let rows = db
                .query_all_raw(Statement::from_string(
                    backend,
                    "SELECT state, COUNT(*) AS n FROM jobs GROUP BY state ORDER BY state",
                ))
                .await
                .ok()?;
            let count = |name: &str| {
                rows.iter()
                    .find(|r| r.try_get::<String>("", "state").ok().as_deref() == Some(name))
                    .and_then(|r| r.try_get::<i64>("", "n").ok())
                    .unwrap_or(0)
            };
            let tone = |n: i64, bad: &'static str| if n > 0 { bad } else { "muted" };
            let (executing, available, retryable, discarded) = (
                count("executing"),
                count("available"),
                count("retryable"),
                count("discarded"),
            );
            Some(
                Panel::new("Jobs", Some("/admin/jobs"))
                    .figure("Executing", executing.to_string(), tone(executing, "info"))
                    .figure("Available", available.to_string(), tone(available, "info"))
                    .figure("Retryable", retryable.to_string(), tone(retryable, "warn"))
                    .figure("Discarded", discarded.to_string(), tone(discarded, "err")),
            )
        })
    }
}
