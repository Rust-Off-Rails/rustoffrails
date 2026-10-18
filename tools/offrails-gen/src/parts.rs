//! Parts: optional kernel pieces (mailer, jobs, accounts, realtime, …) added to an app at
//! `samples: <anchor>` comments. A part is a sample from the Rust off Rails samples repo, vendored
//! into the release at `template/parts/<name>/`, in the samples' format (`sample.toml` + `files/`).
//! This is the same mechanism as the samples repo's `tools/install.sh`, and it records the same
//! `installed sample <name> <version>` line in docs/decisions.md, so either tool sees the other's work.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::util::{self, Fail, Result, rel};

/// The crate names part files are written with; both become the app's package name.
const HOST_CRATES: [&str; 2] = ["offrails_host", "offrails_app"];

#[derive(Debug, Clone, Default)]
pub struct Wire {
    pub file: String,
    pub anchor: String,
    pub when: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Part {
    pub name: String,
    pub version: String,
    pub summary: String,
    pub depends_on: Vec<String>,
    pub crates: Vec<String>,
    pub wires: Vec<Wire>,
    pub dir: PathBuf,
}

fn unquote(s: &str) -> String {
    let s = s.trim().trim_end_matches(',').trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
        .to_owned()
}

fn toml_list(v: &str) -> Vec<String> {
    v.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse the strict TOML subset of samples/FORMAT.md: `key = "value"` lines, the `depends_on` and
/// `crates` arrays, and `[[wire]]` tables whose `text` is a `'''` block.
pub fn parse(name: &str, dir: &Path) -> Result<Part> {
    parse_file(name, dir, &dir.join("sample.toml"))
}

/// `parse`, from another file of the same format (a snippet's `snippet.toml`).
pub fn parse_file(name: &str, dir: &Path, path: &Path) -> Result<Part> {
    let text = util::read_text(path)?;
    let mut part = Part {
        name: name.to_owned(),
        dir: dir.to_path_buf(),
        ..Part::default()
    };
    let mut wire: Option<Wire> = None;
    let mut in_text = false;
    let mut in_crates = false;
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if in_text {
            if line.trim_end() == "'''" {
                in_text = false;
            } else if let Some(w) = wire.as_mut() {
                w.text.push_str(line);
                w.text.push('\n');
            }
            continue;
        }
        if in_crates {
            let t = line.trim();
            if t.starts_with(']') {
                in_crates = false;
            } else if t.starts_with('"') {
                part.crates.push(unquote(t));
            }
            continue;
        }
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t == "[[wire]]" {
            if let Some(w) = wire.take() {
                part.wires.push(w);
            }
            wire = Some(Wire::default());
            section = "wire".into();
            continue;
        }
        if t.starts_with('[') {
            if let Some(w) = wire.take() {
                part.wires.push(w);
            }
            section = t.to_owned();
            continue;
        }
        let Some((key, value)) = t.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if section == "wire" {
            let w = wire.get_or_insert_with(Wire::default);
            match key {
                "file" => w.file = unquote(value),
                "anchor" => w.anchor = unquote(value),
                "when" => w.when = Some(unquote(value)),
                "text" => {
                    let rest = value.trim_start_matches("'''");
                    if !rest.is_empty() {
                        w.text.push_str(rest);
                        w.text.push('\n');
                    }
                    in_text = true;
                }
                _ => {}
            }
            continue;
        }
        if !section.is_empty() {
            continue;
        }
        match key {
            "name" => part.name = unquote(value),
            "version" => part.version = unquote(value),
            "summary" => part.summary = unquote(value),
            "depends_on" => part.depends_on = toml_list(value),
            "crates" => {
                if value.trim() == "[" {
                    in_crates = true;
                } else {
                    part.crates = toml_list(value);
                }
            }
            _ => {}
        }
    }
    if let Some(w) = wire.take() {
        part.wires.push(w);
    }
    if in_text {
        return Err(Fail::new(format!(
            "{}: a ''' text block is not closed",
            path.display()
        )));
    }
    Ok(part)
}

/// Every part shipped in `<fw>/template/parts/`, by name.
pub fn catalog(fw: &Path) -> Result<BTreeMap<String, Part>> {
    let root = fw.join("template").join("parts");
    let mut out = BTreeMap::new();
    for entry in util::ctx(fs::read_dir(&root), "reading", &root)? {
        let entry = entry?;
        let dir = entry.path();
        if dir.join("sample.toml").is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            out.insert(name.clone(), parse(&name, &dir)?);
        }
    }
    Ok(out)
}

