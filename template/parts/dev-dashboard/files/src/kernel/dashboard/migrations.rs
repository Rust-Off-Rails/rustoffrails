//! Migrations, as a timeline: every migration the app's `Migrator` knows, applied or pending,
//! when it ran, which area it belongs to, and what it did, plus the drift between what the
//! migrations imply and what the database holds.
//!
//! What is read and what is derived (the page says the same):
//!
//! | Fact | Source |
//! |------|--------|
//! | the list and its order | `Migrator::migrations()` (the code) |
//! | applied, and when | `seaql_migrations` (`version`, `applied_at`), read-only |
//! | what it did | **derived** from `migration/src/<name>.rs`: `Table::create/alter/drop`, `Index::create/drop` with a literal `.name("…")`, and `CREATE`/`ALTER`/`DROP` in raw SQL. Best effort: an index named from a variable, or a table built in a loop, is not seen |
//! | edited after it was applied | SeaORM records no checksum. The dashboard keeps its own: the SHA-256 of each applied migration's file the first time it sees it applied (`storage/dashboard/migration-checksums.json`), and flags a later difference |
//! | drift | the derived effects compared with the live catalog (`schema::tables`) |
//!
//! GUIDE: .offrails/guides/migrations.md
//! GUIDE: .offrails/guides/dev-tools.md#pending-migrations

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};

use super::erd;
use super::insights::{Finding, Severity};
use super::schema::Table;

/// Where migration sources live, relative to the app root.
pub const SOURCE_DIR: &str = "migration/src";
/// The dashboard's own checksum ledger, relative to the app root.
pub const LEDGER: &str = "storage/dashboard/migration-checksums.json";
/// Tables that belong to the database or the migrator, never to a migration.
const SYSTEM_TABLES: &[&str] = &["seaql_migrations", "sqlite_sequence"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    CreateTable {
        table: String,
        columns: Vec<String>,
    },
    AlterTable {
        table: String,
        added: Vec<String>,
        dropped: Vec<String>,
        renamed: Vec<(String, String)>,
    },
    DropTable {
        table: String,
    },
    CreateIndex {
        name: String,
        unique: bool,
    },
    DropIndex {
        name: String,
    },
    /// Raw SQL the parser recognised (`CREATE TABLE x`, `CREATE INDEX y`, …): the statement's head.
    Sql {
        head: String,
    },
}

