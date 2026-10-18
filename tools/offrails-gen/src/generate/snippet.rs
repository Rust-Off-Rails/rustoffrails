//! Snippets: small, tested code an agent copies (smaller than a part): `template/snippets/<name>/`
//! with a README.md, a `snippet.toml` in a part's `sample.toml` format, and `files/` at app paths.
//! `cargo offrails snippet add <name> [--as <model>]` is a generation like any other (kind
//! `snippet`): its files and wiring are recorded in docs/generated.toml, so
//! `cargo offrails snippet remove <name>` (= `destroy snippet <name>`) takes back exactly them.
//! `snippet show <name>` prints all of it, for copying by hand.

use std::path::{Path, PathBuf};

use super::Flags;
use super::app::AppInfo;
use super::manifest::Manifest;
use super::names::Names;
use super::plan::Plan;
use crate::parts::{self, Part};
use crate::util::{self, Fail, Result, rel};

/// A snippet: the part-format fields, plus the model its files are written for (`as`, renamed by
/// `--as`) and the snippets it builds on.
pub struct Snippet {
    pub part: Part,
    pub model: Option<String>,
    pub snippets: Vec<String>,
}

fn root(fw: &Path) -> PathBuf {
    fw.join("template").join("snippets")
}

/// Every snippet the framework ships, by name.
pub fn catalog(fw: &Path) -> Result<Vec<Snippet>> {
    let dir = root(fw);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("snippet.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        out.push(load(fw, &n)?);
    }
    Ok(out)
}