/// Parts already in the app, from docs/decisions.md (`installed sample <name> `).
pub fn installed(app: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(rel(app, "docs/decisions.md")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = text
        .lines()
        .filter_map(|l| l.split_once(": installed sample ").map(|(_, r)| r))
        .filter_map(|r| r.split_whitespace().next().map(str::to_owned))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Install order for `targets`: dependencies first, each once, already-installed ones skipped.
/// A diamond (auth-core → mailer, auth-core → dev-mailbox → mailer) is not a cycle.
pub fn resolve(
    catalog: &BTreeMap<String, Part>,
    targets: &[String],
    have: &[String],
    force: bool,
) -> Result<Vec<String>> {
    #[allow(clippy::too_many_arguments)]
    fn visit(
        name: &str,
        catalog: &BTreeMap<String, Part>,
        have: &[String],
        explicit: &[String],
        force: bool,
        visiting: &mut Vec<String>,
        done: &mut Vec<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if done.iter().any(|d| d == name) {
            return Ok(());
        }
        if visiting.iter().any(|d| d == name) {
            return Err(Fail::new(format!("dependency cycle at {name}")));
        }
        let part = catalog.get(name).ok_or_else(|| {
            Fail::usage(format!(
                "no part named '{name}' (cargo offrails list-parts)"
            ))
        })?;
        visiting.push(name.to_owned());
        for dep in &part.depends_on {
            visit(dep, catalog, have, explicit, force, visiting, done, order)?;
        }
        visiting.pop();
        let reinstall = force && explicit.iter().any(|e| e == name);
        if !have.iter().any(|h| h == name) || reinstall {
            order.push(name.to_owned());
        }
        done.push(name.to_owned());
        Ok(())
    }
    let (mut visiting, mut done, mut order) = (Vec::new(), Vec::new(), Vec::new());
    for t in targets {
        visit(
            t,
            catalog,
            have,
            targets,
            force,
            &mut visiting,
            &mut done,
            &mut order,
        )?;
    }
    Ok(order)
}

pub fn rename_crate(text: &str, krate: &str) -> String {
    let mut s = text.to_owned();
    for host in HOST_CRATES {
        if host != krate {
            s = s.replace(host, krate);
        }
    }
    s
}

fn trim(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\r'])
}

pub fn is_anchor_line(line: &str, anchor: &str) -> bool {
    // The anchor name, then the end of the line or the comment's closer (`#}`, `*/`, `-->`):
    // `samples: routes` is not `samples: routes table`.
    let needle = format!("samples: {anchor}");
    let l = line.trim_end_matches(['\r', '\n']);
    l.match_indices(&needle).any(|(i, _)| {
        let rest = l[i + needle.len()..].trim();
        rest.is_empty()
            || rest.starts_with("#}")
            || rest.starts_with("*/")
            || rest.starts_with("-->")
    })
}

fn anchor_stands_alone(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("{#") || t.starts_with('#') || t.starts_with("/*")
}

/// `} // samples: x` (rustfmt pulls a comment after a last match arm ending in `}` onto that
/// brace): the code before the anchor is only closing punctuation, so the anchor can safely move
/// back onto its own line, at the same indent. Returns the line split in two.
fn split_trailing_anchor(line: &str, anchor: &str) -> Option<String> {
    let bare = line.trim_end_matches(['\r', '\n']);
    if !is_anchor_line(bare, anchor) || anchor_stands_alone(bare) {
        return None;
    }
    let at = bare.find("//")?;
    let (code, comment) = bare.split_at(at);
    let indent: String = code
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let closers = code.trim();
    if closers.is_empty()
        || !closers
            .chars()
            .all(|c| matches!(c, '}' | ')' | ']' | ',' | ';'))
    {
        return None;
    }
    Some(format!("{indent}{closers}\n{indent}{}", comment.trim_end()))
}

/// Apply `split_trailing_anchor` to every line of a file; `None` when nothing needed it.
pub fn heal_anchor(file: &str, anchor: &str) -> Option<String> {
    let nl = util::newline_of(file);
    let mut changed = false;
    let mut out = String::with_capacity(file.len() + 8);
    for line in file.split_inclusive('\n') {
        match split_trailing_anchor(line, anchor) {
            Some(fixed) => {
                out.push_str(&fixed.replace('\n', nl));
                out.push_str(nl);
                changed = true;
            }
            None => out.push_str(line),
        }
    }
    changed.then_some(out)
}

/// Insert `text` above the anchor line in `file`, indented like it. Idempotent: when the text's
/// non-blank lines already appear together in the file, nothing changes. Returns whether it changed.
pub fn insert_at(file: &str, anchor: &str, text: &str) -> Option<String> {
    let want: String = text
        .lines()
        .map(trim)
        .filter(|l| !l.is_empty())
        .map(|l| format!("{l}\n"))
        .collect();
    let have: String = file
        .lines()
        .map(trim)
        .filter(|l| !l.is_empty())
        .map(|l| format!("{l}\n"))
        .collect();
    if want.is_empty() || have.contains(&want) {
        return None;
    }
    let nl = util::newline_of(file);
    let mut out = String::with_capacity(file.len() + text.len());
    let mut done = false;
    for line in file.split_inclusive('\n') {
        if !done && is_anchor_line(line, anchor) {
            let indent: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            for t in text.lines() {
                let t = t.trim_end_matches('\r');
                if trim(t).is_empty() {
                    out.push_str(nl);
                } else {
                    out.push_str(&format!("{indent}{t}{nl}"));
                }
            }
            done = true;
        }
        out.push_str(line);
    }
    done.then_some(out)
}

pub struct AddOptions {
    pub force: bool,
    pub dry_run: bool,
    pub offline: bool,
    pub fmt: bool,
    /// Called from `new`: skip the closing "next steps" (new prints its own).
    pub quiet_next: bool,
}

/// Add parts (and their dependencies) to the app. One code path for `new --parts` and `add-part`.
pub fn add(fw: &Path, app: &Path, targets: &[String], opts: &AddOptions) -> Result<Vec<String>> {
    let catalog = catalog(fw)?;
    let all: Vec<String>;
    let targets = if targets.iter().any(|t| t == "all") {
        all = catalog.keys().cloned().collect();
        &all[..]
    } else {
        targets
    };
    let krate = util::package_name(app)?;
    let have = installed(app);
    for t in targets {
        if !catalog.contains_key(t) {
            return Err(Fail::usage(format!(
                "no part named '{t}' (cargo offrails list-parts)"
            )));
        }
    }
    let order = resolve(&catalog, targets, &have, opts.force)?;
    for t in targets {
        if have.contains(t) && !order.contains(t) {
            println!("skip   {t} (already installed; --force reinstalls)");
        }
    }
    let mut noted: Vec<&str> = Vec::new();
    for o in &order {
        for dep in &catalog[o].depends_on {
            if have.contains(dep) && !noted.contains(&dep.as_str()) {
                println!("skip   {dep} (already installed)");
                noted.push(dep);
            }
        }
    }
    if order.is_empty() {
        println!("nothing to do: {} already installed", targets.join(", "));
        return Ok(order);
    }
    let installing = |n: &str| order.iter().any(|o| o == n) || have.iter().any(|h| h == n);

    // Wires of parts already in the app that wait for one being installed now (`when`).
    let mut later: Vec<(String, Wire)> = Vec::new();
    for h in &have {
        if order.contains(h) {
            continue;
        }
        if let Some(p) = catalog.get(h) {
            for w in &p.wires {
                if let Some(when) = &w.when
                    && order.contains(when)
                {
                    later.push((h.clone(), w.clone()));
                }
            }
        }
    }

    // Check everything before changing anything.
    let mut problems = Vec::new();
    let check_wire = |owner: &str, w: &Wire, problems: &mut Vec<String>| {
        let target = rel(app, &w.file);
        if !target.is_file() {
            let created = order
                .iter()
                .any(|o| rel(&catalog[o].dir.join("files"), &w.file).is_file());
            if !created {
                problems.push(format!(
                    "{owner}: wiring target {} doesn't exist in the app",
                    w.file
                ));
            }
            return;
        }
        let Ok(text) = fs::read_to_string(&target) else {
            problems.push(format!("{owner}: can't read {}", w.file));
            return;
        };
        match text.lines().find(|l| is_anchor_line(l, &w.anchor)) {
            None => problems.push(format!(
                "{owner}: {} has no 'samples: {}' anchor; add the anchor on a line of its own at the spot \
                 .offrails/generators/new.md (step 4.13) names, or apply this wiring by hand \
                 (.offrails/template/parts/{owner}/README.md, Wiring)",
                w.file, w.anchor
            )),
            Some(l) if !anchor_stands_alone(l) && split_trailing_anchor(l, &w.anchor).is_none() => problems.push(format!(
                "{owner}: the 'samples: {}' anchor in {} shares its line with code; move it onto its own line",
                w.anchor, w.file
            )),
            Some(_) => {}
        }
    };
    for name in &order {
        let part = &catalog[name];
        for f in util::files_under(&part.dir.join("files"))? {
            let dest = rel(app, &f);
            if dest.exists() && !opts.force {
                let src = fs::read(rel(&part.dir.join("files"), &f))?;
                let same = match (util::as_text(&src), fs::read(&dest)) {
                    (Some(s), Ok(d)) => {
                        let d = String::from_utf8_lossy(&d).replace("\r\n", "\n");
                        rename_crate(s, &krate).replace("\r\n", "\n") == d
                    }
                    (None, Ok(d)) => d == src,
                    _ => false,
                };
                if !same {
                    problems.push(format!(
                        "{name}: {f} already exists in the app and differs (--force overwrites it)"
                    ));
                }
            }
        }
        for w in &part.wires {
            if w.when.as_deref().is_none_or(installing) {
                check_wire(name, w, &mut problems);
            }
        }
    }
    for (owner, w) in &later {
        check_wire(owner, w, &mut problems);
    }
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("refuse {p}");
        }
        return Err(Fail::new(format!(
            "nothing changed ({} problem(s))",
            problems.len()
        )));
    }

    let mut touched: Vec<String> = Vec::new();
    let mut migrations = false;
    let mut env = false;
    let apply = |owner: &str, w: &Wire, touched: &mut Vec<String>| -> Result<()> {
        let target = rel(app, &w.file);
        let mut text = util::read_text(&target)?;
        if let Some(healed) = heal_anchor(&text, &w.anchor) {
            util::write_text(&target, &healed)?;
            println!(
                "  fix  {}: the 'samples: {}' anchor back on a line of its own (rustfmt had moved it)",
                w.file, w.anchor
            );
            text = healed;
        }
        let wire = rename_crate(&w.text, &krate);
        match insert_at(&text, &w.anchor, &wire) {
            Some(new) => {
                util::write_text(&target, &new)?;
                println!(
                    "  wire {} @ samples: {}{}",
                    w.file,
                    w.anchor,
                    if owner.is_empty() {
                        String::new()
                    } else {
                        format!(" (for {owner})")
                    }
                );
                if !touched.contains(&w.file) {
                    touched.push(w.file.clone());
                }
            }
            None => println!("  ok   {} @ samples: {} (already there)", w.file, w.anchor),
        }
        Ok(())
    };
    let mut deferred: Vec<(String, Wire)> = Vec::new();
    for name in &order {
        let part = &catalog[name];
        println!("add {} {} -> {}", part.name, part.version, app.display());
        if opts.dry_run {
            for c in &part.crates {
                println!("  cargo add {c}");
            }
            for f in util::files_under(&part.dir.join("files"))? {
                println!("  copy {f}");
            }
            for w in &part.wires {
                let when = w
                    .when
                    .as_ref()
                    .map(|w| format!(" (when {w} is installed)"))
                    .unwrap_or_default();
                println!("  wire {} @ samples: {}{when}", w.file, w.anchor);
            }
            continue;
        }
        for c in &part.crates {
            println!("  cargo add {c}");
            let mut args: Vec<&str> = vec!["add", "-q"];
            args.extend(c.split_whitespace());
            if opts.offline {
                args.push("--offline");
            }
            util::run(app, "cargo", &args)?;
        }
        for f in util::files_under(&part.dir.join("files"))? {
            let src = rel(&part.dir.join("files"), &f);
            let dest = rel(app, &f);
            util::copy_file(&src, &dest)?;
            let bytes = fs::read(&src)?;
            if let Some(text) = util::as_text(&bytes) {
                let renamed = rename_crate(text, &krate);
                if renamed != text {
                    util::write_text(&dest, &renamed)?;
                }
            }
            if f.starts_with("migration/") {
                migrations = true;
            }
            println!("  copy {f}");
        }
        for w in &part.wires {
            match &w.when {
                Some(when) if !installing(when) => {
                    println!(
                        "  wait {} @ samples: {} (applies when {when} is installed)",
                        w.file, w.anchor
                    );
                }
                Some(when)
                    if !have.contains(when)
                        && order.iter().position(|o| o == when)
                            > order.iter().position(|o| o == name) =>
                {
                    deferred.push((name.clone(), w.clone()));
                }
                _ => apply("", w, &mut touched)?,
            }
            if w.file == ".env.example" {
                env = true;
            }
        }
        util::log_decision(
            app,
            &format!(
                "installed sample {} {} (cargo offrails add-part; procedure and checks: .offrails/template/parts/{}/README.md, CHECKS.md)",
                part.name, part.version, part.name
            ),
        )?;
        update_header(app, name)?;
    }
    for (owner, w) in later.iter().chain(deferred.iter()) {
        if opts.dry_run {
            println!("  wire {} @ samples: {} (for {owner})", w.file, w.anchor);
        } else {
            apply(owner, w, &mut touched)?;
        }
    }
    if opts.dry_run {
        return Ok(order);
    }
    // docs/routes.md follows ROUTES (offrails-check [routes]) without compiling the app.
    if crate::generate::routes_md::refresh(app)? {
        println!("  routes docs/routes.md (from ROUTES)");
    }
    if opts.fmt && util::have("cargo") {
        // Wires are inserted unformatted on purpose (FORMAT.md): rustfmt sorts `mod` lines.
        if util::run(app, "cargo", &["fmt", "--all"]).is_err() {
            eprintln!("note: `cargo fmt --all` failed; run it after fixing the error it printed");
        }
    }
    if !opts.quiet_next {
        println!("\ndone: added {}.", order.join(", "));
        println!("Next, in {}:", app.display());
        if migrations {
            println!(
                "  cargo run -q -- db migrate                 # the part's tables (prove rollback too: db rollback, db migrate)"
            );
        }
        if env {
            println!("  compare .env with .env.example             # the part added settings");
        }
        println!(
            "  the Definition of Done on SQLite and Postgres (.offrails/workflow.md#definition-of-done)"
        );
        for name in &order {
            println!(
                "  read .offrails/template/parts/{name}/README.md (\"After installing\", \"Adapting it\") and CHECKS.md"
            );
        }
    }
    Ok(order)
}

