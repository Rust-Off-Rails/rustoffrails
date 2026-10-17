//! `cargo offrails update-part <part>… --framework <newer>`: brings an installed part up to the
//! newer framework's version without losing the app's changes. A three-way merge per file: the
//! base is the part as the app's vendored framework (`.offrails/`) ships it, the version it was
//! installed from; "theirs" is the newer framework's; "ours" is the app's file. Clean hunks
//! apply; overlapping edits are written with conflict markers and reported (exit 1); a file the
//! app changed is never overwritten. Wiring follows: changed wire texts are swapped where the old
//! text still stands, new wires are inserted at their anchors, removed ones taken out.
//! Run it before re-vendoring (`vendor` / the upgrade recipe), so `.offrails/` is still the base.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::generate::plan::{find_block, remove_block};
use crate::merge::merge3;
use crate::parts::{self, Part, Wire};
use crate::util::{self, Fail, Result, rel};

pub struct Options {
    pub dry_run: bool,
    pub fmt: bool,
}

/// What happened to one file or wire.
enum Outcome {
    Same,
    Kept,
    Took,
    Merged,
    Conflict(usize),
    Created,
    Deleted,
    Note(String),
}

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|t| t.replace("\r\n", "\n"))
}

pub fn run(
    base_fw: &Path,
    new_fw: &Path,
    app: &Path,
    names: &[String],
    opts: &Options,
) -> Result<()> {
    let krate = util::package_name(app)?;
    let have = parts::installed(app);
    let old_catalog = parts::catalog(base_fw)?;
    let new_catalog = parts::catalog(new_fw)?;
    let mut total_conflicts = 0;
    let mut any = false;
    for name in names {
        if !have.contains(name) {
            return Err(Fail::usage(format!(
                "{name} isn't installed (docs/decisions.md has no `installed sample {name}`): cargo offrails add {name}"
            )));
        }
        let (Some(old), Some(new)) = (old_catalog.get(name), new_catalog.get(name)) else {
            return Err(Fail::usage(format!(
                "no part '{name}' in {} or {} (cargo offrails parts)",
                base_fw.display(),
                new_fw.display()
            )));
        };
        let recorded = installed_version(app, name);
        if recorded.as_deref().is_some_and(|v| v != old.version) {
            eprintln!(
                "warn   {name}: installed as {}, but the base (.offrails/) has {}; the merge treats {} as what was installed",
                recorded.unwrap_or_default(),
                old.version,
                old.version
            );
        }
        println!(
            "{} {name} {} -> {}{}",
            if opts.dry_run {
                "plan to update"
            } else {
                "update"
            },
            old.version,
            new.version,
            if opts.dry_run {
                " (--dry-run: nothing written)"
            } else {
                ""
            }
        );
        let (conflicts, changed) = update_one(app, &krate, old, new, opts)?;
        total_conflicts += conflicts;
        any |= changed;
        if changed && !opts.dry_run {
            util::log_decision(
                app,
                &format!(
                    "updated sample {name} {} -> {} (cargo offrails update-part; {conflicts} conflict(s))",
                    old.version, new.version
                ),
            )?;
            // The record add-part and the samples' install.sh read.
            util::log_decision(
                app,
                &format!(
                    "installed sample {name} {} (cargo offrails update-part; procedure and checks: .offrails/template/parts/{name}/README.md, CHECKS.md)",
                    new.version
                ),
            )?;
        }
    }
    if opts.dry_run {
        return Ok(());
    }
    if any && opts.fmt && total_conflicts == 0 && util::have("cargo") {
        let _ = util::run(app, "cargo", &["fmt", "--all"]);
    }
    let _ = crate::generate::routes_md::refresh(app)?;
    if total_conflicts > 0 {
        return Err(Fail::new(format!(
            "{total_conflicts} conflict(s): resolve the <<<<<<< ||||||| ======= >>>>>>> markers in the files listed \
             (yours, the installed part, the new part), then cargo fmt --all and the Definition of Done"
        )));
    }
    println!(
        "\ndone. Next: re-vendor the newer framework (.offrails/generators/upgrade.md), cargo run -q -- db migrate, then the Definition of Done on SQLite and Postgres; read the part's CHECKS.md"
    );
    Ok(())
}