impl Effect {
    pub fn tone(&self) -> &'static str {
        match self {
            Effect::CreateTable { .. } | Effect::CreateIndex { .. } => "ok",
            Effect::AlterTable { .. } | Effect::Sql { .. } => "info",
            Effect::DropTable { .. } | Effect::DropIndex { .. } => "err",
        }
    }

    /// "creates table lab_orders (6 columns)".
    pub fn text(&self) -> String {
        match self {
            Effect::CreateTable { table, columns } => format!(
                "creates table {table} ({} column{}: {})",
                columns.len(),
                if columns.len() == 1 { "" } else { "s" },
                columns.join(", ")
            ),
            Effect::AlterTable {
                table,
                added,
                dropped,
                renamed,
            } => {
                let mut parts = Vec::new();
                if !added.is_empty() {
                    parts.push(format!("adds {}", added.join(", ")));
                }
                if !dropped.is_empty() {
                    parts.push(format!("drops {}", dropped.join(", ")));
                }
                for (a, b) in renamed {
                    parts.push(format!("renames {a} to {b}"));
                }
                if parts.is_empty() {
                    parts.push("changes columns".into());
                }
                format!("alters table {table}: {}", parts.join("; "))
            }
            Effect::DropTable { table } => format!("drops table {table}"),
            Effect::CreateIndex { name, unique } => {
                format!(
                    "creates {}index {name}",
                    if *unique { "unique " } else { "" }
                )
            }
            Effect::DropIndex { name } => format!("drops index {name}"),
            Effect::Sql { head } => format!("runs SQL: {head}"),
        }
    }

    fn table(&self) -> Option<&str> {
        match self {
            Effect::CreateTable { table, .. }
            | Effect::AlterTable { table, .. }
            | Effect::DropTable { table } => Some(table),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MigrationInfo {
    pub name: String,
    pub applied_at: Option<DateTime<Utc>>,
    /// The area of the first table it touches (`erd::AREAS`), or a guess from its name.
    pub area: &'static str,
    /// App-relative path of its source, when the file is there.
    pub file: Option<String>,
    pub effects: Vec<Effect>,
    /// The file changed since the dashboard first saw this migration applied.
    pub edited: bool,
}

impl MigrationInfo {
    pub fn applied(&self) -> bool {
        self.applied_at.is_some()
    }
}

/// One way the database and the migrations disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    pub key: String,
    pub title: String,
    pub detail: String,
    pub migration: Option<String>,
    pub table: Option<String>,
}

pub struct Report {
    pub migrations: Vec<MigrationInfo>,
    /// In `seaql_migrations` but unknown to this code (another branch, a deleted file).
    pub unknown_applied: Vec<(String, Option<DateTime<Utc>>)>,
    pub drift: Vec<Drift>,
}

impl Report {
    pub fn pending(&self) -> Vec<&MigrationInfo> {
        self.migrations.iter().filter(|m| !m.applied()).collect()
    }
}

/// `seaql_migrations`, read-only: (version, applied at). An absent table is "nothing applied".
pub async fn applied(db: &DatabaseConnection) -> Vec<(String, Option<DateTime<Utc>>)> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT version, applied_at FROM seaql_migrations ORDER BY version",
        ))
        .await
        .unwrap_or_default();
    rows.iter()
        .map(|r| {
            let at = r
                .try_get::<i64>("", "applied_at")
                .ok()
                .and_then(|s| DateTime::from_timestamp(s, 0));
            (r.try_get::<String>("", "version").unwrap_or_default(), at)
        })
        .collect()
}

/// Everything about every migration. `root` is the app root (sources and the ledger).
pub async fn report(db: &DatabaseConnection, tables: &[Table], root: &Path) -> Report {
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let applied_rows = applied(db).await;
    let applied_at: HashMap<&str, Option<DateTime<Utc>>> = applied_rows
        .iter()
        .map(|(n, at)| (n.as_str(), *at))
        .collect();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut migrations: Vec<MigrationInfo> = names
        .iter()
        .map(|name| {
            let rel = format!("{SOURCE_DIR}/{name}.rs");
            let source = std::fs::read_to_string(root.join(&rel)).ok();
            let effects = source.as_deref().map(parse).unwrap_or_default();
            if let Some(s) = source {
                sources.insert(name.clone(), s);
            }
            let area = effects
                .iter()
                .find_map(Effect::table)
                .map(erd::area)
                .unwrap_or_else(|| area_from_name(name));
            MigrationInfo {
                file: sources.contains_key(name).then_some(rel),
                // Applied with an unreadable time still counts as applied (shown as 1970).
                applied_at: applied_at
                    .get(name.as_str())
                    .map(|at| at.unwrap_or(DateTime::UNIX_EPOCH)),
                area,
                effects,
                edited: false,
                name: name.clone(),
            }
        })
        .collect();
    let known: HashSet<&str> = names.iter().map(String::as_str).collect();
    let unknown_applied = applied_rows
        .iter()
        .filter(|(n, _)| !known.contains(n.as_str()))
        .cloned()
        .collect();
    let applied_sources: BTreeMap<String, String> = sources
        .iter()
        .filter(|(n, _)| applied_at.contains_key(n.as_str()))
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();
    let edited = check_ledger(&root.join(LEDGER), &applied_sources);
    for m in &mut migrations {
        m.edited = edited.contains(&m.name);
    }
    let drift = drift(&migrations, tables);
    Report {
        migrations,
        unknown_applied,
        drift,
    }
}