pub fn load(fw: &Path, name: &str) -> Result<Snippet> {
    let dir = root(fw).join(name);
    if !dir.join("snippet.toml").is_file() {
        let known: Vec<String> = catalog(fw)?.into_iter().map(|s| s.part.name).collect();
        return Err(Fail::usage(format!(
            "no snippet named '{name}' (cargo offrails snippets lists them: {})",
            known.join(", ")
        )));
    }
    // The same strict subset as a part's sample.toml.
    let path = dir.join("snippet.toml");
    let text = util::read_text(&path)?;
    let part = parts::parse_file(name, &dir, &path)?;
    let mut model = None;
    let mut snippets = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[[") {
            break;
        }
        if let Some((k, v)) = t.split_once('=') {
            let v = v.trim();
            match k.trim() {
                "as" => model = Some(v.trim_matches('"').to_owned()),
                "snippets" => {
                    snippets = v
                        .trim_matches(['[', ']'])
                        .split(',')
                        .map(|s| s.trim().trim_matches('"').to_owned())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                _ => {}
            }
        }
    }
    Ok(Snippet {
        part,
        model,
        snippets,
    })
}

/// The name a snippet is recorded under: `atom-feed`, or `resource:article` with `--as article`.
pub fn entry_name(args: &[String]) -> String {
    match args {
        [name] => name.clone(),
        [name, model, ..] => format!("{name}:{}", super::names::snake(model)),
        [] => String::new(),
    }
}

fn is_lower(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// `text` with one model's names swapped for another's, on word boundaries: `notes`, `note`,
/// `Notes`, `Note` (`new_note`, `NoteParams`, `NewNote`, `/notes/{id}` and "Note was…" all
/// change; `notebook` and `denote` don't).
pub fn rename_model(text: &str, from: &Names, to: &Names) -> String {
    let mut pairs: Vec<(String, String)> = vec![
        (from.plural_camel.clone(), to.plural_camel.clone()),
        (from.model.clone(), to.model.clone()),
        (from.plural.clone(), to.plural.clone()),
        (from.singular.clone(), to.singular.clone()),
    ];
    pairs.sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'outer: while i < bytes.len() {
        for (f, t) in &pairs {
            let fb = f.as_bytes();
            if bytes[i..].starts_with(fb) {
                let before = if i == 0 { None } else { Some(bytes[i - 1]) };
                let after = bytes.get(i + fb.len()).copied();
                let capital = fb[0].is_ascii_uppercase();
                let ok_before = match before {
                    None => true,
                    Some(b) if capital => !b.is_ascii_uppercase(),
                    Some(b) => !is_lower(b) && !b.is_ascii_uppercase(),
                };
                let ok_after = after.is_none_or(|a| !is_lower(a));
                if ok_before && ok_after {
                    out.push_str(t);
                    i += fb.len();
                    continue 'outer;
                }
            }
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

struct Renamer {
    krate: String,
    models: Option<(Names, Names)>,
}

impl Renamer {
    fn apply(&self, text: &str) -> String {
        let t = parts::rename_crate(text, &self.krate);
        match &self.models {
            Some((from, to)) => rename_model(&t, from, to),
            None => t,
        }
    }
}

/// `snippet add <name> [--as <model>]`, as a plan: its files, its wiring, its crates.
pub fn plan(info: &AppInfo, args: &[String], flags: &Flags) -> Result<Plan> {
    let Some(name) = args.first() else {
        return Err(Fail::usage(
            "snippet add needs a name (cargo offrails snippets lists them)",
        ));
    };
    let fw = util::framework_root(None)?;
    let sn = load(&fw, name)?;
    let entry = entry_name(args);
    let mut plan = Plan::new("snippet", &entry, args);
    let models = match (&sn.model, args.get(1)) {
        (None, Some(m)) => {
            return Err(Fail::usage(format!(
                "--as {m}: the {name} snippet isn't written for a model; it has nothing to rename"
            )));
        }
        (Some(from), Some(to)) => {
            if to.contains('_') || to.contains('-') {
                return Err(Fail::usage(format!(
                    "--as {to}: one word (article, bookmark); for a multi-word model, `cargo offrails g resource` writes the same code"
                )));
            }
            let from = Names::new(from)?;
            let to = Names::new(to)?;
            plan.tag = Some(if name == "resource" {
                to.plural.clone()
            } else {
                format!("{name} {}", to.singular)
            });
            Some((from, to))
        }
        (Some(from), None) => {
            plan.tag = Some(if name == "resource" {
                Names::new(from)?.plural
            } else {
                name.clone()
            });
            None
        }
        (None, None) => None,
    };
    let have = parts::installed(&info.dir);
    for dep in &sn.part.depends_on {
        if !have.contains(dep) {
            return Err(Fail::usage(format!(
                "the {name} snippet needs the {dep} part: cargo offrails add {dep}"
            )));
        }
    }
    if !flags.replan {
        let manifest = Manifest::load(&info.dir)?;
        for dep in &sn.snippets {
            let want = match (&models, load(&fw, dep)?.model) {
                (Some((_, to)), Some(_)) => format!("{dep}:{}", to.singular),
                _ => dep.clone(),
            };
            if manifest.find("snippet", &want).is_none() {
                let more = args
                    .get(1)
                    .map(|m| format!(" --as {m}"))
                    .unwrap_or_default();
                return Err(Fail::usage(format!(
                    "the {name} snippet builds on the {dep} snippet: cargo offrails snippet add {dep}{more}"
                )));
            }
        }
    }
    let r = Renamer {
        krate: info.krate.clone(),
        models,
    };
    let files = sn.part.dir.join("files");
    for f in util::files_under(&files)? {
        let text = util::read_text(&rel(&files, &f))?;
        let path = r.apply(&f);
        if path.starts_with("migration/") {
            plan.migrations = true;
        }
        plan.create(path, r.apply(&text));
    }
    for w in &sn.part.wires {
        plan.insert(&r.apply(&w.file), &w.anchor, r.apply(&w.text));
    }
    for c in &sn.part.crates {
        let first = c.split_whitespace().next().unwrap_or(c);
        plan.cargo(c, &format!("\n{first} ="));
    }
    plan.option("version", sn.part.version.clone());
    if plan.migrations {
        plan.next.push(
            "cargo run -q -- db migrate   # then prove down(): db rollback && db migrate".into(),
        );
    }
    plan.next.push(format!(
        "read .offrails/template/snippets/{name}/README.md (\"After adding\", \"Adapting it\"): every `// APP:` line is a decision to make"
    ));
    plan.next.push(format!(
        "commit now (git add -A && git commit -m \"snippet {entry}\"); `cargo offrails snippet remove {}` takes it back while its files are unchanged",
        args.join(" --as ")
    ));
    Ok(plan)
}

/// `snippets`: every snippet, what it needs, and whether the app has it.
pub fn list(fw: &Path, app: Option<&Path>) -> Result<()> {
    let all = catalog(fw)?;
    let manifest = app.and_then(|a| Manifest::load(a).ok());
    let width = all.iter().map(|s| s.part.name.len()).max().unwrap_or(4);
    println!(
        "{:<7} {:<width$}  {:<20}  what it is",
        "state", "snippet", "needs"
    );
    for s in &all {
        let mut needs: Vec<String> = s.part.depends_on.clone();
        needs.extend(s.snippets.iter().map(|n| format!("{n} (snippet)")));
        let needs = if needs.is_empty() {
            "-".to_owned()
        } else {
            needs.join(", ")
        };
        let added = manifest.as_ref().is_some_and(|m| {
            m.entries.iter().any(|e| {
                e.kind == "snippet"
                    && (e.name == s.part.name || e.name.starts_with(&format!("{}:", s.part.name)))
            })
        });
        let summary: String = if s.part.summary.chars().count() > 100 {
            format!("{}…", s.part.summary.chars().take(99).collect::<String>())
        } else {
            s.part.summary.clone()
        };
        println!(
            "{:<7} {:<width$}  {needs:<20}  {summary}",
            if added { "added" } else { "-" },
            s.part.name
        );
    }
    println!(
        "\nRead one (README, files, wiring), to copy by hand: cargo offrails snippet show <name>"
    );
    println!(
        "Add one (copied and wired, recorded in docs/generated.toml): cargo offrails snippet add <name> [--as <model>]"
    );
    println!("Take it back (unchanged files only): cargo offrails snippet remove <name>");
    Ok(())
}

/// `snippet show <name>`: the README, then every file and every wire, as they'd be added.
pub fn show(fw: &Path, name: &str) -> Result<()> {
    let sn = load(fw, name)?;
    let readme = sn.part.dir.join("README.md");
    if readme.is_file() {
        println!("{}", util::read_text(&readme)?.trim_end());
    }
    println!("\n---- snippet.toml: {} {}", sn.part.name, sn.part.version);
    for c in &sn.part.crates {
        println!("cargo add {c}");
    }
    let files = sn.part.dir.join("files");
    for f in util::files_under(&files)? {
        println!("\n---- file {f}");
        print!("{}", util::read_text(&rel(&files, &f))?);
    }
    for w in &sn.part.wires {
        println!("\n---- wire {} @ samples: {}", w.file, w.anchor);
        print!("{}", w.text);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_renames_on_word_boundaries_only() {
        let from = Names::new("note").unwrap();
        let to = Names::new("article").unwrap();
        let t = "pub mod notes; use crate::models::{note, note::NoteParams};\nNotesIndex NewNote new_note notes_test /notes/{id} \"Note was saved\" notebook denote Notebook footnotes\n";
        assert_eq!(
            rename_model(t, &from, &to),
            "pub mod articles; use crate::models::{article, article::ArticleParams};\nArticlesIndex NewArticle new_article articles_test /articles/{id} \"Article was saved\" notebook denote Notebook footnotes\n"
        );
    }

    #[test]
    fn every_shipped_snippet_parses_and_documents_itself() {
        let fw = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let all = catalog(&fw).unwrap();
        assert!(all.len() >= 8, "{} snippets", all.len());
        for s in &all {
            let readme = util::read_text(&s.part.dir.join("README.md")).unwrap();
            for heading in ["## When to use it", "## Files", "## Wiring", "## Remove it"] {
                assert!(
                    readme.contains(heading),
                    "{}: README.md has no `{heading}`",
                    s.part.name
                );
            }
            assert!(
                !util::files_under(&s.part.dir.join("files"))
                    .unwrap()
                    .is_empty(),
                "{}: no files",
                s.part.name
            );
            for dep in &s.snippets {
                assert!(
                    all.iter().any(|o| &o.part.name == dep),
                    "{}: needs unknown snippet {dep}",
                    s.part.name
                );
            }
        }
    }
}
