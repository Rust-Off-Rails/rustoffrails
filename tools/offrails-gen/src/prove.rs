//! `cargo offrails prove`: proof that what was built works, for the human and the next agent
//! (workflow.md#definition-of-done). It starts nothing: it needs the running app (`BASE`, the URL
//! bin/dev printed) and runs `.offrails/tools/browser-smoke/prove.mjs` with Node and the installed
//! Chrome over the DevTools Protocol (no npm install). That script visits every GET page in
//! docs/routes.md (plus SMOKE_PATHS, minus SMOKE_SKIP), signed in as the demo account where a page
//! needs it, takes desktop and mobile screenshots in light and dark, fails on console errors,
//! uncaught exceptions, CSP violations, axe violations, broken internal links and horizontal
//! overflow at 390px, diffs each screenshot against the previous run, and writes
//! docs/proof/PROOF.md: every acceptance criterion → the tests that cite it (and their last
//! verify result) → the pages and screenshots it touches.
//!
//! Exit status: 0 proven, 1 a check failed, 2 usage (no BASE), 77 skipped (no Node or Chrome).

use std::path::Path;
use std::process::Command;

use crate::util::{Fail, Result};

pub struct Options {
    /// Visual changes over the threshold fail the run (they are reported either way).
    pub strict: bool,
    /// Keep the PNGs in git (docs/proof/.gitignore ignores them by default).
    pub commit_screenshots: bool,
}

pub const SKIP_EXIT: i32 = 77;

/// Node's major version, if `node` runs.
fn node_major() -> Option<u32> {
    let out = Command::new("node").arg("--version").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout);
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

pub fn run(fw: &Path, app: &Path, o: &Options) -> Result<()> {
    if std::env::var("BASE").map_or(true, |b| b.trim().is_empty()) {
        return Err(Fail::usage(
            "prove needs the running app: BASE=<the URL bin/dev printed> cargo offrails prove (it starts nothing itself)",
        ));
    }
    let script = fw.join("tools").join("browser-smoke").join("prove.mjs");
    if !script.is_file() {
        return Err(Fail::new(format!(
            "{} is missing: re-vendor the framework (offrails-gen vendor --framework <fw>)",
            script.display()
        )));
    }
    match node_major() {
        Some(v) if v >= 18 => {}
        found => {
            println!(
                "skip: prove needs Node >= 18 (https://nodejs.org); found {}",
                found.map_or("none".to_owned(), |v| format!("v{v}"))
            );
            return Err(Fail {
                code: SKIP_EXIT,
                message: "skipped: no Node >= 18".into(),
            });
        }
    }
    let mut cmd = Command::new("node");
    cmd.arg(&script).current_dir(app);
    if o.strict {
        cmd.env("PROVE_STRICT", "1");
    }
    if o.commit_screenshots {
        cmd.env("PROVE_COMMIT_SCREENSHOTS", "1");
    }
    let status = cmd
        .status()
        .map_err(|e| Fail::new(format!("can't run node: {e}")))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(SKIP_EXIT) => Err(Fail {
            code: SKIP_EXIT,
            message: "skipped (see above)".into(),
        }),
        Some(2) => Err(Fail::usage("prove: usage (see above)")),
        _ => Err(Fail::new(
            "prove: some pages failed their checks; docs/proof/PROOF.md lists them",
        )),
    }
}
