//! Small helpers: errors, paths, text files, dates, the AGENTS.md header and docs/decisions.md.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// A refusal or failure the user can act on. Exit status 1 (2 for usage errors).
#[derive(Debug)]
pub struct Fail {
    pub code: i32,
    pub message: String,
}

impl Fail {
    pub fn new(message: impl Into<String>) -> Self {
        Fail {
            code: 1,
            message: message.into(),
        }
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Fail {
            code: 2,
            message: message.into(),
        }
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Self {
        Fail::new(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Fail>;

/// `io::Error` with the path it happened on.
pub fn ctx<T>(r: io::Result<T>, what: &str, path: &Path) -> Result<T> {
    r.map_err(|e| Fail::new(format!("{what} {}: {e}", path.display())))
}

/// The framework root this binary was built from (`<fw>/tools/offrails-gen` → `<fw>`), unless
/// `--framework` or `OFFRAILS_FW` names another.
pub fn framework_root(explicit: Option<&str>) -> Result<PathBuf> {
    let root = match explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("OFFRAILS_FW").ok())
    {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."),
    };
    let root = ctx(
        fs::canonicalize(&root),
        "can't find the framework at",
        &root,
    )?;
    if !root.join("VERSION").is_file() || !root.join("template").join("app").is_dir() {
        return Err(Fail::new(format!(
            "{} is not a Rust off Rails framework with a template/ (no VERSION or template/app)",
            root.display()
        )));
    }
    Ok(strip_verbatim(root))
}

/// Windows' canonicalize returns `\\?\C:\…`, which some tools (and messages) don't like.
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

pub fn framework_version(fw: &Path) -> Result<String> {
    let v = ctx(
        fs::read_to_string(fw.join("VERSION")),
        "reading",
        &fw.join("VERSION"),
    )?;
    Ok(v.lines().next().unwrap_or("").trim().to_owned())
}

/// A relative path written with `/` (sample.toml, messages) as a native path.
pub fn rel(base: &Path, slash_path: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in slash_path.split('/').filter(|s| !s.is_empty() && *s != ".") {
        p.push(part);
    }
    p
}

/// Every file under `dir`, as `/`-separated relative paths, sorted (stable on every OS).
pub fn files_under(dir: &Path) -> Result<Vec<String>> {
    files_under_except(dir, &|_| false)
}

/// Like `files_under`, never descending into a directory `skip` names (by its relative path),
/// so `.git` and build output aren't even walked.
pub fn files_under_except(dir: &Path, skip: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(dir, "", skip, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn walk(
    dir: &Path,
    prefix: &str,
    skip: &dyn Fn(&str) -> bool,
    out: &mut Vec<String>,
) -> Result<()> {
    for entry in ctx(fs::read_dir(dir), "reading", dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if !skip(&rel) {
                walk(&entry.path(), &rel, skip, out)?;
            }
        } else if ty.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

/// Text we may rewrite: valid UTF-8 with no NUL byte. Images and other binaries are copied as is.
pub fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// Copy one file, creating parent directories and keeping the permission bits (bin/ scripts).
pub fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        ctx(fs::create_dir_all(parent), "creating", parent)?;
    }
    ctx(fs::copy(from, to), "copying to", to)?;
    Ok(())
}

/// Write text, keeping the file's permissions when it exists.
pub fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        ctx(fs::create_dir_all(parent), "creating", parent)?;
    }
    ctx(fs::write(path, text), "writing", path)
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        ctx(fs::create_dir_all(parent), "creating", parent)?;
    }
    ctx(fs::write(path, bytes), "writing", path)
}

pub fn read_text(path: &Path) -> Result<String> {
    ctx(fs::read_to_string(path), "reading", path)
}

/// Today's date in UTC as YYYY-MM-DD (dates are UTC everywhere in an app).
pub fn today_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    date_of(secs)
}

/// The UTC date (YYYY-MM-DD) of a Unix time.
pub fn date_of(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The line ending a file already uses (CRLF on a Windows checkout without .gitattributes).
pub fn newline_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Append one dated line to docs/decisions.md (created from the blueprint shape when missing).
pub fn log_decision(app: &Path, line: &str) -> Result<()> {
    let path = rel(app, "docs/decisions.md");
    let mut text = if path.is_file() {
        read_text(&path)?
    } else {
        "# Decisions\n\n".to_owned()
    };
    let nl = newline_of(&text);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str(nl);
    }
    text.push_str(&format!("{}: {line}{nl}", today_utc()));
    write_text(&path, &text)
}

/// Replace the value of one `KEY:` line in the AGENTS.md header block, keeping its alignment.
/// Returns whether the line was found.
pub fn set_header(text: &mut String, key: &str, value: &str) -> bool {
    let nl = newline_of(text);
    let mut found = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\r', '\n']);
        if !found && bare.starts_with(&format!("{key}:")) {
            let width = 16usize.max(key.len() + 2);
            out.push_str(&format!("{:<width$}{value}{nl}", format!("{key}:")));
            found = true;
        } else {
            out.push_str(line);
        }
    }
    *text = out;
    found
}

/// The value after `KEY:` in the header, if any.
pub fn header_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}:")))
        .map(|v| v.trim().to_owned())
}

/// The app's package name: the first `name = "…"` after `[package]` in Cargo.toml.
pub fn package_name(app: &Path) -> Result<String> {
    let path = app.join("Cargo.toml");
    let text = ctx(fs::read_to_string(&path), "reading", &path).map_err(|_| {
        Fail::usage(format!(
            "{} has no Cargo.toml (not an app root; pass --app <dir>)",
            app.display()
        ))
    })?;
    let mut in_package = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if in_package
            && let Some(rest) = t.strip_prefix("name")
            && let Some(v) = rest.trim_start().strip_prefix('=')
        {
            return Ok(v.trim().trim_matches('"').to_owned());
        }
    }
    Err(Fail::new(format!(
        "can't read the package name from {}",
        path.display()
    )))
}

/// Run a program in `dir`, streaming its output. Errors name the command.
pub fn run(dir: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| Fail::new(format!("can't run {program}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Fail::new(format!(
            "`{program} {}` failed ({status})",
            args.join(" ")
        )))
    }
}

/// Whether `program` can be started at all (cargo, git).
pub fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_header_keeps_alignment_and_line_endings() {
        let mut t = "```\r\nAPP:            x\r\nEMAIL:          none\r\n```\r\n".to_owned();
        assert!(set_header(&mut t, "EMAIL", "dev: local mailbox"));
        assert_eq!(
            t,
            "```\r\nAPP:            x\r\nEMAIL:          dev: local mailbox\r\n```\r\n"
        );
        assert!(!set_header(&mut t, "NOPE", "y"));
        assert_eq!(
            header_value(&t, "EMAIL").as_deref(),
            Some("dev: local mailbox")
        );
    }

    #[test]
    fn rel_builds_native_paths() {
        let p = rel(Path::new("app"), "src/kernel/mod.rs");
        assert_eq!(
            p,
            Path::new("app").join("src").join("kernel").join("mod.rs")
        );
    }

    #[test]
    fn today_is_a_date() {
        let d = today_utc();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }
}
