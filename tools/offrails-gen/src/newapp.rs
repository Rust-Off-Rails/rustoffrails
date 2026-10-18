//! `new`: a new app from `template/app`, renamed, with the framework vendored into `.offrails/`
//! and the chosen parts added. Nothing networked runs unless parts are asked for (`cargo add`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::parts::{self, AddOptions};
use crate::util::{self, Fail, Result, rel};

/// The template's placeholders. A rename is these replacements over every text file.
pub const CRATE_TOKEN: &str = "offrails_app";
pub const KEBAB_TOKEN: &str = "offrails-app";
pub const TITLE_TOKEN: &str = "Offrails App";
/// The pitch: `APP_TAGLINE` (lowercase, no period) and the default description (a sentence).
pub const TAGLINE_TOKEN: &str = "a new Rust off Rails app";
pub const DESCRIPTION_TOKEN: &str = "A new Rust off Rails app.";

/// Top-level framework paths that never go into an app's `.offrails/` (generators/new.md step 1).
/// hooks/fingerprint.sh compares by content with the same list.
const VENDOR_EXCLUDES: [&str; 8] = [
    ".git",
    ".claude-plugin",
    "skills",
    "hooks",
    "evals",
    "brand",
    ".in_use",
    "media",
];

pub struct NewOptions {
    pub dir: PathBuf,
    pub crate_name: String,
    pub title: String,
    pub pitch: Option<String>,
    pub parts: Vec<String>,
    pub mode: String,
    pub prod_db: String,
    pub git: bool,
    pub offline: bool,
}

pub fn validate_crate_name(name: &str) -> Result<()> {
    let ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.ends_with('_');
    if !ok {
        return Err(Fail::usage(format!(
            "crate name '{name}' must be snake_case: a-z, 0-9 and _, starting with a letter (e.g. bookshelf)"
        )));
    }
    if [
        "migration",
        "app",
        "test",
        "tests",
        "std",
        "core",
        "self",
        "crate",
        "main",
    ]
    .contains(&name)
    {
        return Err(Fail::usage(format!(
            "crate name '{name}' is reserved here; pick a distinctive one"
        )));
    }
    Ok(())
}

pub fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty()
        || title
            .chars()
            .any(|c| matches!(c, '"' | '\\' | '{' | '}' | '<' | '>' | '\n' | '\r'))
    {
        return Err(Fail::usage(format!(
            "display name '{title}' must be non-empty plain text (no quotes, backslashes, braces or angle brackets)"
        )));
    }
    Ok(())
}

/// Replace the template tokens in one text.
pub fn rename(text: &str, krate: &str, title: &str, pitch: Option<&str>) -> String {
    let kebab = krate.replace('_', "-");
    let mut s = text
        .replace(CRATE_TOKEN, krate)
        .replace(KEBAB_TOKEN, &kebab)
        .replace(TITLE_TOKEN, title);
    if let Some(p) = pitch {
        let sentence = if p.ends_with('.') {
            p.to_owned()
        } else {
            format!("{p}.")
        };
        let tagline = p.trim_end_matches('.');
        s = s
            .replace(DESCRIPTION_TOKEN, &sentence)
            .replace(TAGLINE_TOKEN, tagline);
    }
    s
}

/// Build outputs and local state that never come from the template, even when someone built it in place.
fn skip_template_path(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or("");
    rel.split('/')
        .any(|seg| seg == "target" || seg == "node_modules" || seg == ".DS_Store")
        || rel == ".env"
        || rel == "static/app.css"
        || rel == "bin/tailwindcss"
        || (first == "storage" && rel != "storage/.keep")
        || rel.starts_with(".zap/zap-report")
        || rel == ".zap/zap.yaml"
}

