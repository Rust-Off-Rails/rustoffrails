//! offrails-gen: the Rust off Rails generators, cross-platform (Linux, macOS, Windows).
//!
//! Inside an app, through the alias in `.cargo/config.toml`:
//!   cargo offrails generate <kind> <name> [field:type…] [--dry-run] [--force]   (alias: g)
//!   cargo offrails destroy <kind> <name> [--dry-run]                            (alias: d)
//!   cargo offrails snippets | snippet show|add|remove <name> [--as <model>]     small tested code to copy
//!   cargo offrails parts                       (alias: list-parts)
//!   cargo offrails add mailer [more parts…] [--dry-run] [--force] [--offline] [--no-fmt]   (alias: add-part)
//!   cargo offrails update-part <part>… [--dry-run]   3-way merge of a newer part into the app
//!   cargo offrails verify [--fast|--full]      the Definition of Done, in parallel
//!   cargo offrails brand [--check]             brand.toml → theme CSS, favicon set, social card
//!   cargo offrails prove [--strict]            screenshots + checks of every page → docs/proof/PROOF.md
//!
//! Before the app exists, from the framework (`$FW`: the plugin root, ~/.offrails/current or a checkout):
//!   cargo run -q --manifest-path "$FW/tools/offrails-gen/Cargo.toml" --target-dir <dir>/target/offrails-gen -- \
//!     new <dir> <crate_name> "<Display Name>" [--pitch "<one line>"] [--parts mailer,jobs] [--mode htmx]
//!
//! Exit status: 0 done, 1 refused or failed (nothing half-applied), 2 usage.

mod brand;
mod generate;
mod merge;
mod newapp;
mod parts;
mod prove;
mod update_part;
mod util;
mod verify;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use util::{Fail, Result};

const USAGE: &str = "usage:
  offrails-gen new <dir> <crate_name> \"<Display Name>\" [--pitch \"<one line>\"] [--parts a,b|none]
                   [--invite] [--mode htmx|inertia|spa|api] [--prod-db postgres|sqlite] [--no-git] [--offline]
                   [--framework <path>] [--worktree]
                   a framework that is a git checkout is vendored from HEAD (git archive HEAD), its
                   commit recorded in .offrails/VERSION; --worktree: its working tree as it is
  offrails-gen generate <kind> <name> [args…] [--dry-run] [--force] [--public] [--app <dir>]   (alias: g)
                   migration | model | resource (scaffold) | controller | page | api | job | mailer | channel | test
                   `generate --help` lists each kind, the field types and the options
  offrails-gen destroy <kind> <name> [--dry-run] [--app <dir>]   (alias: d) undo a generate (unchanged files only)
  offrails-gen snippets [--app <dir>]   small tested code to copy: every snippet, what it needs, whether it's in
  offrails-gen snippet show <name>      its README, files and wiring (to read, or copy by hand)
  offrails-gen snippet add <name> [--as <model>] [--dry-run] [--force]   copy and wire it, recorded
  offrails-gen snippet remove <name> [--as <model>] [--dry-run]          take it back (unchanged files only)
  offrails-gen add <part>… [--app <dir>] [--dry-run] [--force] [--offline] [--no-fmt]   (alias: add-part)
  offrails-gen update-part <part>… --framework <newer> [--base <fw>] [--app <dir>] [--dry-run] [--no-fmt] [--worktree]
                   merge a newer framework's part into the app: 3-way against the vendored .offrails/ (the
                   installed version), clean hunks applied, conflicts marked, app changes never overwritten
  offrails-gen parts [--app <dir>]   (alias: list-parts) every part, installed or not, and what it adds
  offrails-gen vendor --framework <newer framework> [--app <dir>] [--worktree]   re-copy a framework into
                   <app>/.offrails/ (a git checkout: from HEAD, as for new)
  offrails-gen verify [--fast|--full] [--no-postgres] [--test-threads <n>] [--app <dir>]
                   the Definition of Done in parallel: --fast while iterating, --full once at the end
  offrails-gen brand [--check] [--no-images] [--init] [--app <dir>]
                   brand.toml → assets/brand.css (theme, light + dark, WCAG AA checked), src/brand.rs,
                   the favicon set and the social card; --check: fail if they are stale
  offrails-gen prove [--strict] [--commit-screenshots] [--app <dir>]   (needs BASE=<the running app>)
                   every page in docs/routes.md: screenshots (desktop + mobile, light + dark), console,
                   CSP, axe, links, overflow, a visual diff; writes docs/proof/PROOF.md