/// The area a migration's name suggests (`m…_create_lab_orders` → `dev-dashboard`).
fn area_from_name(name: &str) -> &'static str {
    let words: Vec<&str> = name.split('_').skip(3).collect();
    for start in 0..words.len() {
        let candidate = words[start..].join("_");
        let a = erd::area(&candidate);
        if a != "app" {
            return a;
        }
    }
    "app"
}

fn sha256(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Records the checksum of each applied migration seen for the first time; returns the names
/// whose file no longer matches. A missing or unreadable ledger starts a new one.
pub fn check_ledger(path: &Path, applied_sources: &BTreeMap<String, String>) -> HashSet<String> {
    let mut ledger: BTreeMap<String, String> = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut edited = HashSet::new();
    let mut changed = false;
    for (name, source) in applied_sources {
        let sum = sha256(source);
        match ledger.get(name) {
            Some(seen) if *seen != sum => {
                edited.insert(name.clone());
            }
            Some(_) => {}
            None => {
                ledger.insert(name.clone(), sum);
                changed = true;
            }
        }
    }
    if changed {
        write_atomically(path, &ledger);
    }
    edited
}

fn write_atomically(path: &Path, ledger: &BTreeMap<String, String>) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string_pretty(ledger) else {
        return;
    };
    // WHY: a temp file per process, then a rename: two processes (bin/dev and a test run) never
    // leave a half-written ledger.
    let tmp: PathBuf = dir.join(format!(".checksums-{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// What the migrations imply versus what the database holds.
pub fn drift(migrations: &[MigrationInfo], tables: &[Table]) -> Vec<Drift> {
    let live: HashMap<&str, &Table> = tables.iter().map(|t| (t.name.as_str(), t)).collect();
    // Expected tables and columns, replaying the applied migrations' effects in order.
    let mut expected: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut touched: HashSet<String> = HashSet::new();
    for m in migrations.iter().filter(|m| m.applied()) {
        for e in &m.effects {
            match e {
                Effect::CreateTable { table, columns } => {
                    expected.insert(table.clone(), (m.name.clone(), columns.clone()));
                    touched.insert(table.clone());
                }
                Effect::AlterTable {
                    table,
                    added,
                    dropped,
                    renamed,
                } => {
                    touched.insert(table.clone());
                    if let Some((_, cols)) = expected.get_mut(table) {
                        cols.extend(added.iter().cloned());
                        cols.retain(|c| !dropped.contains(c));
                        for (a, b) in renamed {
                            for c in cols.iter_mut().filter(|c| *c == a) {
                                *c = b.clone();
                            }
                        }
                    }
                }
                Effect::DropTable { table } => {
                    expected.remove(table);
                    touched.insert(table.clone());
                }
                Effect::Sql { head } => {
                    if let Some(t) = sql_table(head) {
                        touched.insert(t);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (table, (migration, cols)) in &expected {
        match live.get(table.as_str()) {
            None => out.push(Drift {
                key: format!("drift-missing-table-{table}"),
                title: format!("Table {table} is missing"),
                detail: format!(
                    "{migration} is applied and creates it, but the database has no such table (dropped by hand, or a different database?)."
                ),
                migration: Some(migration.clone()),
                table: Some(table.clone()),
            }),
            Some(t) => {
                let have: HashSet<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
                let missing: Vec<&str> = cols
                    .iter()
                    .map(String::as_str)
                    .filter(|c| !have.contains(c))
                    .collect();
                if !missing.is_empty() {
                    out.push(Drift {
                        key: format!("drift-missing-columns-{table}"),
                        title: format!("{table} lacks column{} the migrations add", if missing.len() == 1 { "" } else { "s" }),
                        detail: format!("Expected {}: not in the database.", missing.join(", ")),
                        migration: Some(migration.clone()),
                        table: Some(table.clone()),
                    });
                }
                let want: HashSet<&str> = cols.iter().map(String::as_str).collect();
                let extra: Vec<&str> = t
                    .columns
                    .iter()
                    .map(|c| c.name.as_str())
                    .filter(|c| !want.contains(c))
                    .collect();
                if !extra.is_empty() && !cols.is_empty() {
                    out.push(Drift {
                        key: format!("drift-extra-columns-{table}"),
                        title: format!("{table} has column{} no migration adds", if extra.len() == 1 { "" } else { "s" }),
                        detail: format!(
                            "{} in the database; none of the migration files we could read adds {}.",
                            extra.join(", "),
                            if extra.len() == 1 { "it" } else { "them" }
                        ),
                        migration: Some(migration.clone()),
                        table: Some(table.clone()),
                    });
                }
            }
        }
    }
    for t in tables {
        if SYSTEM_TABLES.contains(&t.name.as_str()) || touched.contains(&t.name) {
            continue;
        }
        out.push(Drift {
            key: format!("drift-unmigrated-table-{}", t.name),
            title: format!("Table {} isn't created by any migration", t.name),
            detail: "No applied migration file we could read creates it: made by hand, by another branch's migration, or by code we can't parse.".into(),
            migration: None,
            table: Some(t.name.clone()),
        });
    }
    for m in migrations.iter().filter(|m| m.edited) {
        out.push(Drift {
            key: format!("drift-edited-{}", m.name),
            title: format!("{} was edited after it was applied", m.name),
            detail: "Its file no longer matches the checksum the dashboard recorded when it first saw it applied. The database won't re-run it: write a new migration for the change, and restore this one.".into(),
            migration: Some(m.name.clone()),
            table: None,
        });
    }
    out
}

fn sql_table(head: &str) -> Option<String> {
    let words: Vec<String> = head
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c| c == '"' || c == '`' || c == '(')
                .to_lowercase()
        })
        .collect();
    let at = words.iter().position(|w| w == "table")?;
    words
        .iter()
        .skip(at + 1)
        .find(|w| !matches!(w.as_str(), "if" | "not" | "exists"))
        .cloned()
}

/// Drift and edits as Insights findings.
pub fn findings(report: &Report) -> Vec<Finding> {
    let mut out: Vec<Finding> = report
        .drift
        .iter()
        .map(|d| Finding {
            key: d.key.clone(),
            kind: "migrations",
            severity: if d.key.starts_with("drift-edited") || d.key.starts_with("drift-missing") {
                Severity::High
            } else {
                Severity::Medium
            },
            score: 1,
            title: d.title.clone(),
            evidence: d.detail.clone(),
            fix: if d.key.starts_with("drift-edited") {
                "Restore the applied migration's file and put the change in a new migration (`app generate migration …`).".into()
            } else {
                "Make the schema come from migrations only: a new migration for the change, or recreate the development database (`app db reset`).".into()
            },
            guide: ".offrails/guides/migrations.md#rules",
            guide_url: "https://rustoffrails.org/docs/guides/migrations#rules",
            href: "/dev/dashboard/schema/migrations".into(),
        })
        .collect();
    if !report.unknown_applied.is_empty() {
        out.push(Finding {
            key: "migrations-unknown-applied".into(),
            kind: "migrations",
            severity: Severity::Medium,
            score: report.unknown_applied.len() as u64,
            title: format!(
                "{} applied migration{} this code doesn't have",
                report.unknown_applied.len(),
                if report.unknown_applied.len() == 1 { "" } else { "s" }
            ),
            evidence: report
                .unknown_applied
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            fix: "Another branch's migration ran against this database. Switch back and roll it back, or recreate the development database.".into(),
            guide: ".offrails/guides/migrations.md#rules",
            guide_url: "https://rustoffrails.org/docs/guides/migrations#rules",
            href: "/dev/dashboard/schema/migrations".into(),
        });
    }
    out
}

// ---- the source parser -----------------------------------------------------------------------

/// What a migration's `up` does, from its source. Best effort, never panics.
pub fn parse(source: &str) -> Vec<Effect> {
    let idents = iden_overrides(source);
    let up = up_body(source);
    let markers = [
        "Table::create()",
        "Table::alter()",
        "Table::drop()",
        "Index::create()",
        "Index::drop()",
        "execute_unprepared(",
    ];
    let mut starts: Vec<(usize, &str)> = markers
        .iter()
        .flat_map(|m| {
            up.match_indices(m)
                .map(|(i, _)| (i, *m))
                .collect::<Vec<_>>()
        })
        .collect();
    starts.sort();
    let mut out = Vec::new();
    for (k, (at, marker)) in starts.iter().enumerate() {
        let end = starts.get(k + 1).map_or(up.len(), |(i, _)| *i);
        let seg = &up[*at..end];
        match *marker {
            "Table::create()" => {
                if let Some(table) = arg_ident(seg, ".table(", &idents) {
                    let columns = all_args(seg, ".col(", &idents);
                    out.push(Effect::CreateTable { table, columns });
                }
            }
            "Table::alter()" => {
                if let Some(table) = arg_ident(seg, ".table(", &idents) {
                    let mut added = all_args(seg, ".add_column(", &idents);
                    added.extend(all_args(seg, ".add_column_if_not_exists(", &idents));
                    let dropped = all_args(seg, ".drop_column(", &idents);
                    let renamed = seg
                        .match_indices(".rename_column(")
                        .filter_map(|(i, m)| {
                            let inner = paren_inner(&seg[i + m.len() - 1..])?;
                            let mut it = inner.split(',').filter_map(|p| path_ident(p, &idents));
                            Some((it.next()?, it.next()?))
                        })
                        .collect();
                    out.push(Effect::AlterTable {
                        table,
                        added,
                        dropped,
                        renamed,
                    });
                }
            }
            "Table::drop()" => {
                if let Some(table) = arg_ident(seg, ".table(", &idents) {
                    out.push(Effect::DropTable { table });
                }
            }
            "Index::create()" => {
                if let Some(name) = string_arg(seg, ".name(") {
                    out.push(Effect::CreateIndex {
                        name,
                        unique: seg.contains(".unique()"),
                    });
                }
            }
            "Index::drop()" => {
                if let Some(name) = string_arg(seg, ".name(") {
                    out.push(Effect::DropIndex { name });
                }
            }
            _ => {
                for stmt in string_literals(seg) {
                    for part in stmt.split(';') {
                        let head: String = part
                            .split_whitespace()
                            .take(6)
                            .collect::<Vec<_>>()
                            .join(" ");
                        let lower = head.to_lowercase();
                        if ["create ", "alter ", "drop "]
                            .iter()
                            .any(|k| lower.starts_with(k))
                        {
                            out.push(Effect::Sql { head });
                        }
                    }
                }
            }
        }
    }
    out
}

/// The text of `fn up` (to `fn down`, or the end).
fn up_body(source: &str) -> &str {
    let start = source.find("fn up").unwrap_or(0);
    let rest = &source[start..];
    let end = rest.find("fn down").unwrap_or(rest.len());
    &rest[..end]
}

/// `#[sea_orm(iden = "x")]` on enum variants: (`Enum::Variant`) → name.
fn iden_overrides(source: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut current_enum: Option<String> = None;
    let mut pending: Option<String> = None;
    for line in source.lines() {
        let l = line.trim();
        if let Some(rest) = l
            .strip_prefix("enum ")
            .or_else(|| l.strip_prefix("pub enum "))
        {
            current_enum = rest
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .map(str::to_owned);
            continue;
        }
        if l.starts_with('}') {
            current_enum = None;
            continue;
        }
        if let Some(e) = &current_enum {
            if l.contains("iden") && l.starts_with("#[") {
                pending = l.split('"').nth(1).map(str::to_owned);
            } else if !l.is_empty() && !l.starts_with("//") && !l.starts_with("#[") {
                let variant: String = l
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if let Some(name) = pending.take() {
                    out.insert(format!("{e}::{variant}"), name);
                }
            }
        }
    }
    out
}

/// SeaORM's `DeriveIden` naming: `Enum::Table` is the enum in snake_case, any other variant is
/// the variant in snake_case, unless an `iden = "…"` override says otherwise.
fn path_ident(text: &str, idents: &HashMap<String, String>) -> Option<String> {
    // The first `Enum::Variant` (both CamelCase): `ColumnDef::new(Users::Email)` is `Users::Email`.
    let ident = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    };
    text.match_indices("::").find_map(|(at, _)| {
        let before: String = text[..at]
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let after = ident(&text[at + 2..]);
        let camel = |s: &str| s.starts_with(|c: char| c.is_ascii_uppercase());
        if !camel(&before) || !camel(&after) {
            return None;
        }
        if let Some(name) = idents.get(&format!("{before}::{after}")) {
            return Some(name.clone());
        }
        Some(if after == "Table" {
            snake(&before)
        } else {
            snake(&after)
        })
    })
}

pub fn snake(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if i > 0 && (prev_lower || (next_lower && chars[i - 1].is_uppercase())) {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The text inside the parenthesis that `s` starts with (balanced).
fn paren_inner(s: &str) -> Option<&str> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return s.get(1..i);
                }
            }
            _ => {}
        }
    }
    None
}

fn arg_ident(seg: &str, call: &str, idents: &HashMap<String, String>) -> Option<String> {
    let i = seg.find(call)?;
    let inner = paren_inner(&seg[i + call.len() - 1..])?;
    path_ident(inner, idents)
}

fn all_args(seg: &str, call: &str, idents: &HashMap<String, String>) -> Vec<String> {
    seg.match_indices(call)
        .filter_map(|(i, _)| {
            let inner = paren_inner(&seg[i + call.len() - 1..])?;
            path_ident(inner, idents)
        })
        .collect()
}

fn string_arg(seg: &str, call: &str) -> Option<String> {
    let i = seg.find(call)?;
    let inner = paren_inner(&seg[i + call.len() - 1..])?;
    let inner = inner.trim();
    inner
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .map(str::to_owned)
}

/// String literals in a segment (plain and `r#"…"#`).
fn string_literals(seg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = seg;
    while let Some(i) = rest.find('"') {
        let raw = rest[..i].ends_with("r#");
        let body = &rest[i + 1..];
        let end = if raw {
            body.find("\"#")
        } else {
            body.find('"')
        };
        let Some(end) = end else { break };
        out.push(body[..end].to_owned());
        rest = &body[end + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
use sea_orm_migration::{prelude::*, schema::*};
pub struct Migration;
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(LabOrders::Table)
                .if_not_exists()
                .col(big_pk_auto(LabOrders::Id))
                .col(string(LabOrders::TotalCents))
                .col(ColumnDef::new(LabOrders::Note).text())
                .to_owned(),
        ).await?;
        m.alter_table(Table::alter().table(LabOrders::Table).add_column(string(LabOrders::Status)).drop_column(LabOrders::Note).to_owned()).await?;
        m.create_index(Index::create().name("lab_orders_status_idx").table(LabOrders::Table).col(LabOrders::Status).unique().to_owned()).await?;
        m.get_connection().execute_unprepared("CREATE INDEX IF NOT EXISTS raw_idx ON lab_orders (status)").await?;
        Ok(())
    }
    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(LabOrders::Table).to_owned()).await
    }
}
#[derive(DeriveIden)]
enum LabOrders {
    Table,
    Id,
    #[sea_orm(iden = "total")]
    TotalCents,
    Note,
    Status,
}
"#;

    #[test]
    fn migration_effects_are_derived_from_the_source() {
        let effects = parse(SOURCE);
        assert_eq!(
            effects[0],
            Effect::CreateTable {
                table: "lab_orders".into(),
                columns: vec!["id".into(), "total".into(), "note".into()],
            }
        );
        assert_eq!(
            effects[1],
            Effect::AlterTable {
                table: "lab_orders".into(),
                added: vec!["status".into()],
                dropped: vec!["note".into()],
                renamed: vec![],
            }
        );
        assert_eq!(
            effects[2],
            Effect::CreateIndex {
                name: "lab_orders_status_idx".into(),
                unique: true
            }
        );
        assert!(matches!(&effects[3], Effect::Sql { head } if head.starts_with("CREATE INDEX")));
        assert_eq!(effects.len(), 4, "down() is not part of what it did");
        assert_eq!(snake("LabOrderItems"), "lab_order_items");
        assert_eq!(snake("HTTPCache"), "http_cache");
    }

    fn migration(name: &str, applied: bool, effects: Vec<Effect>) -> MigrationInfo {
        MigrationInfo {
            name: name.into(),
            applied_at: applied.then(Utc::now),
            area: "app",
            file: None,
            effects,
            edited: false,
        }
    }

    fn live(name: &str, cols: &[&str]) -> Table {
        Table {
            name: name.into(),
            rows: Some(0),
            bytes: None,
            columns: cols
                .iter()
                .map(|c| crate::kernel::dashboard::schema::Column {
                    name: (*c).into(),
                    ty: "text".into(),
                    nullable: false,
                    pk: false,
                })
                .collect(),
            indexes: vec![],
            fks: vec![],
        }
    }

    #[test]
    fn drift_between_migrations_and_the_database_is_found() {
        let ms = vec![
            migration(
                "m1_create_a",
                true,
                vec![Effect::CreateTable {
                    table: "a".into(),
                    columns: vec!["id".into(), "x".into()],
                }],
            ),
            migration(
                "m2_create_b",
                true,
                vec![Effect::CreateTable {
                    table: "b".into(),
                    columns: vec!["id".into()],
                }],
            ),
            migration(
                "m3_create_c",
                false,
                vec![Effect::CreateTable {
                    table: "c".into(),
                    columns: vec!["id".into()],
                }],
            ),
        ];
        let tables = vec![
            live("a", &["id", "y"]),
            live("hand_made", &["id"]),
            live("seaql_migrations", &["version"]),
        ];
        let keys: Vec<String> = drift(&ms, &tables).into_iter().map(|d| d.key).collect();
        assert!(
            keys.contains(&"drift-missing-columns-a".to_owned()),
            "{keys:?}"
        );
        assert!(keys.contains(&"drift-extra-columns-a".to_owned()));
        assert!(keys.contains(&"drift-missing-table-b".to_owned()));
        assert!(keys.contains(&"drift-unmigrated-table-hand_made".to_owned()));
        assert!(
            !keys.iter().any(|k| k.ends_with("-c")),
            "pending migrations imply nothing yet"
        );
        assert!(!keys.iter().any(|k| k.contains("seaql")));
    }

    #[test]
    fn a_migration_edited_after_it_was_applied_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir
            .path()
            .join("storage/dashboard/migration-checksums.json");
        let mut sources = BTreeMap::new();
        sources.insert("m1_create_a".to_owned(), "fn up() {}".to_owned());
        assert!(
            check_ledger(&ledger, &sources).is_empty(),
            "first sight records"
        );
        assert!(ledger.exists());
        assert!(check_ledger(&ledger, &sources).is_empty(), "unchanged");
        sources.insert("m1_create_a".to_owned(), "fn up() { edited }".to_owned());
        assert!(check_ledger(&ledger, &sources).contains("m1_create_a"));
        let mut m = migration("m1_create_a", true, vec![]);
        m.edited = true;
        let d = drift(&[m], &[]);
        assert_eq!(d[0].key, "drift-edited-m1_create_a");
    }
}