/// Keep the AGENTS.md header honest when a part changes what it describes.
fn update_header(app: &Path, part: &str) -> Result<()> {
    let path = app.join("AGENTS.md");
    if !path.is_file() {
        return Ok(());
    }
    let mut text = util::read_text(&path)?;
    let before = text.clone();
    let none = |v: Option<String>| v.as_deref().is_none_or(|v| v.starts_with("none"));
    match part {
        "mailer" if none(util::header_value(&text, "EMAIL")) => {
            util::set_header(
                &mut text,
                "EMAIL",
                "dev: local mailbox (/dev/mailbox); prod: not yet (logged only)",
            );
        }
        "auth-password" | "auth-magic-link" => {
            let value = util::header_value(&text, "ACCOUNTS").unwrap_or_default();
            let method = if part == "auth-password" {
                "password"
            } else {
                "magic-link"
            };
            let updated = accounts_with(&value, method);
            if updated != value {
                util::set_header(&mut text, "ACCOUNTS", &updated);
            }
        }
        _ => {}
    }
    if text != before {
        util::write_text(&path, &text)?;
        println!("  header AGENTS.md (updated for {part}; check it matches the interview)");
    }
    Ok(())
}

/// The `ACCOUNTS` header's own words (before `ROLES:`), in the grammar `<method>[+<method>…]
/// [invite]` (`password`, `magic-link`, `password+magic-link invite`), and the rest of the line.
/// The words before 0.7.3 map: `signup` = `password`, `invite` = `password invite`,
/// `magic-link-invite` = `magic-link invite`.
pub fn parse_accounts(value: &str) -> (Vec<String>, bool, String) {
    let (own, rest) = match value.find("ROLES:") {
        Some(i) => (&value[..i], value[i..].to_owned()),
        None => (value, String::new()),
    };
    let mut methods: Vec<String> = Vec::new();
    let mut invite = false;
    let add = |m: &str, methods: &mut Vec<String>| {
        if !m.is_empty() && !methods.iter().any(|x| x == m) {
            methods.push(m.to_owned());
        }
    };
    for word in own.split_whitespace() {
        match word {
            "none" => {}
            "signup" => add("password", &mut methods),
            "invite" => {
                if methods.is_empty() {
                    add("password", &mut methods);
                }
                invite = true;
            }
            "magic-link-invite" => {
                add("magic-link", &mut methods);
                invite = true;
            }
            other => {
                for m in other.split('+') {
                    add(m, &mut methods);
                }
            }
        }
    }
    (methods, invite, rest)
}