In an app: cargo offrails <command> … (the alias in .cargo/config.toml). Docs: .offrails/template/README.md";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const VALUE_FLAGS: [&str; 14] = [
    "--as",
    "--layout",
    "--base",
    "--queue",
    "--every",
    "--path",
    "--pitch",
    "--parts",
    "--mode",
    "--prod-db",
    "--framework",
    "--app",
    "--title",
    "--test-threads",
];

fn parse_args(raw: Vec<String>) -> Result<Args> {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        if let Some((k, v)) = a.split_once('=').filter(|(k, _)| k.starts_with("--")) {
            flags.push((k.to_owned(), Some(v.to_owned())));
        } else if VALUE_FLAGS.contains(&a.as_str()) {
            let v = it
                .next()
                .ok_or_else(|| Fail::usage(format!("{a} needs a value\n\n{USAGE}")))?;
            flags.push((a, Some(v)));
        } else if a.starts_with("--") || a == "-h" {
            flags.push((a, None));
        } else {
            positional.push(a);
        }
    }
    Ok(Args { positional, flags })
}

impl Args {
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }
    fn value(&self, name: &str) -> Option<String> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.clone())
    }
    fn reject_unknown(&self, allowed: &[&str]) -> Result<()> {
        for (k, _) in &self.flags {
            if !allowed.contains(&k.as_str()) {
                return Err(Fail::usage(format!("unknown option {k}\n\n{USAGE}")));
            }
        }
        Ok(())
    }
    fn app(&self) -> Result<PathBuf> {
        let dir = self
            .value("--app")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let dir = util::ctx(std::fs::canonicalize(&dir), "no app at", &dir)?;
        Ok(util::strip_verbatim(dir))
    }
}

/// Inside an app, the framework is the app's vendored copy unless --framework or OFFRAILS_FW says otherwise.
fn framework_for(app: Option<&Path>, args: &Args) -> Result<PathBuf> {
    let explicit = args.value("--framework");
    if explicit.is_none()
        && std::env::var("OFFRAILS_FW").is_err()
        && let Some(app) = app
        && app.join(".offrails").join("template").is_dir()
    {
        return util::framework_root(Some(&app.join(".offrails").to_string_lossy()));
    }
    util::framework_root(explicit.as_deref())
}

fn split_list(v: Option<String>) -> Vec<String> {
    v.map(|s| {
        s.split(',')
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty() && p != "none")
            .collect()
    })
    .unwrap_or_default()
}

