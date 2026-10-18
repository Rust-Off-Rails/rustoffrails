//! `cargo offrails generate <kind> …` (`g`) and `cargo offrails destroy <kind> <name>` (`d`):
//! Rails' generators as code, not recipes. Each generator builds a `Plan` (files to create,
//! wiring at `samples:` anchors, crates), which is rendered in memory and checked before
//! anything is written, then formatted and recorded in `docs/generated.toml`.
//! The Markdown recipes in `.offrails/generators/` say why, and what to customize after.

pub mod api;
pub mod app;
pub mod channel;
pub mod controller;
pub mod feature;
pub mod fields;
pub mod html;
pub mod job;
pub mod mailer;
pub mod manifest;
pub mod migration;
pub mod model;
pub mod names;
pub mod page;
pub mod plan;
pub mod resource;
pub mod routes_md;
pub mod snippet;
pub mod tenancy;
pub mod testgen;

#[cfg(test)]
mod golden;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use app::AppInfo;
use manifest::{Entry, Manifest};
use plan::{Action, Plan};

use crate::util::{self, Fail, Result, rel};

pub const USAGE: &str = "usage: cargo offrails generate <kind> <name> [args…] [options]   (alias: cargo offrails g)
       cargo offrails destroy <kind> <name> [--dry-run]               (alias: cargo offrails d)

kinds:
  migration <name> [field:type…]       create_<table> | add_<x>_to_<table> | remove_<x>_from_<table> | <any>
  model <Name> field:type…             migration + entity, validation, scoped queries + factory + model tests
  resource <name> field:type…          (scaffold) model + htmx CRUD: 7 actions, pages, routes, nav, request tests
  controller <name> [action[:/path]…]  handlers + pages + routes + request tests (default action: index);
                                       path params: show:/blog/{slug} (or --path /blog/{slug} for one action)
  page <name>                          a public page: route, handler, template, sitemap entry, test [--nav]
                                       [--path /about-us | --path /people/{handle}]
  api <name> field:type…               versioned JSON API under /api/v1 (bearer tokens, OpenAPI, tests);
                                       makes the model too unless it exists
  job <Name> [arg:type…]               a background job (jobs part): worker, registration, test [--queue q]
  mailer <name> [action…]              a mailer (mailer part): HTML + text templates, previews, tests
  channel <name>                       a realtime topic (realtime part): authorization, broadcast, test
  test <request|model|job|unit> <name> a test file with a first test to replace, registered

