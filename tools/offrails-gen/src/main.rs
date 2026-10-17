//! offrails-gen: the Rust off Rails generators, cross-platform (Linux, macOS, Windows).
//!
//! Inside an app, through the alias in `.cargo/config.toml`:
//!   cargo offrails list-parts
//!   cargo offrails add-part mailer [more parts…] [--dry-run] [--force] [--offline] [--no-fmt]
//!
//! Before the app exists, from the framework (`$FW`: the plugin root, ~/.offrails/current or a checkout):
//!   cargo run -q --manifest-path "$FW/tools/offrails-gen/Cargo.toml" --target-dir <dir>/target/offrails-gen -- \
//!     new <dir> <crate_name> "<Display Name>" [--pitch "<one line>"] [--parts mailer,jobs] [--mode htmx]
//!
//! Exit status: 0 done, 1 refused or failed (nothing half-applied), 2 usage.

mod newapp;
mod parts;
mod util;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use util::{Fail, Result};

const USAGE: &str = "usage:
  offrails-gen new <dir> <crate_name> \"<Display Name>\" [--pitch \"<one line>\"] [--parts a,b|none]
                   [--mode htmx|inertia|spa|api] [--prod-db postgres|sqlite] [--no-git] [--offline]
                   [--framework <path>]
  offrails-gen add-part <name>… [--app <dir>] [--dry-run] [--force] [--offline] [--no-fmt]
  offrails-gen list-parts [--app <dir>]
  offrails-gen vendor --framework <newer framework> [--app <dir>]   re-copy a framework into <app>/.offrails/

In an app: cargo offrails <command> … (the alias in .cargo/config.toml). Docs: .offrails/template/README.md";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const VALUE_FLAGS: [&str; 7] = [
    "--pitch",
    "--parts",
    "--mode",
    "--prod-db",
    "--framework",
    "--app",
    "--title",
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
    if args.flag("--help")
        || args.flag("-h")
        || matches!(command.as_str(), "help" | "--help" | "-h")
    {
        println!("{USAGE}");
        return Ok(());
    }
    match command.as_str() {
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
            newapp::run(
                &fw,
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
                },
            )
        }
        "add-part" | "add-parts" => {
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
        "list-parts" | "parts" => {
            args.reject_unknown(&["--app", "--framework"])?;
            let app = args.app().ok().filter(|a| a.join("Cargo.toml").is_file());
            let fw = framework_for(app.as_deref(), &args)?;
            parts::list(&fw, app.as_deref())
        }
        "vendor" => {
            args.reject_unknown(&["--app", "--framework"])?;
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
            newapp::vendor(&fw, &app)?;
            println!(
                "vendored Rust off Rails {} into {}",
                util::framework_version(&fw)?,
                app.join(".offrails").display()
            );
            Ok(())
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