fn dir_is_empty_enough(dir: &Path) -> Result<bool> {
    if !dir.exists() {
        return Ok(true);
    }
    for entry in util::ctx(fs::read_dir(dir), "reading", dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        // target/: `cargo run --target-dir <dir>/target/offrails-gen` builds this tool there first.
        if name != "target" && name != ".git" && name != ".DS_Store" {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Copy the framework into `<app>/.offrails/` (generators/new.md step 1), then date the vendoring.
pub fn vendor(fw: &Path, app: &Path) -> Result<()> {
    let dest = app.join(".offrails");
    if dest.exists() {
        util::ctx(fs::remove_dir_all(&dest), "removing", &dest)?;
    }
    let skip_dir = |d: &str| {
        VENDOR_EXCLUDES.contains(&d)
            || d.ends_with("/target")
            || d.ends_with("/node_modules")
            || d == "target"
    };
    for f in util::files_under_except(fw, &skip_dir)? {
        let first = f.split('/').next().unwrap_or("");
        if VENDOR_EXCLUDES.contains(&first)
            || f.split('/').any(|s| s == ".DS_Store")
            || f.starts_with("tools/offrails-gen/target/")
            || f.starts_with("template/app/target/")
        {
            continue;
        }
        util::copy_file(&rel(fw, &f), &rel(&dest, &f))?;
    }
    let version = util::framework_version(fw)?;
    util::write_text(
        &dest.join("VERSION"),
        &format!("{version}\nvendored {}\n", util::today_utc()),
    )?;
    Ok(())
}

pub fn run(fw: &Path, o: &NewOptions) -> Result<()> {
    validate_crate_name(&o.crate_name)?;
    validate_title(&o.title)?;
    match o.mode.as_str() {
        "htmx" | "api" | "inertia" | "spa" => {}
        m => {
            return Err(Fail::usage(format!(
                "--mode {m}: expected htmx | inertia | spa | api"
            )));
        }
    }
    match o.prod_db.as_str() {
        "postgres" | "sqlite" => {}
        d => {
            return Err(Fail::usage(format!(
                "--prod-db {d}: expected postgres | sqlite"
            )));
        }
    }
    if !dir_is_empty_enough(&o.dir)? {
        return Err(Fail::usage(format!(
            "{} exists and isn't empty; `new` only writes into a new or empty directory (use add-part in an existing app)",
            o.dir.display()
        )));
    }
    let catalog = parts::catalog(fw)?;
    for p in &o.parts {
        if p != "all" && !catalog.contains_key(p) {
            return Err(Fail::usage(format!(
                "--parts: no part named '{p}' (list-parts shows them)"
            )));
        }
    }
    let app = &o.dir;
    let template = fw.join("template").join("app");
    let version = util::framework_version(fw)?;
    let today = util::today_utc();

    println!(
        "new {} ({}) from Rust off Rails {version} template -> {}",
        o.crate_name,
        o.title,
        app.display()
    );
    let mut count = 0;
    for f in util::files_under_except(&template, &|d: &str| {
        d == "target" || d.ends_with("/node_modules")
    })? {
        if skip_template_path(&f) {
            continue;
        }
        let src = rel(&template, &f);
        let dest = rel(app, &f);
        util::copy_file(&src, &dest)?;
        let bytes = fs::read(&src)?;
        if let Some(text) = util::as_text(&bytes) {
            let renamed = rename(text, &o.crate_name, &o.title, o.pitch.as_deref());
            if renamed != text {
                util::write_text(&dest, &renamed)?;
            }
        }
        count += 1;
    }
    println!(
        "  copy {count} files from template/app (tokens {CRATE_TOKEN}, {KEBAB_TOKEN}, \"{TITLE_TOKEN}\" renamed)"
    );

    vendor(fw, app)?;
    println!("  vendor .offrails/ (Rust off Rails {version}, vendored {today})");

    // AGENTS.md and product/README.md come from the blueprints, the one copy of each.
    let mut agents = util::read_text(&fw.join("blueprints").join("AGENTS.md"))?;
    let api = if o.mode == "api" { "rest" } else { "none" };
    let header: [(&str, String); 11] = [
        ("APP", o.crate_name.clone()),
        (
            "FRAMEWORK",
            format!("Rust off Rails {version} (.offrails/, vendored {today})"),
        ),
        ("FRONTEND_MODE", "htmx".into()),
        (
            "UI_LIBRARY",
            "none   (none = no JS UI: htmx without islands, or api)".into(),
        ),
        ("ISLANDS", "no".into()),
        (
            "API",
            format!("{api}   (a versioned API for other programs; api mode is never none)"),
        ),
        (
            "DEV_DATABASE",
            "sqlite (storage/development.sqlite3)".into(),
        ),
        ("PROD_DATABASE", o.prod_db.clone()),
        ("DEPLOY_TARGET", "later".into()),
        ("ACCOUNTS", "none  ROLES: none  TENANCY: none".into()),
        (
            "EMAIL",
            "none (no mailer yet: cargo offrails add-part mailer adds it, with the dev mailbox)"
                .into(),
        ),
    ];
    for (k, v) in &header {
        if !util::set_header(&mut agents, k, v) {
            return Err(Fail::new(format!("blueprints/AGENTS.md has no {k}: line")));
        }
    }
    util::write_text(&app.join("AGENTS.md"), &agents)?;
    let product = util::read_text(&fw.join("blueprints").join("product-README.md"))?
        .replace("{{App Name}}", &o.title)
        .replace("{{YYYY-MM-DD}}", &today);
    util::write_text(&rel(app, "product/README.md"), &product)?;
    println!(
        "  write AGENTS.md, product/README.md (from .offrails/blueprints; fill them from the interview)"
    );

    let parts_note = if o.parts.is_empty() {
        "none".to_owned()
    } else {
        o.parts.join(",")
    };
    util::log_decision(
        app,
        &format!(
            "app generated from the Rust off Rails {version} template (cargo offrails new; mode=htmx, prod db={}, parts={parts_note})",
            o.prod_db
        ),
    )?;
    if o.mode != "htmx" {
        util::log_decision(
            app,
            &format!(
                "FRONTEND_MODE {} requested: generated the htmx template, to be converted per .offrails/template/README.md#other-modes (the header says htmx until the code matches)",
                o.mode
            ),
        )?;
    }

    if !o.parts.is_empty() {
        println!();
        parts::add(
            fw,
            app,
            &o.parts,
            &AddOptions {
                force: false,
                dry_run: false,
                offline: o.offline,
                fmt: true,
                quiet_next: true,
            },
        )?;
    }

    // brand.toml carries the new name: the theme CSS, src/brand.rs, the favicon set and the card
    // (after the parts: the mailer's layout takes the brand's email colors).
    if app.join("brand.toml").is_file() {
        crate::brand::run(
            app,
            &crate::brand::Options {
                check: false,
                images: true,
                init: false,
                quiet: true,
            },
        )?;
    }

    if o.parts.is_empty() && util::have("cargo") {
        // The new crate name sorts differently in `use` lists, and a pitch rewraps a line.
        if util::run(app, "cargo", &["fmt", "--all"]).is_err() {
            eprintln!("note: `cargo fmt --all` failed; run it before the first commit");
        }
    }

    if o.git && !app.join(".git").exists() {
        if util::have("git") {
            util::run(app, "git", &["init", "-q"])?;
            println!(
                "  git init (no commit yet: the baseline commit comes after the first green run)"
            );
        } else {
            println!("  note: git not found; run `git init` yourself");
        }
    }

    println!("\ndone. Next, in {}:", app.display());
    println!(
        "  bin/setup-tailwind && cargo offrails verify   # fmt, CSS, clippy, SQLite + Postgres (first build: minutes)"
    );
    println!(
        "  cargo offrails verify --full               # + offrails-check, coverage, deny, gitleaks"
    );
    println!(
        "  bin/setup && bin/dev                       # Windows: see .offrails/template/README.md#windows"
    );
    println!(
        "  edit brand.toml, then cargo offrails brand   # colors, fonts, radius, logo: theme, favicon set, social card"
    );
    if o.mode != "htmx" {
        println!(
            "  convert to {}: .offrails/template/README.md#other-modes",
            o.mode
        );
    }
    println!("  then write only the app's own code: product/ specs first, cargo offrails add-part");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_replaces_every_token() {
        let t = "name = \"offrails_app\"\nimage: offrails-app:local\nAPP_NAME = \"Offrails App\"\ntest_offrails_app_x\n";
        assert_eq!(
            rename(t, "book_shelf", "Book Shelf", None),
            "name = \"book_shelf\"\nimage: book-shelf:local\nAPP_NAME = \"Book Shelf\"\ntest_book_shelf_x\n"
        );
        let p = "TAGLINE: &str = \"a new Rust off Rails app\";\nDESC = \"A new Rust off Rails app.\";\n";
        assert_eq!(
            rename(p, "x", "X", Some("track the books you read")),
            "TAGLINE: &str = \"track the books you read\";\nDESC = \"track the books you read.\";\n"
        );
    }

    #[test]
    fn crate_names_are_checked() {
        assert!(validate_crate_name("bookshelf").is_ok());
        assert!(validate_crate_name("book_shelf2").is_ok());
        for bad in [
            "Bookshelf",
            "book-shelf",
            "2books",
            "",
            "migration",
            "books_",
        ] {
            assert!(validate_crate_name(bad).is_err(), "{bad}");
        }
        assert!(validate_title("Book \"Shelf\"").is_err());
        assert!(validate_title("Bookshelf").is_ok());
    }

    #[test]
    fn build_outputs_never_come_from_the_template() {
        for p in [
            "target/debug/x",
            ".env",
            "static/app.css",
            "bin/tailwindcss",
            "storage/development.sqlite3",
        ] {
            assert!(skip_template_path(p), "{p}");
        }
        for p in [
            "storage/.keep",
            ".env.example",
            "static/app.js",
            "src/main.rs",
        ] {
            assert!(!skip_template_path(p), "{p}");
        }
    }
}