/// The `ACCOUNTS` value written back: the methods `+`-joined (`none` without one), ` invite`, then
/// two spaces and the rest of the line (`ROLES: … TENANCY: …`).
pub fn format_accounts(methods: &[String], invite: bool, rest: &str) -> String {
    let mut own = if methods.is_empty() {
        "none".to_owned()
    } else {
        methods.join("+")
    };
    if invite && !methods.is_empty() {
        own.push_str(" invite");
    }
    if rest.is_empty() {
        own
    } else {
        format!("{own}  {rest}")
    }
}

/// `ACCOUNTS` with this sign-in method among its methods (old words normalized).
fn accounts_with(value: &str, method: &str) -> String {
    let (mut methods, invite, rest) = parse_accounts(value);
    if !methods.iter().any(|m| m == method) {
        methods.push(method.to_owned());
    }
    format_accounts(&methods, invite, &rest)
}

/// `list-parts`: every part, what it adds, what it needs, and whether the app has it.
pub fn list(fw: &Path, app: Option<&Path>) -> Result<()> {
    let catalog = catalog(fw)?;
    let have = app.map(installed).unwrap_or_default();
    let width = catalog.keys().map(String::len).max().unwrap_or(4);
    if app.is_some() {
        println!(
            "{:<9} {:<width$}  {:<22}  what it adds",
            "state", "part", "needs"
        );
    } else {
        println!("{:<width$}  {:<22}  what it adds", "part", "needs");
    }
    for (name, p) in &catalog {
        let needs = if p.depends_on.is_empty() {
            "-".to_owned()
        } else {
            p.depends_on.join(", ")
        };
        let summary: String = if p.summary.chars().count() > 110 {
            format!("{}…", p.summary.chars().take(109).collect::<String>())
        } else {
            p.summary.clone()
        };
        if app.is_some() {
            let state = if have.contains(name) {
                "installed"
            } else {
                "available"
            };
            println!("{state:<9} {name:<width$}  {needs:<22}  {summary}");
        } else {
            println!("{name:<width$}  {needs:<22}  {summary}");
        }
    }
    println!(
        "\nAdd one (dependencies come first, re-running is a no-op): cargo offrails add-part <name>"
    );
    println!("Which ones an app needs: .offrails/template/README.md#choosing-parts");
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_accounts_header_grammar_and_its_old_words() {
        let rest = "ROLES: none  TENANCY: none";
        let cases = [
            (
                "none  ROLES: none  TENANCY: none",
                "password",
                "password  ROLES: none  TENANCY: none",
            ),
            (
                "password  ROLES: none  TENANCY: none",
                "magic-link",
                "password+magic-link  ROLES: none  TENANCY: none",
            ),
            (
                "magic-link invite  ROLES: none  TENANCY: none",
                "magic-link",
                "magic-link invite  ROLES: none  TENANCY: none",
            ),
            (
                "signup  ROLES: none  TENANCY: none",
                "password",
                "password  ROLES: none  TENANCY: none",
            ),
            (
                "invite  ROLES: none  TENANCY: none",
                "magic-link",
                "password+magic-link invite  ROLES: none  TENANCY: none",
            ),
            (
                "magic-link-invite  ROLES: none  TENANCY: none",
                "password",
                "magic-link+password invite  ROLES: none  TENANCY: none",
            ),
        ];
        for (before, method, after) in cases {
            assert_eq!(accounts_with(before, method), after, "{before} + {method}");
        }
        let (methods, invite, r) =
            parse_accounts("password+magic-link invite  ROLES: none  TENANCY: none");
        assert_eq!(
            (methods, invite, r.as_str()),
            (
                vec!["password".to_owned(), "magic-link".to_owned()],
                true,
                rest
            )
        );
        assert_eq!(
            format_accounts(&[], true, rest),
            "none  ROLES: none  TENANCY: none"
        );
    }
    use super::*;

    fn part(name: &str, deps: &[&str]) -> (String, Part) {
        (
            name.to_owned(),
            Part {
                name: name.into(),
                depends_on: deps.iter().map(|s| s.to_string()).collect(),
                ..Part::default()
            },
        )
    }

    #[test]
    fn a_diamond_installs_each_part_once_dependencies_first() {
        let c: BTreeMap<_, _> = [
            part("base", &[]),
            part("mid", &["base"]),
            part("top", &["base", "mid"]),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            resolve(&c, &["top".into()], &[], false).unwrap(),
            vec!["base", "mid", "top"]
        );
        assert_eq!(
            resolve(&c, &["top".into()], &["base".into()], false).unwrap(),
            vec!["mid", "top"]
        );
        assert!(
            resolve(
                &c,
                &["top".into()],
                &["top".into(), "mid".into(), "base".into()],
                false
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            resolve(&c, &["top".into()], &["top".into()], true).unwrap(),
            vec!["base", "mid", "top"]
        );
    }

    #[test]
    fn a_cycle_is_refused() {
        let c: BTreeMap<_, _> = [part("a", &["b"]), part("b", &["a"])].into_iter().collect();
        assert!(resolve(&c, &["a".into()], &[], false).is_err());
    }

    #[test]
    fn insert_at_indents_like_the_anchor_and_is_idempotent() {
        let file = "fn x() {\n    // samples: things\n}\n";
        let once = insert_at(file, "things", "a();\nb();\n").unwrap();
        assert_eq!(
            once,
            "fn x() {\n    a();\n    b();\n    // samples: things\n}\n"
        );
        assert!(insert_at(&once, "things", "a();\nb();\n").is_none());
        assert!(insert_at(file, "other", "a();\n").is_none());
    }

    #[test]
    fn an_anchor_rustfmt_pulled_onto_a_brace_is_healed() {
        let file = "    match x {\n        A => {\n            a()\n        } // samples: cli dispatch\n    }\n";
        let healed = heal_anchor(file, "cli dispatch").unwrap();
        assert_eq!(
            healed,
            "    match x {\n        A => {\n            a()\n        }\n        // samples: cli dispatch\n    }\n"
        );
        assert!(heal_anchor(&healed, "cli dispatch").is_none());
        // Code before the anchor that isn't only closers is never moved.
        assert!(heal_anchor("    pub mod x; // samples: kernel mods\n", "kernel mods").is_none());
    }

    #[test]
    fn insert_at_keeps_crlf() {
        let file = "a\r\n# samples: env\r\n";
        assert_eq!(
            insert_at(file, "env", "X=1\n").unwrap(),
            "a\r\nX=1\r\n# samples: env\r\n"
        );
    }

    #[test]
    fn anchors_match_whole_names_only() {
        assert!(is_anchor_line("    // samples: routes", "routes"));
        assert!(is_anchor_line("{# samples: nav links #}", "nav links"));
        assert!(!is_anchor_line("    // samples: routes table", "routes"));
        assert!(!anchor_stands_alone("pub mod x; // samples: kernel mods"));
    }

    #[test]
    fn parse_reads_the_samples_subset() {
        let dir = std::env::temp_dir().join(format!("offrails-gen-parse-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("sample.toml"),
            "name = \"x\"\nversion = \"1\"\nsummary = \"s\"\ndepends_on = [\"a\", \"b\"]\ncrates = [\n  \"lettre --features x,y\",\n]\n\n[[wire]]\nfile = \"src/lib.rs\"\nanchor = \"lib mods\"\nwhen = \"a\"\ntext = '''\npub mod x;\n'''\n",
        )
        .unwrap();
        let p = parse("x", &dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(p.depends_on, vec!["a", "b"]);
        assert_eq!(p.crates, vec!["lettre --features x,y"]);
        assert_eq!(p.wires.len(), 1);
        assert_eq!(p.wires[0].text, "pub mod x;\n");
        assert_eq!(p.wires[0].when.as_deref(), Some("a"));
    }
}