fn installed_version(app: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(rel(app, "docs/decisions.md")).ok()?;
    text.lines()
        .rev()
        .filter_map(|l| l.split_once(": installed sample ").map(|(_, r)| r))
        .find_map(|r| {
            let mut w = r.split_whitespace();
            (w.next() == Some(name))
                .then(|| w.next().map(str::to_owned))
                .flatten()
        })
}

fn part_text(part: &Part, f: &str, krate: &str) -> Option<String> {
    let bytes = fs::read(rel(&part.dir.join("files"), f)).ok()?;
    util::as_text(&bytes).map(|t| parts::rename_crate(t, krate).replace("\r\n", "\n"))
}

fn update_one(
    app: &Path,
    krate: &str,
    old: &Part,
    new: &Part,
    opts: &Options,
) -> Result<(usize, bool)> {
    let old_files: BTreeSet<String> = util::files_under(&old.dir.join("files"))?
        .into_iter()
        .collect();
    let new_files: BTreeSet<String> = util::files_under(&new.dir.join("files"))?
        .into_iter()
        .collect();
    let labels = (
        "app".to_owned(),
        format!("{} {}", old.name, old.version),
        format!("{} {}", new.name, new.version),
    );
    let mut conflicts = 0;
    let mut changed = false;
    let report = |path: &str, o: &Outcome| {
        let line = match o {
            Outcome::Same => return,
            Outcome::Kept => format!("  keep   {path} (only the app changed it)"),
            Outcome::Took => format!("  take   {path} (the app hadn't changed it)"),
            Outcome::Merged => format!("  merge  {path} (both changed it; merged cleanly)"),
            Outcome::Conflict(n) => format!("  CONFLICT {path}: {n} hunk(s) with markers"),
            Outcome::Created => format!("  create {path} (new in this version)"),
            Outcome::Deleted => {
                format!("  remove {path} (gone from this version, unchanged in the app)")
            }
            Outcome::Note(n) => format!("  note   {path}: {n}"),
        };
        println!("{line}");
    };
    let write = |path: &str, text: &str| -> Result<()> {
        if !opts.dry_run {
            util::write_text(&rel(app, path), text)?;
        }
        Ok(())
    };
    for f in &new_files {
        let dest = rel(app, f);
        let Some(theirs) = part_text(new, f, krate) else {
            // Binary: replace only when the app's copy is the old one.
            let n = fs::read(rel(&new.dir.join("files"), f))?;
            let o = fs::read(rel(&old.dir.join("files"), f)).ok();
            let a = fs::read(&dest).ok();
            if a.as_deref() == Some(&n[..]) {
                continue;
            }
            if a.is_none() || a == o {
                if !opts.dry_run {
                    util::write_bytes(&dest, &n)?;
                }
                changed = true;
                report(f, &Outcome::Took);
            } else {
                report(
                    f,
                    &Outcome::Note(
                        "binary, changed in the app and in the part: kept the app's".into(),
                    ),
                );
            }
            continue;
        };
        let base = part_text(old, f, krate);
        let ours = read(&dest);
        let outcome = match (base, ours) {
            (_, Some(o)) if o == theirs => Outcome::Same,
            (Some(b), Some(o)) if b == theirs => {
                let _ = o;
                Outcome::Kept
            }
            (Some(b), Some(o)) if o == b => {
                write(f, &theirs)?;
                Outcome::Took
            }
            (Some(b), Some(o)) => {
                let m = merge3(&b, &o, &theirs, (&labels.0, &labels.1, &labels.2));
                write(f, &m.text)?;
                if m.conflicts > 0 {
                    conflicts += m.conflicts;
                    Outcome::Conflict(m.conflicts)
                } else {
                    Outcome::Merged
                }
            }
            (None, Some(o)) => {
                // New in the part, and the app already has a file there: merge against nothing.
                let m = merge3("", &o, &theirs, (&labels.0, &labels.1, &labels.2));
                write(f, &m.text)?;
                conflicts += m.conflicts;
                Outcome::Conflict(m.conflicts.max(1))
            }
            (Some(b), None) => {
                if b == theirs {
                    Outcome::Note(
                        "the app deleted it; the part didn't change it: still deleted".into(),
                    )
                } else {
                    Outcome::Note("the app deleted it and the part changed it: still deleted; read the new version in the part".into())
                }
            }
            (None, None) => {
                write(f, &theirs)?;
                Outcome::Created
            }
        };
        if !matches!(outcome, Outcome::Same | Outcome::Kept | Outcome::Note(_)) {
            changed = true;
        }
        report(f, &outcome);
    }
    for f in old_files.difference(&new_files) {
        let dest = rel(app, f);
        let (Some(b), Some(o)) = (part_text(old, f, krate), read(&dest)) else {
            continue;
        };
        if o == b {
            if !opts.dry_run {
                util::ctx(fs::remove_file(&dest), "removing", &dest)?;
            }
            changed = true;
            report(f, &Outcome::Deleted);
        } else {
            report(
                f,
                &Outcome::Note("gone from the new version, changed in the app: kept".into()),
            );
        }
    }
    // Wiring.
    let have = parts::installed(app);
    let applies = |w: &Wire| {
        w.when
            .as_deref()
            .is_none_or(|when| have.iter().any(|h| h == when))
    };
    let key = |w: &Wire| {
        format!(
            "{}\u{0}{}\u{0}{}",
            w.file,
            w.anchor,
            w.when.clone().unwrap_or_default()
        )
    };
    for w in new.wires.iter().filter(|w| applies(w)) {
        let text = parts::rename_crate(&w.text, krate);
        let old_w = old.wires.iter().find(|o| key(o) == key(w));
        // Unchanged wiring stays as the app has it (rustfmt may have reordered it).
        if old_w.is_some_and(|o| o.text == w.text) {
            continue;
        }
        let Some(file_text) = read(&rel(app, &w.file)) else {
            report(
                &w.file,
                &Outcome::Note(format!(
                    "missing; its `samples: {}` wiring can't be updated",
                    w.anchor
                )),
            );
            continue;
        };
        if find_block(&file_text, &text).is_some() {
            continue;
        }
        let updated = match old_w {
            Some(o) => {
                let old_text = parts::rename_crate(&o.text, krate);
                match find_block(&file_text, &old_text) {
                    Some((start, _)) => remove_block(&file_text, &old_text).map(|without| {
                        let lines: Vec<&str> = without.split_inclusive('\n').collect();
                        let indent: String = file_text
                            .split_inclusive('\n')
                            .nth(start)
                            .map(|l| l.chars().take_while(|c| *c == ' ').collect())
                            .unwrap_or_default();
                        let mut out = String::new();
                        for (i, l) in lines.iter().enumerate() {
                            if i == start {
                                for t in text.lines() {
                                    if t.trim().is_empty() {
                                        out.push('\n');
                                    } else {
                                        out.push_str(&format!("{indent}{t}\n"));
                                    }
                                }
                            }
                            out.push_str(l);
                        }
                        out
                    }),
                    None => None,
                }
            }
            None => parts::insert_at(&file_text, &w.anchor, &text),
        };
        match updated {
            Some(t) => {
                write(&w.file, &t)?;
                changed = true;
                report(
                    &w.file,
                    &Outcome::Note(format!("wiring at `samples: {}` updated", w.anchor)),
                );
            }
            None => {
                conflicts += 1;
                report(
                    &w.file,
                    &Outcome::Note(format!(
                        "CONFLICT: the `samples: {}` wiring changed in the part, and the app's copy isn't the old one; make it:\n{}",
                        w.anchor,
                        text.lines()
                            .map(|l| format!("         {l}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )),
                );
            }
        }
    }
    for o in old
        .wires
        .iter()
        .filter(|o| !new.wires.iter().any(|w| key(w) == key(o)) && applies(o))
    {
        let old_text = parts::rename_crate(&o.text, krate);
        if let Some(file_text) = read(&rel(app, &o.file))
            && let Some(t) = remove_block(&file_text, &old_text)
        {
            write(&o.file, &t)?;
            changed = true;
            report(
                &o.file,
                &Outcome::Note(format!(
                    "wiring at `samples: {}` removed (gone from the part)",
                    o.anchor
                )),
            );
        }
    }
    for c in new.crates.iter().filter(|c| !old.crates.contains(c)) {
        println!("  cargo add {c}");
        if !opts.dry_run {
            let mut a: Vec<&str> = vec!["add", "-q"];
            a.extend(c.split_whitespace());
            util::run(app, "cargo", &a)?;
        }
        changed = true;
    }
    Ok((conflicts, changed))
}