field types: string text integer bigint boolean decimal{p.s} money datetime date uuid json references{model}
  modifiers: name:type?  (nullable; or name:type:optional, which zsh won't glob)   :unique   :index
  e.g. slug:string:unique 'published_at:datetime?' price:money book:references

options:
  --dry-run      print the plan (files, wiring, crates); write nothing
  --force        regenerate over files this generator wrote and nobody changed since
  --public       model/resource/api/controller: no accounts, anyone may read and write (refused otherwise
                 when the app has no auth part)
  --no-nav       resource: no nav link          --nav   page/controller: add a nav link
  --layout <l>   resource/controller/page: the layout the pages extend: application (default, the top
                 nav), sidebar, marketing, auth or docs (guides/views.md#layouts)
  --queue <q>    job: its queue (default: default)
  --no-fmt       don't run cargo fmt afterwards
  --app <dir>    the app (default: .)

  --forget       destroy: drop the record only (a generation recorded before 0.7.3; delete its files by hand)

Each feature is small files in its own folders (src/controllers/<x>/, src/models/<x>/, src/routes/<x>.rs,
templates/<x>/, tests/app/<x>/) plus one line per spot in shared files, marked `// <x>`. Every generated
file is recorded in docs/generated.toml; destroy removes the unchanged ones and the marked lines, and
reports what it kept. Commit after each run. Smaller recipes to copy: cargo offrails snippets.
Guides: .offrails/generators/";

/// Options shared by every generator, plus the choices a previous run recorded.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub dry_run: bool,
    pub force: bool,
    pub fmt: bool,
    pub public: bool,
    pub no_nav: bool,
    pub nav: bool,
    pub values: BTreeMap<String, String>,
    /// From docs/generated.toml when re-planning (destroy, --force): stamp, scope, …
    pub recorded: BTreeMap<String, String>,
    /// Every shared file some generation recorded.
    pub manifest_shared: BTreeSet<String>,
    /// Re-planning a recorded generation (destroy, a re-run, another one's wiring): what it
    /// wrote then is what `recorded` says, markers included only when its layout had them.
    pub replan: bool,
    /// destroy: drop the record only (a generation an older offrails-gen wrote).
    pub forget: bool,
}

impl Flags {
    pub fn recorded(&self, key: &str) -> Option<String> {
        self.recorded.get(key).cloned()
    }
    pub fn value(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

/// `scaffold` → `resource`, `g` aliases spelled out.
pub fn canonical(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "migration" => "migration",
        "model" => "model",
        "resource" | "scaffold" => "resource",
        "controller" => "controller",
        "page" => "page",
        "api" => "api",
        "job" => "job",
        "mailer" => "mailer",
        "channel" => "channel",
        "test" => "test",
        "snippet" => "snippet",
        _ => return None,
    })
}

/// The layout 0.7.3 writes: small files per feature, routes in `src/routes/<feature>.rs`, and
/// every wiring line marked with its owner. Recorded as `layout = "2"`.
pub const LAYOUT: &str = "2";

/// Kinds whose files moved in 0.7.3 (a split per feature): a generation an older offrails-gen
/// recorded can't be re-planned by this one.
const SPLIT_KINDS: [&str; 5] = ["resource", "model", "controller", "page", "api"];

/// A generation from before 0.7.3 of a kind whose layout changed.
fn old_layout(e: &Entry) -> bool {
    SPLIT_KINDS.contains(&e.kind.as_str())
        && e.options.get("layout").map(String::as_str) != Some(LAYOUT)
}

fn old_layout_refusal(e: &Entry) -> Fail {
    let files: Vec<&str> = e.files.keys().map(String::as_str).collect();
    Fail::new(format!(
        "`generate {kind} {name}` was recorded by an offrails-gen before 0.7.3 (one file per controller and \
         model, its rows in src/routes.rs); this one writes the split layout and can't re-plan the old one. \
         Nothing changed. Its files: {files}. To remove it, delete them and its wiring lines by hand \
         (each anchor in src/routes.rs, src/controllers/mod.rs, src/views/mod.rs, src/models/mod.rs, \
         tests/app/main.rs, the nav), then `cargo offrails destroy {kind} {name} --forget` drops the record",
        kind = e.kind,
        name = e.name,
        files = if files.is_empty() {
            "(none recorded)".to_owned()
        } else {
            files.join(", ")
        }
    ))
}

fn plan_for(kind: &str, info: &AppInfo, args: &[String], flags: &Flags) -> Result<Plan> {
    let mut plan = plan_kind(kind, info, args, flags)?;
    let marked = !flags.replan || flags.recorded("layout").as_deref() == Some(LAYOUT);
    if marked {
        let tag = plan.tag.clone().unwrap_or_else(|| {
            plan.name
                .rsplit(':')
                .next()
                .unwrap_or(&plan.name)
                .to_owned()
        });
        plan.mark_wiring(&tag);
        plan.option("layout", LAYOUT);
    }
    Ok(plan)
}

pub(crate) fn plan_kind(
    kind: &str,
    info: &AppInfo,
    args: &[String],
    flags: &Flags,
) -> Result<Plan> {
    match kind {
        "migration" => migration::plan(info, args, flags),
        "model" => model::plan(info, args, flags),
        "resource" => resource::plan(info, args, flags),
        "controller" => controller::plan(info, args, flags),
        "page" => page::plan(info, args, flags),
        "api" => api::plan(info, args, flags),
        "job" => job::plan(info, args, flags),
        "mailer" => mailer::plan(info, args, flags),
        "channel" => channel::plan(info, args, flags),
        "test" => testgen::plan(info, args, flags),
        "snippet" => snippet::plan(info, args, flags),
        other => Err(Fail::usage(format!("unknown kind '{other}'\n\n{USAGE}"))),
    }
}

/// The name a kind records its generation under (so `destroy resource post` finds `posts`).
fn entry_name(kind: &str, raw: &[String]) -> Result<String> {
    let first = raw.first().map(String::as_str).unwrap_or("");
    Ok(match kind {
        "model" => names::Names::new(first)?.singular,
        "resource" | "api" => names::Names::new(first)?.plural,
        "test" => format!(
            "{}:{}",
            first,
            names::snake(raw.get(1).map(String::as_str).unwrap_or(""))
        ),
        "job" => job::job_name(first)?,
        "snippet" => snippet::entry_name(raw),
        "mailer" => mailer::mailer_name(first)?,
        _ => names::snake(first),
    })
}

/// The wiring every other recorded generation put into the app (re-planned from the record),
/// with the generation it belongs to.
fn foreign_inserts(
    info: &AppInfo,
    manifest: &Manifest,
    kind: &str,
    name: &str,
    flags: &Flags,
) -> Vec<(String, plan::Insert)> {
    let mut out = Vec::new();
    for e in &manifest.entries {
        if e.kind == kind && e.name == name || old_layout(e) {
            continue;
        }
        let mut f = flags.clone();
        f.recorded = e.options.clone();
        f.force = false;
        f.replan = true;
        if let Ok(p) = plan_for(&e.kind, info, &e.args, &f) {
            for a in p.actions {
                if let Action::Insert(i) = a {
                    out.push((format!("{} {}", e.kind, e.name), i));
                }
            }
        }
    }
    out
}

fn describe(action: &Action) -> String {
    match action {
        Action::Create { path, .. } => format!("  create {path}"),
        Action::Shared { path, .. } => format!("  shared {path}"),
        Action::Insert(i) => format!("  wire   {} @ {}", i.file, i.anchor.describe()),
        Action::Crate { args, .. } => format!("  cargo add {args}"),
        Action::Byproduct { path, .. } => format!("  later  {path} (written by the app's tests)"),
    }
}

fn snapshot(app: &Path, paths: &[String]) -> Vec<(String, Option<String>)> {
    paths
        .iter()
        .map(|p| (p.clone(), std::fs::read_to_string(rel(app, p)).ok()))
        .collect()
}

fn restore(app: &Path, snap: &[(String, Option<String>)]) {
    for (p, content) in snap {
        let dest = rel(app, p);
        match content {
            Some(t) => {
                let _ = util::write_text(&dest, t);
            }
            None => {
                let _ = std::fs::remove_file(&dest);
            }
        }
    }
}

fn fmt(app: &Path, flags: &Flags) {
    if flags.fmt && util::have("cargo") && app.join("Cargo.toml").is_file() {
        // WHY: generated code is written close to rustfmt's style, then formatted for real, and
        // only then hashed, so the record matches the files as they stay.
        if util::run(app, "cargo", &["fmt", "--all"]).is_err() {
            eprintln!(
                "note: `cargo fmt --all` failed; fix what it printed, then run it (destroy compares hashes of formatted files)"
            );
        }
    }
}

/// `generate <kind> …`.
pub fn generate(app: &Path, kind: &str, args: &[String], mut flags: Flags) -> Result<()> {
    let Some(kind) = canonical(kind) else {
        return Err(Fail::usage(format!("unknown kind '{kind}'\n\n{USAGE}")));
    };
    if args.is_empty() {
        return Err(Fail::usage(format!(
            "generate {kind} needs a name\n\n{USAGE}"
        )));
    }
    let info = AppInfo::load(app)?;
    let mut manifest = Manifest::load(app)?;
    flags.manifest_shared = manifest
        .entries
        .iter()
        .flat_map(|e| e.shared.iter().cloned())
        .collect();
    let name = entry_name(kind, args)?;
    let mut undo_snapshot: Vec<(String, Option<String>)> = Vec::new();
    let mut owned: BTreeSet<String> = BTreeSet::new();
    if let Some(entry) = manifest.find(kind, &name).cloned() {
        if old_layout(&entry) {
            return Err(old_layout_refusal(&entry));
        }
        let mut old_flags = flags.clone();
        old_flags.recorded = entry.options.clone();
        old_flags.replan = true;
        let old = plan_for(kind, &info, &entry.args, &old_flags)?;
        let others = manifest.shared_by_others(kind, &name);
        let foreign: Vec<plan::Insert> = foreign_inserts(&info, &manifest, kind, &name, &flags)
            .into_iter()
            .map(|(_, i)| i)
            .collect();
        let undo = plan::undo(app, &old, &entry, &others, &foreign);
        let unchanged = undo.kept.is_empty();
        if entry.args == args && !flags.force {
            let missing: Vec<&str> = old
                .created()
                .into_iter()
                .filter(|p| !rel(app, p).exists())
                .collect();
            if unchanged && missing.is_empty() {
                println!(
                    "nothing to do: `generate {kind} {}` is already in this app, unchanged (docs/generated.toml)",
                    args.join(" ")
                );
                println!("  to start over: cargo offrails destroy {kind} {name}");
                return Ok(());
            }
            for k in &undo.kept {
                eprintln!("changed {k}");
            }
            for m in &missing {
                eprintln!("missing {m}");
            }
            return Err(Fail::new(format!(
                "`generate {kind} {name}` was run before, and its files changed since; nothing changed now. \
                 --force rewrites what it wrote if nobody changed it; or `cargo offrails destroy {kind} {name}`"
            )));
        }
        if !flags.force {
            return Err(Fail::new(format!(
                "`generate {kind} {name}` was already run with other arguments ({}); nothing changed. \
                 `cargo offrails destroy {kind} {name}` first, or add --force to regenerate (only over files nobody changed)",
                entry.args.join(" ")
            )));
        }
        if !unchanged {
            for k in &undo.kept {
                eprintln!("refuse {k}");
            }
            return Err(Fail::new(
                "--force only regenerates over files nobody changed since they were generated; nothing changed",
            ));
        }
        // Regenerate in place: undo the old generation (strictly), keeping its migration stamp.
        if let Some(stamp) = entry.options.get("stamp") {
            flags.recorded.insert("stamp".into(), stamp.clone());
        }
        if !flags.dry_run {
            let paths: Vec<String> = undo.files.keys().cloned().collect();
            undo_snapshot = snapshot(app, &paths);
            plan::write_undo(app, &undo)?;
        }
        owned = old.created().into_iter().map(str::to_owned).collect();
        println!("regenerate {kind} {name} (--force): the previous files were unchanged");
    }
    let plan = match plan_for(kind, &info, args, &flags) {
        Ok(p) => p,
        Err(e) => {
            restore(app, &undo_snapshot);
            return Err(e);
        }
    };
    let mut problems = plan::claimed_elsewhere(&plan, &manifest);
    let rendered = match plan::render(app, &plan, &owned) {
        Ok(r) if problems.is_empty() => r,
        Ok(_) => {
            restore(app, &undo_snapshot);
            return Err(plan::refuse(&problems));
        }
        Err(mut p) => {
            restore(app, &undo_snapshot);
            problems.append(&mut p);
            return Err(plan::refuse(&problems));
        }
    };
    println!(
        "{} {kind} {name}{}",
        if flags.dry_run {
            "plan for"
        } else {
            "generate"
        },
        if flags.dry_run {
            " (--dry-run: nothing written)"
        } else {
            ""
        }
    );
    for a in &plan.actions {
        if let Action::Crate { args, .. } = a
            && !rendered.crates.contains(args)
        {
            continue;
        }
        if let Action::Shared { path, .. } = a
            && rendered
                .files
                .get(path)
                .is_some_and(|(before, _)| before.is_some())
        {
            continue;
        }
        if let Action::Insert(i) = a
            && let Some((Some(before), _)) = rendered.files.get(&i.file)
            && plan::find_block(before, &i.text).is_some()
        {
            println!(
                "  ok     {} @ {} (already there)",
                i.file,
                i.anchor.describe()
            );
            continue;
        }
        println!("{}", describe(a));
    }
    for n in &rendered.notes {
        println!("  {n}");
    }
    if flags.dry_run {
        print_next(&plan, app);
        return Ok(());
    }
    for c in &rendered.crates {
        let mut a: Vec<&str> = vec!["add", "-q"];
        a.extend(c.split_whitespace());
        if let Err(e) = util::run(app, "cargo", &a) {
            restore(app, &undo_snapshot);
            return Err(e);
        }
    }
    plan::write(app, &rendered.files)?;
    fmt(app, &flags);
    let mut files = BTreeMap::new();
    for p in plan.created() {
        let text = util::read_text(&rel(app, p))?;
        files.insert(p.to_owned(), plan::hash(&text));
    }
    manifest.upsert(Entry {
        kind: kind.to_owned(),
        name: name.clone(),
        args: args.to_vec(),
        options: plan.options.clone(),
        date: util::today_utc(),
        files,
        shared: plan.shared_paths().into_iter().map(str::to_owned).collect(),
    });
    manifest.save(app)?;
    if routes_md::refresh(app)? {
        println!("  routes docs/routes.md (from ROUTES)");
    }
    println!(
        "\ndone: {} (recorded in docs/generated.toml).",
        plan.command()
    );
    print_next(&plan, app);
    Ok(())
}

fn print_next(plan: &Plan, app: &Path) {
    println!("Next, in {}:", app.display());
    for n in &plan.next {
        println!("  {n}");
    }
    println!(
        "  cargo offrails verify --fast                # fmt, clippy, tests while iterating; --full before calling it done"
    );
}

/// `destroy <kind> <name>`.
pub fn destroy(app: &Path, kind: &str, args: &[String], mut flags: Flags) -> Result<()> {
    let Some(kind) = canonical(kind) else {
        return Err(Fail::usage(format!("unknown kind '{kind}'\n\n{USAGE}")));
    };
    if args.is_empty() {
        return Err(Fail::usage(format!(
            "destroy {kind} needs the name it was generated with"
        )));
    }
    let info = AppInfo::load(app)?;
    let mut manifest = Manifest::load(app)?;
    let name = entry_name(kind, args)?;
    let Some(entry) = manifest.find(kind, &name).cloned() else {
        let known: Vec<String> = manifest
            .entries
            .iter()
            .map(|e| format!("{} {}", e.kind, e.name))
            .collect();
        return Err(Fail::usage(format!(
            "docs/generated.toml has no `{kind} {name}`; destroy removes only what a generator wrote{}",
            if known.is_empty() {
                " (nothing is recorded)".to_owned()
            } else {
                format!(". Recorded: {}", known.join(", "))
            }
        )));
    };
    if flags.forget {
        let files: Vec<&str> = entry.files.keys().map(String::as_str).collect();
        if flags.dry_run {
            println!("plan to forget {kind} {name} (--dry-run: the record stays)");
            return Ok(());
        }
        manifest.remove(kind, &name);
        manifest.save(app)?;
        println!(
            "forgot {kind} {name}: docs/generated.toml no longer records it; nothing else changed.\n  \
             its files (delete them by hand if they're still there): {}",
            if files.is_empty() {
                "(none)".to_owned()
            } else {
                files.join(", ")
            }
        );
        return Ok(());
    }
    if old_layout(&entry) {
        return Err(old_layout_refusal(&entry));
    }
    flags.manifest_shared = manifest
        .entries
        .iter()
        .flat_map(|e| e.shared.iter().cloned())
        .collect();
    flags.recorded = entry.options.clone();
    flags.replan = true;
    let plan = plan_for(kind, &info, &entry.args, &flags)?;
    let others = manifest.shared_by_others(kind, &name);
    let foreign = foreign_inserts(&info, &manifest, kind, &name, &flags);
    // A later generation wired into a file this one wrote (`books` → `has_many` in author.rs):
    // destroying this first would break it.
    let created: Vec<&str> = plan.created();
    let mut blockers: Vec<String> = Vec::new();
    for (owner, ins) in &foreign {
        if created.contains(&ins.file.as_str())
            && info
                .read(&ins.file)
                .is_some_and(|t| plan::find_block(&t, &ins.text).is_some())
            && !blockers.contains(owner)
        {
            blockers.push(owner.clone());
        }
    }
    if !blockers.is_empty() {
        return Err(Fail::new(format!(
            "`{}` wired into files `{kind} {name}` wrote; destroy {} first (cargo offrails destroy …), then this. Nothing changed",
            blockers.join("`, `"),
            if blockers.len() == 1 { "it" } else { "them" }
        )));
    }
    let foreign: Vec<plan::Insert> = foreign.into_iter().map(|(_, i)| i).collect();
    let undo = plan::undo(app, &plan, &entry, &others, &foreign);
    println!(
        "{} {kind} {name}{}",
        if flags.dry_run {
            "plan to destroy"
        } else {
            "destroy"
        },
        if flags.dry_run {
            " (--dry-run: nothing removed)"
        } else {
            ""
        }
    );
    if plan.migrations {
        println!(
            "  note   its migration goes too: if it ran (db migrate), roll it back first, while the file exists:\n         cargo run -q -- db rollback   (the dev database keeps the table otherwise)"
        );
    }
    for (p, c) in &undo.files {
        match c {
            None => println!("  remove {p}"),
            Some(_) => println!("  unwire {p}"),
        }
    }
    for k in &undo.kept {
        println!("  keep   {k}");
    }
    for a in &plan.actions {
        if let Action::Crate { args, .. } = a {
            println!(
                "  keep   Cargo.toml: `cargo add {args}` stays (cargo remove it if nothing else uses it)"
            );
        }
    }
    if flags.dry_run {
        return Ok(());
    }
    plan::write_undo(app, &undo)?;
    fmt(app, &flags);
    manifest.remove(kind, &name);
    manifest.save(app)?;
    if routes_md::refresh(app)? {
        println!("  routes docs/routes.md (from ROUTES)");
    }
    println!(
        "\ndone: destroyed {kind} {name}{}.",
        if undo.kept.is_empty() {
            String::new()
        } else {
            format!(
                "; {} thing(s) kept because they changed (listed above)",
                undo.kept.len()
            )
        }
    );
    Ok(())
}