fn real_main() -> Result<()> {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        return Err(Fail::usage(USAGE));
    }
    let command = raw.remove(0);
    let args = parse_args(raw)?;
    if matches!(command.as_str(), "generate" | "g" | "destroy" | "d")
        && (args.flag("--help") || args.flag("-h") || args.positional.is_empty())
    {
        println!("{}", generate::USAGE);
        return Ok(());
    }
    if args.flag("--help")
        || args.flag("-h")
        || matches!(command.as_str(), "help" | "--help" | "-h")
    {
        println!("{USAGE}");
        return Ok(());
    }
    match command.as_str() {
        "generate" | "g" | "destroy" | "d" => {
            args.reject_unknown(&[
                "--app",
                "--dry-run",
                "--force",
                "--no-fmt",
                "--public",
                "--no-nav",
                "--nav",
                "--queue",
                "--every",
                "--path",
                "--layout",
                "--forget",
            ])?;
            let app = args.app()?;
            let mut values = std::collections::BTreeMap::new();
            for k in ["--queue", "--every", "--path", "--layout"] {
                if let Some(v) = args.value(k) {
                    values.insert(k.trim_start_matches('-').to_owned(), v);
                }
            }
            let flags = generate::Flags {
                dry_run: args.flag("--dry-run"),
                force: args.flag("--force"),
                fmt: !args.flag("--no-fmt"),
                public: args.flag("--public"),
                no_nav: args.flag("--no-nav"),
                nav: args.flag("--nav"),
                forget: args.flag("--forget"),
                values,
                ..generate::Flags::default()
            };
            let kind = args.positional[0].clone();
            let rest = &args.positional[1..];
            if matches!(command.as_str(), "destroy" | "d") {
                generate::destroy(&app, &kind, rest, flags)
            } else {
                generate::generate(&app, &kind, rest, flags)
            }
        }
        "snippets" | "list-snippets" => {
            args.reject_unknown(&["--app", "--framework"])?;
            let app = args.app().ok().filter(|a| a.join("Cargo.toml").is_file());
            let fw = framework_for(app.as_deref(), &args)?;
            generate::snippet::list(&fw, app.as_deref())
        }
        "snippet" => {
            let sub = args.positional.first().map(String::as_str).unwrap_or("");
            let rest: Vec<String> = args.positional.iter().skip(1).cloned().collect();
            let Some(name) = rest.first().cloned() else {
                return Err(Fail::usage(format!(
                    "snippet show|add|remove <name> (cargo offrails snippets lists them)\n\n{USAGE}"
                )));
            };
            match sub {
                "show" => {
                    args.reject_unknown(&["--app", "--framework"])?;
                    let app = args.app().ok().filter(|a| a.join("Cargo.toml").is_file());
                    let fw = framework_for(app.as_deref(), &args)?;
                    generate::snippet::show(&fw, &name)
                }
                "add" | "remove" => {
                    args.reject_unknown(&["--app", "--as", "--dry-run", "--force", "--no-fmt"])?;
                    let app = args.app()?;
                    let mut gen_args = vec![name];
                    if let Some(m) = args.value("--as") {
                        gen_args.push(m);
                    }
                    let flags = generate::Flags {
                        dry_run: args.flag("--dry-run"),
                        force: args.flag("--force"),
                        fmt: !args.flag("--no-fmt"),
                        ..generate::Flags::default()
                    };
                    if sub == "add" {
                        generate::generate(&app, "snippet", &gen_args, flags)
                    } else {
                        generate::destroy(&app, "snippet", &gen_args, flags)
                    }
                }
                other => Err(Fail::usage(format!(
                    "snippet {other}: show, add or remove\n\n{USAGE}"
                ))),
            }
        }
        "new" => {
            args.reject_unknown(&[
                "--pitch",
                "--parts",
                "--mode",
                "--prod-db",
                "--framework",
                "--no-git",
                "--offline",
                "--title",
                "--invite",
                "--worktree",
            ])?;
            let need = || {
                Fail::usage(format!(
                    "new needs <dir> <crate_name> \"<Display Name>\"\n\n{USAGE}"
                ))
            };
            let (dir, krate, title) = match args.positional.as_slice() {
                [dir, krate, title] => (dir.clone(), krate.clone(), title.clone()),
                [dir, krate] => (
                    dir.clone(),
                    krate.clone(),
                    args.value("--title").ok_or_else(need)?,
                ),
                _ => return Err(need()),
            };
            let fw = util::framework_root(args.value("--framework").as_deref())?;
            let snap = newapp::snapshot(&fw, args.flag("--worktree"))?;
            newapp::run(
                &snap.root,
                &newapp::NewOptions {
                    dir: PathBuf::from(dir),
                    crate_name: krate,
                    title,
                    pitch: args.value("--pitch"),
                    parts: split_list(args.value("--parts")),
                    mode: args.value("--mode").unwrap_or_else(|| "htmx".into()),
                    prod_db: args.value("--prod-db").unwrap_or_else(|| "postgres".into()),
                    git: !args.flag("--no-git"),
                    offline: args.flag("--offline"),
                    invite: args.flag("--invite"),
                    provenance: snap.provenance.clone(),
                },
            )
        }
        "add-part" | "add-parts" | "add" => {
            args.reject_unknown(&[
                "--app",
                "--dry-run",
                "--force",
                "--offline",
                "--no-fmt",
                "--framework",
            ])?;
            let mut names: Vec<String> = Vec::new();
            for p in &args.positional {
                names.extend(split_list(Some(p.clone())));
            }
            if names.is_empty() {
                return Err(Fail::usage(format!(
                    "add-part needs a part name (list-parts shows them)\n\n{USAGE}"
                )));
            }
            let app = args.app()?;
            let fw = framework_for(Some(&app), &args)?;
            parts::add(
                &fw,
                &app,
                &names,
                &parts::AddOptions {
                    force: args.flag("--force"),
                    dry_run: args.flag("--dry-run"),
                    offline: args.flag("--offline"),
                    fmt: !args.flag("--no-fmt"),
                    quiet_next: false,
                },
            )
            .map(|_| ())
        }
        "update-part" | "update-parts" => {
            args.reject_unknown(&[
                "--app",
                "--dry-run",
                "--no-fmt",
                "--framework",
                "--base",
                "--worktree",
            ])?;
            if args.positional.is_empty() {
                return Err(Fail::usage(format!(
                    "update-part needs a part name\n\n{USAGE}"
                )));
            }
            let app = args.app()?;
            let Some(newer) = args.value("--framework") else {
                return Err(Fail::usage(
                    "update-part needs --framework <the newer framework> (the plugin root, ~/.offrails/<version>, or a checkout); the base is the app's .offrails/",
                ));
            };
            // A checkout: its HEAD, as `vendor` will copy it (newapp::snapshot).
            let snap = newapp::snapshot(
                &util::framework_root(Some(&newer))?,
                args.flag("--worktree"),
            )?;
            let new_fw = snap.root.clone();
            let base = match args.value("--base") {
                Some(b) => util::framework_root(Some(&b))?,
                None => util::framework_root(Some(&app.join(".offrails").to_string_lossy()))?,
            };
            let mut names: Vec<String> = Vec::new();
            for p in &args.positional {
                names.extend(split_list(Some(p.clone())));
            }
            update_part::run(
                &base,
                &new_fw,
                &app,
                &names,
                &update_part::Options {
                    dry_run: args.flag("--dry-run"),
                    fmt: !args.flag("--no-fmt"),
                },
            )
        }
        "list-parts" | "parts" => {
            args.reject_unknown(&["--app", "--framework"])?;
            let app = args.app().ok().filter(|a| a.join("Cargo.toml").is_file());
            let fw = framework_for(app.as_deref(), &args)?;
            parts::list(&fw, app.as_deref())
        }
        "vendor" => {
            args.reject_unknown(&["--app", "--framework", "--worktree"])?;
            let app = args.app()?;
            let Some(explicit) = args.value("--framework") else {
                return Err(Fail::usage(
                    "vendor needs --framework <path to the newer framework>",
                ));
            };
            let fw = util::framework_root(Some(&explicit))?;
            if std::fs::canonicalize(app.join(".offrails"))
                .map(util::strip_verbatim)
                .ok()
                .as_deref()
                == Some(fw.as_path())
            {
                return Err(Fail::usage(
                    "vendor copies another framework in; --framework is this app's own .offrails/",
                ));
            }
            let snap = newapp::snapshot(&fw, args.flag("--worktree"))?;
            newapp::vendor(&snap.root, &app, snap.provenance.as_deref())?;
            println!(
                "vendored Rust off Rails {} into {}{}",
                util::framework_version(&snap.root)?,
                app.join(".offrails").display(),
                snap.provenance
                    .as_deref()
                    .map(|p| format!(" ({p})"))
                    .unwrap_or_default()
            );
            Ok(())
        }
        "verify" => {
            args.reject_unknown(&[
                "--app",
                "--fast",
                "--full",
                "--no-postgres",
                "--test-threads",
            ])?;
            if args.flag("--fast") && args.flag("--full") {
                return Err(Fail::usage("verify takes --fast or --full, not both"));
            }
            let test_threads = match args.value("--test-threads") {
                Some(n) => Some(n.parse::<usize>().map_err(|_| {
                    Fail::usage(format!("--test-threads needs a number, not {n:?}"))
                })?),
                None => None,
            };
            let app = args.app()?;
            verify::run(
                &app,
                &verify::VerifyOptions {
                    full: args.flag("--full"),
                    postgres: !args.flag("--no-postgres"),
                    test_threads,
                },
            )
        }
        "brand" => {
            args.reject_unknown(&["--app", "--check", "--no-images", "--init"])?;
            let app = args.app()?;
            brand::run(
                &app,
                &brand::Options {
                    check: args.flag("--check"),
                    images: !args.flag("--no-images"),
                    init: args.flag("--init"),
                    quiet: false,
                },
            )
        }
        "prove" => {
            args.reject_unknown(&["--app", "--strict", "--commit-screenshots", "--framework"])?;
            let app = args.app()?;
            let fw = framework_for(Some(&app), &args)?;
            prove::run(
                &fw,
                &app,
                &prove::Options {
                    strict: args.flag("--strict"),
                    commit_screenshots: args.flag("--commit-screenshots"),
                },
            )
        }
        other => Err(Fail::usage(format!("unknown command '{other}'\n\n{USAGE}"))),
    }
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("offrails-gen: {e}");
            ExitCode::from(e.code.clamp(1, 255) as u8)
        }
    }
}
