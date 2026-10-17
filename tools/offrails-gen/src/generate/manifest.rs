//! `docs/generated.toml`: what each `cargo offrails generate` wrote, so `destroy` removes exactly
//! that (and only what's unchanged) and a re-run is a no-op. Committed with the app; a strict
//! TOML subset, read and written here with the standard library.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::util::{self, Result, rel};

pub const PATH: &str = "docs/generated.toml";

const HEADER: &str = "# Written by `cargo offrails generate`; read by `cargo offrails destroy` and by re-runs.\n\
# One [[generated]] per command: its arguments, the choices it made, and the hash of each file it\n\
# created (as formatted), so destroy removes only files nobody changed. Keep it in git; don't edit it.\n";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub kind: String,
    pub name: String,
    pub args: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub date: String,
    /// Created file → hash.
    pub files: BTreeMap<String, String>,
    pub shared: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub entries: Vec<Entry>,
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unquote(s: &str) -> String {
    let s = s.trim().trim_end_matches(',').trim();
    let inner = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s);
    inner.replace("\\\"", "\"").replace("\\\\", "\\")
}

impl Manifest {
    pub fn load(app: &Path) -> Result<Self> {
        let path = rel(app, PATH);
        if !path.is_file() {
            return Ok(Self::default());
        }
        Ok(Self::parse(&util::read_text(&path)?))
    }

    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        let mut cur: Option<Entry> = None;
        let mut list: Option<String> = None;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[generated]]" {
                if let Some(e) = cur.take() {
                    entries.push(e);
                }
                cur = Some(Entry::default());
                list = None;
                continue;
            }
            let Some(e) = cur.as_mut() else { continue };
            if let Some(key) = &list {
                if line.starts_with(']') {
                    list = None;
                    continue;
                }
                let item = unquote(line);
                match key.as_str() {
                    "files" => {
                        if let Some((p, h)) = item.rsplit_once(' ') {
                            e.files.insert(p.to_owned(), h.to_owned());
                        }
                    }
                    "shared" => {
                        e.shared.insert(item);
                    }
                    "args" => e.args.push(item),
                    _ => {}
                }
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            if v == "[" {
                list = Some(k.to_owned());
                continue;
            }
            match k {
                "kind" => e.kind = unquote(v),
                "name" => e.name = unquote(v),
                "date" => e.date = unquote(v),
                "options" => {
                    for pair in unquote(v).split_whitespace() {
                        if let Some((a, b)) = pair.split_once('=') {
                            e.options.insert(a.to_owned(), b.to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(e) = cur.take() {
            entries.push(e);
        }
        Manifest { entries }
    }

    pub fn render(&self) -> String {
        let mut out = String::from(HEADER);
        for e in &self.entries {
            out.push_str("\n[[generated]]\n");
            out.push_str(&format!("kind = {}\n", quote(&e.kind)));
            out.push_str(&format!("name = {}\n", quote(&e.name)));
            out.push_str(&format!("date = {}\n", quote(&e.date)));
            out.push_str("args = [\n");
            for a in &e.args {
                out.push_str(&format!("  {},\n", quote(a)));
            }
            out.push_str("]\n");
            let opts: Vec<String> = e.options.iter().map(|(k, v)| format!("{k}={v}")).collect();
            out.push_str(&format!("options = {}\n", quote(&opts.join(" "))));
            out.push_str("files = [\n");
            for (p, h) in &e.files {
                out.push_str(&format!("  {},\n", quote(&format!("{p} {h}"))));
            }
            out.push_str("]\n");
            if !e.shared.is_empty() {
                out.push_str("shared = [\n");
                for s in &e.shared {
                    out.push_str(&format!("  {},\n", quote(s)));
                }
                out.push_str("]\n");
            }
        }
        out
    }

    /// Writes it, or deletes it when nothing is recorded (the app as it was before generating).
    pub fn save(&self, app: &Path) -> Result<()> {
        let path = rel(app, PATH);
        if self.entries.is_empty() {
            if path.is_file() {
                util::ctx(std::fs::remove_file(&path), "removing", &path)?;
            }
            return Ok(());
        }
        util::write_text(&path, &self.render())
    }

    pub fn find(&self, kind: &str, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.kind == kind && e.name == name)
    }

    pub fn remove(&mut self, kind: &str, name: &str) {
        self.entries.retain(|e| !(e.kind == kind && e.name == name));
    }

    pub fn upsert(&mut self, entry: Entry) {
        self.remove(&entry.kind.clone(), &entry.name.clone());
        self.entries.push(entry);
    }

    /// Shared files recorded by entries other than (kind, name).
    pub fn shared_by_others(&self, kind: &str, name: &str) -> BTreeSet<String> {
        self.entries
            .iter()
            .filter(|e| !(e.kind == kind && e.name == name))
            .flat_map(|e| e.shared.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut m = Manifest::default();
        let mut e = Entry {
            kind: "resource".into(),
            name: "posts".into(),
            args: vec!["posts".into(), "title:string".into(), "say:\"hi\"".into()],
            date: "2026-10-17".into(),
            ..Entry::default()
        };
        e.options.insert("scope".into(), "user".into());
        e.options.insert("stamp".into(), "20261017_101500".into());
        e.files
            .insert("src/models/post.rs".into(), "0123456789abcdef".into());
        e.shared.insert("src/controllers/pages.rs".into());
        m.upsert(e.clone());
        let back = Manifest::parse(&m.render());
        assert_eq!(back.entries, vec![e]);
        assert!(back.find("resource", "posts").is_some());
    }
}
