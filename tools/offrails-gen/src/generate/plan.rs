//! A generator's plan: files to create, wiring to insert at anchors, crates to add. Rendered in
//! memory first (every anchor found, no file in the way), so a refusal changes nothing; then
//! written, formatted, and recorded in `docs/generated.toml` so `destroy` can undo exactly it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::app::anchor_hint;
use super::manifest::{Entry, Manifest};
use crate::parts;
use crate::util::{self, Fail, Result, rel};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// `samples: <name>` on a line of its own (generators/new.md#anchors).
    Named(String),
    /// Before the first line containing `pattern` (after the first line containing `after`):
    /// part files whose spots have no anchor (`src/jobs/mod.rs`'s match arms).
    Before {
        after: Option<String>,
        pattern: String,
    },
    /// After the last line that starts (indent aside) with `prefix`.
    AfterLast(String),
}

impl Anchor {
    pub fn describe(&self) -> String {
        match self {
            Anchor::Named(n) => format!("samples: {n}"),
            Anchor::Before { pattern, .. } => format!("before `{pattern}`"),
            Anchor::AfterLast(p) => format!("after the last `{p}…`"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Insert {
    pub file: String,
    pub anchor: Anchor,
    pub text: String,
    /// Wiring that belongs to a shared file (its `pub mod` line): removed with that file only.
    pub owner: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Action {
    /// A file this generator owns: destroy removes it when unchanged.
    Create {
        path: String,
        content: String,
    },
    /// A file several generators add to (`controllers/pages.rs`): created when missing, removed
    /// when the last of them is destroyed and nothing else was added.
    Shared {
        path: String,
        content: String,
    },
    Insert(Insert),
    /// `cargo add <args>`, unless Cargo.toml already has `present`. Never undone.
    Crate {
        args: String,
        present: String,
    },
    /// A file the app writes when it runs (docs/openapi.json from its test), removed with the
    /// shared file `owner`.
    Byproduct {
        owner: String,
        path: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub kind: String,
    pub name: String,
    /// What was typed after the kind, for the record and for re-planning.
    pub args: Vec<String>,
    /// Decisions resolved while planning (scope, migration timestamp), so `destroy` re-plans
    /// exactly what was generated.
    pub options: BTreeMap<String, String>,
    pub actions: Vec<Action>,
    /// What to do next, printed after generating.
    pub next: Vec<String>,
    pub migrations: bool,
    /// The marker a shared file's wiring carries (`// api` for every line the API kernel
    /// adds), by shared-file path; default: the file's name.
    pub shared_tags: BTreeMap<String, String>,
    /// The marker this generation's wiring carries, when it isn't its name (a snippet added
    /// `--as article` marks `// articles`).
    pub tag: Option<String>,
}

impl Plan {
    pub fn new(kind: &str, name: &str, args: &[String]) -> Self {
        Plan {
            kind: kind.to_owned(),
            name: name.to_owned(),
            args: args.to_vec(),
            ..Plan::default()
        }
    }
    pub fn create(&mut self, path: impl Into<String>, content: impl Into<String>) {
        self.actions.push(Action::Create {
            path: path.into(),
            content: content.into(),
        });
    }
    pub fn shared(&mut self, path: impl Into<String>, content: impl Into<String>) {
        self.actions.push(Action::Shared {
            path: path.into(),
            content: content.into(),
        });
    }
    pub fn insert(&mut self, file: &str, anchor: &str, text: impl Into<String>) {
        self.actions.push(Action::Insert(Insert {
            file: file.to_owned(),
            anchor: Anchor::Named(anchor.to_owned()),
            text: text.into(),
            owner: None,
        }));
    }
    pub fn insert_at(&mut self, file: &str, anchor: Anchor, text: impl Into<String>) {
        self.actions.push(Action::Insert(Insert {
            file: file.to_owned(),
            anchor,
            text: text.into(),
            owner: None,
        }));
    }
    /// Wiring for a shared file (removed only together with it).
    pub fn insert_for(&mut self, owner: &str, file: &str, anchor: &str, text: impl Into<String>) {
        self.actions.push(Action::Insert(Insert {
            file: file.to_owned(),
            anchor: Anchor::Named(anchor.to_owned()),
            text: text.into(),
            owner: Some(owner.to_owned()),
        }));
    }
    pub fn cargo(&mut self, args: &str, present: &str) {
        self.actions.push(Action::Crate {
            args: args.to_owned(),
            present: present.to_owned(),
        });
    }
    pub fn byproduct(&mut self, owner: &str, path: &str) {
        self.actions.push(Action::Byproduct {
            owner: owner.to_owned(),
            path: path.to_owned(),
        });
    }
    pub fn option(&mut self, key: &str, value: impl Into<String>) {
        self.options.insert(key.to_owned(), value.into());
    }
    pub fn created(&self) -> Vec<&str> {
        self.actions
            .iter()
            .filter_map(|a| match a {
                Action::Create { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect()
    }
    pub fn shared_paths(&self) -> Vec<&str> {
        self.actions
            .iter()
            .filter_map(|a| match a {
                Action::Shared { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect()
    }
    /// Marks every wiring line with its owner (`pub mod posts; // posts`), so a human, or
    /// `destroy`, finds what a feature put into a shared file. `tag`: this generation's name.
    pub fn mark_wiring(&mut self, tag: &str) {
        let shared_tags = self.shared_tags.clone();
        for a in &mut self.actions {
            if let Action::Insert(i) = a {
                let t = match &i.owner {
                    Some(o) => shared_tags.get(o).cloned().unwrap_or_else(|| owner_tag(o)),
                    None => tag.to_owned(),
                };
                i.text = mark(&i.file, &i.text, &t);
            }
        }
    }
    pub fn command(&self) -> String {
        if self.kind == "snippet" {
            return match self.args.as_slice() {
                [name, model, ..] => format!("cargo offrails snippet add {name} --as {model}"),
                [name] => format!("cargo offrails snippet add {name}"),
                [] => "cargo offrails snippet add".to_owned(),
            };
        }
        format!(
            "cargo offrails generate {} {}",
            self.kind,
            self.args.join(" ")
        )
        .trim_end()
        .to_owned()
    }
}

/// The default marker of a shared file's wiring: its name (`tests/app/models/mod.rs` → `models`).
pub fn owner_tag(path: &str) -> String {
    let p = Path::new(path);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if stem == "mod" {
        p.parent()
            .and_then(|d| d.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(stem)
    } else {
        stem.trim_end_matches("_test").to_owned()
    }
}

/// The comment that names a wiring line's owner, in `file`'s language.
pub fn marker(file: &str, tag: &str) -> String {
    if file.ends_with(".html") {
        format!("{{# {tag} #}}")
    } else if file.ends_with(".rs") || file.ends_with(".js") || file.ends_with(".css") {
        format!("// {tag}")
    } else {
        format!("# {tag}")
    }
}

/// `text` with its owner's marker. One statement or declaration (`pub mod posts;`, `app = …;`,
/// a nav link): the marker ends its line. A list element, field or match arm (it ends with `,`),
/// or several lines: the marker is a comment line above it, because rustfmt aligns a comment
/// that follows a trailing one in a list. Already marked: unchanged.
pub fn mark(file: &str, text: &str, tag: &str) -> String {
    let m = marker(file, tag);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let code: Vec<usize> = (0..lines.len())
        .filter(|i| !lines[*i].trim().is_empty())
        .collect();
    let Some(&first) = code.first() else {
        return text.to_owned();
    };
    if lines
        .iter()
        .any(|l| l.trim_end().ends_with(m.as_str()) || l.trim() == m)
    {
        return text.to_owned();
    }
    let trailing = code.len() == 1 && !lines[first].trim_end().ends_with(',');
    let mut out = String::with_capacity(text.len() + m.len() + 2);
    for (i, l) in lines.iter().enumerate() {
        if i == first {
            if trailing {
                let body = l.trim_end_matches(['\n', '\r']);
                out.push_str(&format!("{body} {m}\n"));
                continue;
            }
            let indent: String = l.chars().take_while(|c| *c == ' ').collect();
            out.push_str(&format!("{indent}{m}\n"));
        }
        out.push_str(l);
    }
    out
}

/// A `mod x;` / `pub mod x;` / `use …;` / `pub use …;` line: (kind, the name rustfmt sorts by).
fn decl_key(line: &str) -> Option<(&'static str, String)> {
    let code = line.split("//").next().unwrap_or("").trim();
    let code = code.strip_suffix(';')?;
    let code = code
        .strip_prefix("pub(crate) ")
        .or_else(|| code.strip_prefix("pub "))
        .unwrap_or(code);
    if let Some(name) = code.strip_prefix("mod ") {
        return Some(("mod", name.trim().to_owned()));
    }
    if let Some(path) = code.strip_prefix("use ") {
        return Some(("use", path.trim().to_owned()));
    }
    None
}

/// rustfmt's version sort, closely enough for generated names: digit runs compare as numbers,
/// everything else byte by byte (`api` < `api2` < `api_v1_posts` < `apix`).
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (x, y) = (
                std::str::from_utf8(&a[si..i])
                    .unwrap_or("0")
                    .trim_start_matches('0'),
                std::str::from_utf8(&b[sj..j])
                    .unwrap_or("0")
                    .trim_start_matches('0'),
            );
            let o = x.len().cmp(&y.len()).then(x.cmp(y));
            if o != std::cmp::Ordering::Equal {
                return o;
            }
            continue;
        }
        if a[i] != b[j] {
            return a[i].cmp(&b[j]);
        }
        i += 1;
        j += 1;
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Where a one-line declaration goes above `anchor_at`: before the first line of the run of the
/// same declarations right above the anchor that sorts after it, so the list stays sorted and
/// rustfmt never moves a marked line (it would move the marker onto another line).
fn sorted_spot(lines: &[&str], anchor_at: usize, text: &str) -> usize {
    let code: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let [one] = code.as_slice() else {
        return anchor_at;
    };
    let Some((kind, key)) = decl_key(one) else {
        return anchor_at;
    };
    let mut start = anchor_at;
    while start > 0 && decl_key(lines[start - 1]).is_some_and(|(k, _)| k == kind) {
        start -= 1;
    }
    (start..anchor_at)
        .find(|i| {
            decl_key(lines[*i])
                .is_some_and(|(_, k)| version_cmp(&k, &key) == std::cmp::Ordering::Greater)
        })
        .unwrap_or(anchor_at)
}

/// Code compared for "is it still what we wrote": whitespace, commas and braces dropped, so
/// rustfmt's rewrapping (and its trailing commas and arm blocks) doesn't count as a change.
pub fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, ',' | '{' | '}'))
        .collect()
}

/// A shared file is back to what the generator first wrote, rustfmt's reordering of imports and
/// rewrapping aside: the same code characters, in any order.
pub fn same_content(a: &str, b: &str) -> bool {
    let sorted = |t: &str| {
        let mut v: Vec<char> = normalize(t).chars().collect();
        v.sort_unstable();
        v
    };
    sorted(a) == sorted(b)
}

/// FNV-1a 64 of the text with CRLF as LF: the record of a file as written (and formatted).
pub fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.replace("\r\n", "\n").bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The lines `[start, end)` of `file` that hold `text` (compared normalized), if any.
pub fn find_block(file: &str, text: &str) -> Option<(usize, usize)> {
    let want = normalize(text);
    if want.is_empty() {
        return None;
    }
    // Closing lines (`}`, `},`) normalize to nothing: after a match, the block also takes the
    // ones that close braces it opened (its own, or the arm block rustfmt wrapped it in).
    let closers = |l: &str| !l.trim().is_empty() && normalize(l).is_empty();
    let balance = |ls: &[&str]| -> i64 {
        ls.iter()
            .map(|l| l.matches('{').count() as i64 - l.matches('}').count() as i64)
            .sum()
    };
    let lines: Vec<&str> = file.split_inclusive('\n').collect();
    for start in 0..lines.len() {
        let first = normalize(lines[start]);
        if first.is_empty() || !want.starts_with(&first) {
            continue;
        }
        let mut have = String::new();
        for (end, line) in lines.iter().enumerate().skip(start) {
            have.push_str(&normalize(line));
            if have.len() >= want.len() {
                if have == want {
                    let mut stop = end + 1;
                    let mut open = balance(&lines[start..stop]);
                    while open > 0 && stop < lines.len() && closers(lines[stop]) {
                        open += balance(&lines[stop..stop + 1]);
                        stop += 1;
                    }
                    return Some((start, stop));
                }
                break;
            }
            if !want.starts_with(&have) {
                break;
            }
        }
    }
    None
}

/// `file` without `text`'s block, and the blank lines it was inserted with. `None`: not there.
pub fn remove_block(file: &str, text: &str) -> Option<String> {
    let (mut start, mut end) = find_block(file, text)?;
    let lines: Vec<&str> = file.split_inclusive('\n').collect();
    let blank = |l: &str| l.trim().is_empty();
    let lead = text.lines().take_while(|l| blank(l)).count();
    let trail = text
        .lines()
        .rev()
        .take_while(|l| blank(l))
        .count()
        .min(text.lines().count().saturating_sub(lead));
    for _ in 0..lead {
        if start > 0 && blank(lines[start - 1]) {
            start -= 1;
        }
    }
    for _ in 0..trail {
        if end < lines.len() && blank(lines[end]) {
            end += 1;
        }
    }
    let mut out = String::with_capacity(file.len());
    for (i, l) in lines.iter().enumerate() {
        if i < start || i >= end {
            out.push_str(l);
        }
    }
    Some(out)
}

/// `migration/src/lib.rs` after `sea-orm-cli migrate generate` rewrote it without the anchors:
/// both come back, after the last `mod m…;` line and the last `Box::new(…)` line.
pub fn heal_migration_lib(text: &str) -> Option<String> {
    let has = |a: &str| text.lines().any(|l| l.trim() == format!("// samples: {a}"));
    if has("migration mods") && has("migrations") {
        return None;
    }
    let nl = util::newline_of(text);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let last_mod = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("mod m") && l.trim_end().ends_with(';'));
    let last_box = lines.iter().rposition(|l| l.contains("Box::new(m"));
    let mut out = String::with_capacity(text.len() + 64);
    for (i, l) in lines.iter().enumerate() {
        out.push_str(l);
        if !l.ends_with('\n') {
            out.push_str(nl);
        }
        if Some(i) == last_mod && !has("migration mods") {
            out.push_str(&format!("// samples: migration mods{nl}"));
        }
        if Some(i) == last_box && !has("migrations") {
            let indent: String = l.chars().take_while(|c| c.is_whitespace()).collect();
            out.push_str(&format!("{indent}// samples: migrations{nl}"));
        }
    }
    (out != text).then_some(out)
}

/// Inserts `text` at `anchor` in `file`. `Ok(None)`: it's already there.
fn insert_text(file: &str, anchor: &Anchor, text: &str) -> std::result::Result<Option<String>, ()> {
    if find_block(file, text).is_some() {
        return Ok(None);
    }
    match anchor {
        Anchor::Named(name) => {
            let lines: Vec<&str> = file.split_inclusive('\n').collect();
            if let Some(at) = lines.iter().position(|l| parts::is_anchor_line(l, name)) {
                let spot = sorted_spot(&lines, at, text);
                if spot < at {
                    return Ok(Some(splice(file, &lines, spot, text)));
                }
            }
            match parts::insert_at(file, name, text) {
                Some(out) => Ok(Some(out)),
                None => {
                    let present = file.lines().any(|l| parts::is_anchor_line(l, name));
                    if present { Ok(None) } else { Err(()) }
                }
            }
        }
        Anchor::Before { after, pattern } => {
            let lines: Vec<&str> = file.split_inclusive('\n').collect();
            let from = match after {
                Some(a) => lines
                    .iter()
                    .position(|l| l.contains(a.as_str()))
                    .ok_or(())?,
                None => 0,
            };
            let at = lines
                .iter()
                .enumerate()
                .skip(from)
                .find(|(_, l)| l.contains(pattern.as_str()))
                .map(|(i, _)| i)
                .ok_or(())?;
            Ok(Some(splice(file, &lines, at, text)))
        }
        Anchor::AfterLast(prefix) => {
            let lines: Vec<&str> = file.split_inclusive('\n').collect();
            let at = lines
                .iter()
                .rposition(|l| l.trim_start().starts_with(prefix.as_str()))
                .ok_or(())?;
            // A declaration goes in sorted among the ones it follows (see `sorted_spot`).
            let spot = sorted_spot(&lines, at + 1, text);
            if spot <= at {
                return Ok(Some(splice(file, &lines, spot, text)));
            }
            let indent: String = lines[at].chars().take_while(|c| *c == ' ').collect();
            let nl = util::newline_of(file);
            let mut out = String::with_capacity(file.len() + text.len());
            for (i, l) in lines.iter().enumerate() {
                out.push_str(l);
                if i == at {
                    if !l.ends_with('\n') {
                        out.push_str(nl);
                    }
                    for t in text.lines() {
                        out.push_str(&format!("{indent}{t}{nl}"));
                    }
                }
            }
            Ok(Some(out))
        }
    }
}

/// `text` inserted above line `at`, indented like it.
fn splice(file: &str, lines: &[&str], at: usize, text: &str) -> String {
    let nl = util::newline_of(file);
    let indent: String = lines[at]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let mut out = String::with_capacity(file.len() + text.len());
    for (i, l) in lines.iter().enumerate() {
        if i == at {
            for t in text.lines() {
                if t.trim().is_empty() {
                    out.push_str(nl);
                } else {
                    out.push_str(&format!("{indent}{t}{nl}"));
                }
            }
        }
        out.push_str(l);
    }
    out
}

/// The app's files as the plan leaves them, in memory: path → (before, after).
pub struct Rendered {
    pub files: BTreeMap<String, (Option<String>, String)>,
    pub crates: Vec<String>,
    pub notes: Vec<String>,
}

/// Renders `plan` onto the app. `owned`: paths the plan may overwrite (files of the same
/// generation, unchanged, when re-generating with `--force`).
pub fn render(
    app: &Path,
    plan: &Plan,
    owned: &BTreeSet<String>,
) -> std::result::Result<Rendered, Vec<String>> {
    let mut files: BTreeMap<String, (Option<String>, String)> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut crates = Vec::new();
    let mut notes = Vec::new();
    let cargo_toml = fs::read_to_string(app.join("Cargo.toml")).unwrap_or_default();
    let load =
        |files: &mut BTreeMap<String, (Option<String>, String)>, path: &str| -> Option<String> {
            if let Some((_, now)) = files.get(path) {
                return Some(now.clone());
            }
            let text = fs::read_to_string(rel(app, path)).ok()?;
            files.insert(path.to_owned(), (Some(text.clone()), text.clone()));
            Some(text)
        };
    for action in &plan.actions {
        match action {
            Action::Crate { args, present } => {
                if !cargo_toml.contains(present.as_str()) {
                    crates.push(args.clone());
                }
            }
            Action::Byproduct { .. } => {}
            Action::Create { path, content } => {
                let dest = rel(app, path);
                if dest.exists() && !owned.contains(path) {
                    problems.push(format!(
                        "{path} already exists and this generator didn't write it (or it changed since): \
                         pick another name, or move that file away first"
                    ));
                    continue;
                }
                let before = fs::read_to_string(&dest).ok();
                files.insert(path.clone(), (before, content.clone()));
            }
            Action::Shared { path, content } => {
                if load(&mut files, path).is_none() {
                    files.insert(path.clone(), (None, content.clone()));
                }
            }
            Action::Insert(ins) => {
                let Some(mut text) = load(&mut files, &ins.file) else {
                    problems.push(format!(
                        "{} doesn't exist, so its wiring can't go in ({})",
                        ins.file,
                        ins.anchor.describe()
                    ));
                    continue;
                };
                if ins.file == "migration/src/lib.rs"
                    && let Some(healed) = heal_migration_lib(&text)
                {
                    notes.push(
                        "fix  migration/src/lib.rs: put back the `samples: migration mods` / `samples: migrations` anchors (sea-orm-cli had removed them)".into(),
                    );
                    text = healed;
                }
                if let Anchor::Named(name) = &ins.anchor
                    && let Some(healed) = parts::heal_anchor(&text, name)
                {
                    notes.push(format!(
                        "fix  {}: the 'samples: {name}' anchor back on a line of its own (rustfmt had moved it)",
                        ins.file
                    ));
                    text = healed;
                }
                match insert_text(&text, &ins.anchor, &ins.text) {
                    Ok(Some(new)) => text = new,
                    Ok(None) => {}
                    Err(()) => {
                        let hint = match &ins.anchor {
                            Anchor::Named(n) => format!(
                                "add `// samples: {n}` (`{{# samples: {n} #}}` in a template) on a line of its own in {}",
                                anchor_hint(n)
                            ),
                            other => format!(
                                "the line it goes {} isn't in {} any more; add the wiring by hand:\n{}",
                                other.describe(),
                                ins.file,
                                indent_block(&ins.text)
                            ),
                        };
                        problems.push(format!(
                            "{} has no {} anchor: {hint}",
                            ins.file,
                            ins.anchor.describe()
                        ));
                        continue;
                    }
                }
                if let Some(entry) = files.get_mut(&ins.file) {
                    entry.1 = text;
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(Rendered {
            files,
            crates,
            notes,
        })
    } else {
        Err(problems)
    }
}

fn indent_block(text: &str) -> String {
    text.lines()
        .map(|l| format!("         {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What `destroy` would change: path → new content (`None`: delete), and what it keeps.
pub struct Undo {
    pub files: BTreeMap<String, Option<String>>,
    pub kept: Vec<String>,
}

/// Undoes `plan` (re-planned from `entry`): removes its unchanged files and its wiring.
/// `others`: shared files other generations still use.
/// `foreign`: wiring other generations put into files (a `has_many` in a parent model): it doesn't
/// count as a change to the file.
pub fn undo(
    app: &Path,
    plan: &Plan,
    entry: &Entry,
    others: &BTreeSet<String>,
    foreign: &[Insert],
) -> Undo {
    let mut files: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut kept = Vec::new();
    let current = |files: &BTreeMap<String, Option<String>>, path: &str| -> Option<String> {
        match files.get(path) {
            Some(v) => v.clone(),
            None => fs::read_to_string(rel(app, path)).ok(),
        }
    };
    let created: BTreeSet<&str> = plan.created().into_iter().collect();
    let remove = |files: &mut BTreeMap<String, Option<String>>,
                  kept: &mut Vec<String>,
                  ins: &Insert| {
        if created.contains(ins.file.as_str()) {
            return; // the whole file goes
        }
        let Some(text) = current(files, &ins.file) else {
            return;
        };
        match remove_block(&text, &ins.text) {
            Some(new) => {
                files.insert(ins.file.clone(), Some(new));
            }
            None => {
                let first = ins
                    .text
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                kept.push(format!(
                    "{} ({}): `{}` isn't there as generated (edited?); remove it by hand if it's still wanted gone",
                    ins.file,
                    ins.anchor.describe(),
                    first.trim()
                ));
            }
        }
    };
    for action in plan.actions.iter().rev() {
        if let Action::Insert(ins) = action
            && ins.owner.is_none()
        {
            remove(&mut files, &mut kept, ins);
        }
    }
    for action in &plan.actions {
        match action {
            Action::Create { path, content } => {
                let Some(text) = current(&files, path) else {
                    continue;
                };
                let recorded = entry.files.get(path);
                let mut own = text.clone();
                for f in foreign.iter().filter(|f| f.file == *path) {
                    while let Some(without) = remove_block(&own, &f.text) {
                        own = without;
                    }
                }
                // The hash of the file as formatted; or, formatted later (`--no-fmt`) or wired into
                // by other generators since, the same code.
                if recorded.is_some_and(|h| *h == hash(&text)) || same_content(&own, content) {
                    files.insert(path.clone(), None);
                } else {
                    kept.push(format!(
                        "{path}: changed since it was generated, so it stays"
                    ));
                }
            }
            Action::Shared { path, content } => {
                if others.contains(path) {
                    continue;
                }
                let Some(text) = current(&files, path) else {
                    continue;
                };
                if same_content(&text, content) {
                    files.insert(path.clone(), None);
                    for a in plan.actions.iter().rev() {
                        if let Action::Insert(ins) = a
                            && ins.owner.as_deref() == Some(path.as_str())
                        {
                            remove(&mut files, &mut kept, ins);
                        }
                        if let Action::Byproduct { owner, path: made } = a
                            && owner == path
                            && rel(app, made).is_file()
                        {
                            files.insert(made.clone(), None);
                        }
                    }
                } else {
                    if std::env::var_os("OFFRAILS_GEN_DEBUG").is_some() {
                        eprintln!(
                            "--- {path} after unwiring:\n{text}\n--- as first generated:\n{content}"
                        );
                    }
                    kept.push(format!(
                        "{path}: shared, and it holds more than this generator put there, so it stays"
                    ));
                }
            }
            _ => {}
        }
    }
    Undo { files, kept }
}

/// Writes rendered files (creating directories), keeping each file's line endings.
pub fn write(app: &Path, files: &BTreeMap<String, (Option<String>, String)>) -> Result<()> {
    for (path, (before, after)) in files {
        if before.as_deref() == Some(after.as_str()) {
            continue;
        }
        util::write_text(&rel(app, path), after)?;
    }
    Ok(())
}

/// Applies an `Undo`: deletes files (and directories left empty), writes the rest.
pub fn write_undo(app: &Path, undo: &Undo) -> Result<()> {
    for (path, content) in &undo.files {
        let dest = rel(app, path);
        match content {
            Some(text) => util::write_text(&dest, text)?,
            None => {
                if dest.exists() {
                    util::ctx(fs::remove_file(&dest), "removing", &dest)?;
                }
                let mut dir = dest.parent().map(Path::to_path_buf);
                while let Some(d) = dir {
                    if d == app || fs::remove_dir(&d).is_err() {
                        break;
                    }
                    dir = d.parent().map(Path::to_path_buf);
                }
            }
        }
    }
    Ok(())
}

/// Problems collected while planning become one refusal, nothing changed.
pub fn refuse(problems: &[String]) -> Fail {
    for p in problems {
        eprintln!("refuse {p}");
    }
    Fail::new(format!("nothing changed ({} problem(s))", problems.len()))
}

/// Paths this plan creates that the manifest says belong to another generation.
pub fn claimed_elsewhere(plan: &Plan, manifest: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    for path in plan.created() {
        for e in &manifest.entries {
            if (e.kind != plan.kind || e.name != plan.name) && e.files.contains_key(path) {
                out.push(format!(
                    "{path} belongs to `generate {} {}`: destroy that first, or pick another name",
                    e.kind, e.name
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_found_and_removed_whatever_rustfmt_did_to_it() {
        let file = "fn router() {\n    let x = 1;\n    app = app\n        .route(\"/posts\", get(posts::index).post(posts::create))\n        .route(\n            \"/posts/{id}\",\n            get(posts::show),\n        );\n\n    // samples: routes\n}\n";
        let text = "app = app\n    .route(\"/posts\", get(posts::index).post(posts::create))\n    .route(\"/posts/{id}\", get(posts::show));\n\n";
        let out = remove_block(file, text).unwrap();
        assert_eq!(
            out,
            "fn router() {\n    let x = 1;\n    // samples: routes\n}\n"
        );
        assert!(remove_block(&out, text).is_none());
    }

    #[test]
    fn inserting_twice_changes_nothing_and_a_missing_anchor_is_an_error() {
        let file = "a\n// samples: things\n";
        let once = insert_text(file, &Anchor::Named("things".into()), "x();\n")
            .unwrap()
            .unwrap();
        assert_eq!(once, "a\nx();\n// samples: things\n");
        assert!(
            insert_text(&once, &Anchor::Named("things".into()), "x();\n")
                .unwrap()
                .is_none()
        );
        assert!(insert_text(file, &Anchor::Named("other".into()), "y();\n").is_err());
        let before = Anchor::Before {
            after: Some("fn dispatch".into()),
            pattern: "other =>".into(),
        };
        let f =
            "fn dispatch() {\n    match w {\n        A => a(),\n        other => b(),\n    }\n}\n";
        assert_eq!(
            insert_text(f, &before, "B => c(),\n").unwrap().unwrap(),
            "fn dispatch() {\n    match w {\n        A => a(),\n        B => c(),\n        other => b(),\n    }\n}\n"
        );
        let sorted = insert_text(
            "pub mod a;\npub mod c;\n\nuse x;\n",
            &Anchor::AfterLast("pub mod ".into()),
            "pub mod b; // b\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            sorted,
            "pub mod a;\npub mod b; // b\npub mod c;\n\nuse x;\n"
        );
        let after = Anchor::AfterLast("pub mod ".into());
        assert_eq!(
            insert_text("pub mod a;\npub mod b;\n\nuse x;\n", &after, "pub mod c;\n")
                .unwrap()
                .unwrap(),
            "pub mod a;\npub mod b;\npub mod c;\n\nuse x;\n"
        );
    }

    #[test]
    fn migration_anchors_come_back_after_sea_orm_cli() {
        let lib = "pub use sea_orm_migration::prelude::*;\n\nmod m1_a;\nmod m2_b;\n\npub struct Migrator;\n\nimpl MigratorTrait for Migrator {\n    fn migrations() -> Vec<Box<dyn MigrationTrait>> {\n        vec![\n            Box::new(m1_a::Migration),\n            Box::new(m2_b::Migration),\n        ]\n    }\n}\n";
        let healed = heal_migration_lib(lib).unwrap();
        assert!(healed.contains("mod m2_b;\n// samples: migration mods\n"));
        assert!(
            healed.contains("Box::new(m2_b::Migration),\n            // samples: migrations\n")
        );
        assert!(heal_migration_lib(&healed).is_none());
    }

    #[test]
    fn undo_removes_unchanged_files_and_wiring_and_keeps_edited_ones() {
        let dir = std::env::temp_dir().join(format!("offrails-gen-undo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.join("src/b.rs"), "fn b() {}\n// edited\n").unwrap();
        fs::write(
            dir.join("src/mod.rs"),
            "pub mod a;\npub mod b;\n// samples: mods\n",
        )
        .unwrap();
        let mut plan = Plan::new("x", "y", &[]);
        plan.create("src/a.rs", "fn a() {}\n");
        plan.create("src/b.rs", "fn b() {}\n");
        plan.insert("src/mod.rs", "mods", "pub mod a;\n");
        let mut entry = Entry::default();
        entry.files.insert("src/a.rs".into(), hash("fn a() {}\n"));
        entry.files.insert("src/b.rs".into(), hash("fn b() {}\n"));
        let u = undo(&dir, &plan, &entry, &BTreeSet::new(), &[]);
        assert_eq!(u.files.get("src/a.rs"), Some(&None));
        assert!(!u.files.contains_key("src/b.rs"));
        assert!(u.kept.iter().any(|k| k.starts_with("src/b.rs")));
        assert_eq!(
            u.files.get("src/mod.rs"),
            Some(&Some("pub mod b;\n// samples: mods\n".to_owned()))
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wiring_carries_its_owners_marker() {
        assert_eq!(
            mark("src/a.rs", "pub mod posts;\n", "posts"),
            "pub mod posts; // posts\n"
        );
        assert_eq!(
            mark("templates/shared/_nav.html", "<a href=\"/x\">X</a>\n", "x"),
            "<a href=\"/x\">X</a> {# x #}\n"
        );
        assert_eq!(
            mark("src/a.rs", "\nfn a() {}\nfn b() {}\n\n", "posts"),
            "\n// posts\nfn a() {}\nfn b() {}\n\n"
        );
        assert_eq!(
            mark("src/a.rs", "posts::ROUTES,\n", "posts"),
            "// posts\nposts::ROUTES,\n"
        );
        let once = mark("src/a.rs", "pub mod posts;\n", "posts");
        assert_eq!(mark("src/a.rs", &once, "posts"), once);
        assert_eq!(owner_tag("tests/app/models/mod.rs"), "models");
        assert_eq!(owner_tag("tests/app/pages_test.rs"), "pages");
        assert_eq!(owner_tag("src/controllers/api.rs"), "api");
    }

    #[test]
    fn declarations_go_in_sorted_so_rustfmt_never_moves_a_marker() {
        let file = "pub mod about; // about\npub mod books; // books\n// samples: route mods\n";
        let out = insert_text(
            file,
            &Anchor::Named("route mods".into()),
            "pub mod blog; // blog\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            out,
            "pub mod about; // about\npub mod blog; // blog\npub mod books; // books\n// samples: route mods\n"
        );
        let last = insert_text(
            file,
            &Anchor::Named("route mods".into()),
            "pub mod zz; // zz\n",
        )
        .unwrap()
        .unwrap();
        assert!(last.ends_with("pub mod zz; // zz\n// samples: route mods\n"));
        assert_eq!(version_cmp("api", "api2"), std::cmp::Ordering::Less);
        assert_eq!(
            version_cmp("api2", "api_v1_posts"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            version_cmp("api_v1_posts", "apix"),
            std::cmp::Ordering::Less
        );
        assert_eq!(version_cmp("m2", "m10"), std::cmp::Ordering::Less);
    }

    #[test]
    fn hashes_ignore_line_endings() {
        assert_eq!(hash("a\r\nb\r\n"), hash("a\nb\n"));
        assert_ne!(hash("a"), hash("b"));
    }
}
