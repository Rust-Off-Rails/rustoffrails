//! Golden files: each generator's plan against the template app, compared with
//! `testdata/golden/<case>.txt` (every created file and every wire, in order). A change to what
//! a generator writes shows up as a diff here; after an intended one:
//! `UPDATE_GOLDEN=1 cargo test golden` and review the diff.

use std::path::{Path, PathBuf};

use super::app::AppInfo;
use super::plan::{Action, Plan};
use super::{Flags, plan_for};

fn template() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../template/app")
}

fn render(plan: &Plan) -> String {
    let mut out = format!("# {}\n# options: {:?}\n", plan.command(), plan.options);
    for a in &plan.actions {
        match a {
            Action::Create { path, content } => {
                out.push_str(&format!("\n=== create {path}\n{content}"));
            }
            Action::Shared { path, content } => {
                out.push_str(&format!("\n=== shared {path}\n{content}"));
            }
            Action::Insert(i) => {
                out.push_str(&format!(
                    "\n=== wire {} @ {}{}\n{}",
                    i.file,
                    i.anchor.describe(),
                    i.owner
                        .as_ref()
                        .map(|o| format!(" (for {o})"))
                        .unwrap_or_default(),
                    i.text
                ));
            }
            Action::Crate { args, .. } => out.push_str(&format!("\n=== cargo add {args}\n")),
            Action::Byproduct { path, .. } => out.push_str(&format!("\n=== byproduct {path}\n")),
        }
    }
    out.replace("\r\n", "\n")
}

fn check(case: &str, kind: &str, args: &[&str], public: bool) {
    let info = AppInfo::load(&template()).expect("the template app");
    let mut flags = Flags {
        public,
        ..Flags::default()
    };
    flags
        .recorded
        .insert("stamp".into(), "20261017_120000".into());
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let kind = if kind == "page_profile" {
        flags
            .values
            .insert("path".into(), "/people/{handle}".into());
        "page"
    } else {
        kind
    };
    let plan = plan_for(kind, &info, &args, &flags).expect("a plan");
    let got = render(&plan);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/golden")
        .join(format!("{case}.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, &got).expect("write golden");
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "{} missing: UPDATE_GOLDEN=1 cargo test golden",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    if want != got {
        let line = want
            .lines()
            .zip(got.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(want.lines().count().min(got.lines().count()));
        panic!(
            "{case}: the generated code changed (first difference at line {}):\n  want: {:?}\n  got:  {:?}\nIf intended: UPDATE_GOLDEN=1 cargo test golden, then review the diff",
            line + 1,
            want.lines().nth(line).unwrap_or(""),
            got.lines().nth(line).unwrap_or("")
        );
    }
}

#[test]
fn golden_migration() {
    check(
        "migration_create_widgets",
        "migration",
        &["create_widgets", "name:string", "size:integer:optional"],
        false,
    );
    check(
        "migration_add_column",
        "migration",
        &["add_note_to_cache_entries", "note:string:optional:index"],
        false,
    );
}

#[test]
fn golden_model() {
    check(
        "model_note",
        "model",
        &["Note", "title:string:unique", "body:text"],
        true,
    );
}

#[test]
fn golden_resource_every_field_type() {
    check(
        "resource_items",
        "resource",
        &[
            "items",
            "name:string",
            "notes:text?",
            "count:integer",
            "big:bigint",
            "flag:boolean",
            "active:boolean?",
            "weight:decimal{8.3}",
            "price:money",
            "at:datetime",
            "day:date?",
            "code:uuid:unique",
            "meta:json?",
        ],
        true,
    );
}

#[test]
fn golden_page_and_controller() {
    check("page_about", "page", &["about"], false);
    check(
        "controller_reports",
        "controller",
        &["reports", "index", "summary"],
        false,
    );
    check(
        "controller_blog_params",
        "controller",
        &["blog", "index", "show:/blog/{slug}"],
        false,
    );
    check("page_profile_param", "page_profile", &["profile"], false);
}

#[test]
fn golden_test() {
    check("test_request", "test", &["request", "checkout"], false);
}

/// The resource snippet is the resource generator's output, as files: the same code (formatting,
/// doc lines and `// unscoped:` notes aside), its model test moved into the feature's test folder,
/// and the same wiring. A change to one is a change to the other: re-export the snippet from
/// `g resource notes title:string body:text --public` (template/snippets/resource/README.md).
#[test]
fn the_resource_snippet_is_what_the_resource_generator_writes() {
    use super::plan::{Action, same_content};
    let info = AppInfo::load(&template()).expect("the template app");
    let mut flags = Flags {
        public: true,
        ..Flags::default()
    };
    flags
        .recorded
        .insert("stamp".into(), "20261018_000100".into());
    let args: Vec<String> = ["notes", "title:string", "body:text"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let plan = super::plan_kind("resource", &info, &args, &flags).expect("a plan");
    let snippet = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../template/snippets/resource");
    let comparable = |text: &str| -> String {
        text.lines()
            .filter(|l| !l.trim_start().starts_with("//!") && l.trim() != "mod model_test;")
            .map(|l| match l.find(" // unscoped:") {
                Some(at) => &l[..at],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut seen = 0;
    for a in &plan.actions {
        let Action::Create { path, content } = a else {
            continue;
        };
        let at = if path == "tests/app/models/note_test.rs" {
            "tests/app/notes/model_test.rs".to_owned()
        } else {
            path.clone()
        };
        let file = snippet.join("files").join(&at);
        let text = std::fs::read_to_string(&file).unwrap_or_else(|_| {
            panic!("the resource snippet has no {at} (the generator writes {path})")
        });
        assert!(
            same_content(&comparable(&text), &comparable(content)),
            "template/snippets/resource/files/{at} differs from what `g resource notes` writes"
        );
        seen += 1;
    }
    let shipped = crate::util::files_under(&snippet.join("files")).expect("files");
    assert_eq!(
        seen,
        shipped.len(),
        "the snippet has files the generator doesn't write"
    );
    let toml = std::fs::read_to_string(snippet.join("snippet.toml")).expect("snippet.toml");
    for a in &plan.actions {
        let Action::Insert(i) = a else { continue };
        if i.owner.is_some() || i.file == "tests/app/models/mod.rs" {
            continue; // the snippet keeps its model test in tests/app/notes/
        }
        let wire = format!(
            "file = \"{}\"\nanchor = \"{}\"\ntext = '''\n{}'''",
            i.file,
            i.anchor.describe().trim_start_matches("samples: "),
            i.text.trim_start_matches('\n')
        );
        assert!(
            toml.contains(&wire),
            "snippet.toml has no wire like the generator's:\n{wire}"
        );
    }
}
