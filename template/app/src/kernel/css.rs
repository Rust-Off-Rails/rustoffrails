//! What the templates ask of the CSS. Tailwind only emits the classes it saw when it last ran, so
//! a template edited while no watcher ran renders with silently missing styles (a `dark:` partner
//! that does nothing). Used by the theme guard tests, `/dev/introspect.json` and the development
//! CSS banner. Spec: .offrails/guides/views.md#dark-mode, test-standards.md#11-theme-and-css-guards

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Rendered with static/app.css. Kernel pages and mailers carry their own styles.
pub const TEMPLATES: &str = "templates";
pub const SKIP_DIRS: &[&str] = &["templates/kernel", "templates/mailers"];
/// The component layer: `@apply` lists follow the same color rules as class attributes.
pub const CSS_SOURCE: &str = "assets/app.css";
pub const CSS_BUILT: &str = "static/app.css";
/// Classes that are hooks for JS, htmx or tests, never styles. Each needs its reason.
pub const HOOK_CLASSES: &[(&str, &str)] = &[(
    "errors",
    "the error summary's hook (shared/_errors.html, errors/_invalid.html) for tests and scripts",
)];

/// Template classes with no selector in the built CSS, as "file:line: class".
/// `None` when the CSS isn't built (nothing to compare).
pub fn missing_classes() -> Option<Vec<String>> {
    let css = fs::read_to_string(CSS_BUILT).ok()?;
    let mut missing = Vec::new();
    for (at, tokens) in class_lists(false) {
        for token in tokens {
            if !HOOK_CLASSES.iter().any(|(c, _)| *c == token) && !has_selector(&css, &token) {
                missing.push(format!("{at}: {token}"));
            }
        }
    }
    Some(missing)
}

/// Tailwind's selector for a class: `CSS.escape` rules (`md:w-1/2` → `md\:w-1\/2`, `2xl:` → `\32 xl\:`).
pub fn css_escape(class: &str) -> String {
    let mut out = String::new();
    for (i, c) in class.chars().enumerate() {
        if i == 0 && c.is_ascii_digit() {
            out.push_str(&format!("\\3{c} "));
        } else if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

/// `.class` followed by something that ends the identifier (`.p-4` doesn't count for `.p-40`).
pub fn has_selector(css: &str, class: &str) -> bool {
    let selector = format!(".{}", css_escape(class));
    css.match_indices(&selector).any(|(i, m)| {
        css[i + m.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '\\')))
    })
}

/// Every `class="…"` in the templates (and, with `with_apply`, every `@apply …;` in the CSS
/// source) as ("file:line", tokens). Askama tags inside an attribute are dropped, so both
/// branches of an `{% if %}` form one list.
pub fn class_lists(with_apply: bool) -> Vec<(String, Vec<String>)> {
    let mut files = Vec::new();
    html_files(Path::new(TEMPLATES), &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };
        let mut offset = 0;
        while let Some(i) = src[offset..].find("class=\"") {
            let start = offset + i + 7;
            let value = attribute_value(&src[start..]);
            let line = src[..start].lines().count().max(1);
            out.push((
                format!("{}:{line}", file.display()),
                tokens(&strip_askama(value)),
            ));
            offset = start + value.len();
        }
    }
    if with_apply && let Ok(css) = fs::read_to_string(CSS_SOURCE) {
        for (n, line) in css.lines().enumerate() {
            if let Some(i) = line.find("@apply ") {
                let list = line[i + 7..].split(';').next().unwrap_or("");
                out.push((format!("{CSS_SOURCE}:{}", n + 1), tokens(list)));
            }
        }
    }
    out
}

fn html_files(dir: &Path, out: &mut Vec<PathBuf>) {
    html_files_under(Path::new(""), dir, out);
}

/// `html_files` for templates under `root` (`SKIP_DIRS` are relative to it).
fn html_files_under(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if SKIP_DIRS
            .iter()
            .any(|s| path.strip_prefix(root).is_ok_and(|p| p.starts_with(s)))
        {
            continue;
        }
        if path.is_dir() {
            html_files_under(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

/// The attribute value up to its closing quote, skipping quotes inside `{{ }}` / `{% %}`.
fn attribute_value(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut depth = 0;
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'{', Some(b'{' | b'%')) => (depth, i) = (depth + 1, i + 1),
            (b'}' | b'%', Some(b'}')) if depth > 0 => (depth, i) = (depth - 1, i + 1),
            (b'"', _) if depth == 0 => return &s[..i],
            _ => {}
        }
        i += 1;
    }
    s
}

/// Drops `{{ … }}`, `{% … %}` and `{# … #}`, keeping the text between them.
pub fn strip_askama(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        let open = &rest[i..];
        let close = match open.as_bytes().get(1) {
            Some(b'{') => "}}",
            Some(b'%') => "%}",
            Some(b'#') => "#}",
            _ => {
                out.push_str(&rest[..=i]);
                rest = &rest[i + 1..];
                continue;
            }
        };
        out.push_str(&rest[..i]);
        out.push(' ');
        rest = open.find(close).map_or("", |j| &open[j + 2..]);
    }
    out.push_str(rest);
    out
}

fn tokens(list: &str) -> Vec<String> {
    list.split_whitespace().map(str::to_owned).collect()
}

/// The development CSS banner: the CSS is missing, or stale (template classes with no rule).
///
/// Every development page asks, and the scan reads every template and the built CSS (~85 ms in a
/// grown app: most of the CPU under load). So the answer is cached until a template or the CSS
/// changes: size and mtime of `static/app.css`, `assets/app.css` and every template, a few
/// dozen `stat` calls per page instead of reading them all.
pub fn css_warning() -> Option<String> {
    cached_warning(&WARNING, inputs_key(Path::new(".")), scan_css_warning)
}

/// The last answer and the inputs it was computed from.
type Cached = std::sync::Mutex<Option<(u64, Option<String>)>>;
static WARNING: Cached = std::sync::Mutex::new(None);

fn cached_warning(
    cache: &Cached,
    key: u64,
    compute: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((k, answer)) = cache.as_ref()
        && *k == key
    {
        return answer.clone();
    }
    let answer = compute();
    *cache = Some((key, answer.clone()));
    answer
}

/// What the warning depends on, under `root`: which files exist, their sizes and mtimes.
fn inputs_key(root: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut files = Vec::new();
    html_files_under(root, &root.join(TEMPLATES), &mut files);
    files.sort();
    files.push(root.join(CSS_BUILT));
    files.push(root.join(CSS_SOURCE));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for f in files {
        f.hash(&mut hasher);
        if let Ok(meta) = fs::metadata(&f) {
            meta.len().hash(&mut hasher);
            meta.modified().ok().hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn scan_css_warning() -> Option<String> {
    if let Some(problem) = crate::kernel::assets::app_css_problem() {
        return Some(problem);
    }
    let missing = missing_classes()?;
    let first = missing.first()?;
    Some(format!(
        "{CSS_BUILT} is stale: {} template class(es) have no CSS rule (first: {first}). bin/dev rebuilds it.",
        missing.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_css_warning_is_recomputed_only_when_a_template_or_the_css_changes() {
        let root = std::env::temp_dir().join(format!("offrails-css-{}", std::process::id()));
        let page = root.join(TEMPLATES).join("posts").join("index.html");
        fs::create_dir_all(page.parent().unwrap()).unwrap();
        fs::create_dir_all(root.join("static")).unwrap();
        fs::write(&page, r#"<p class="p-4">"#).unwrap();
        fs::write(root.join(CSS_BUILT), ".p-4{padding:1rem}").unwrap();

        let key = inputs_key(&root);
        assert_eq!(inputs_key(&root), key, "nothing changed");
        let calls = std::cell::Cell::new(0);
        let compute = || {
            calls.set(calls.get() + 1);
            None
        };
        let cache = Cached::new(None);
        cached_warning(&cache, key, compute);
        cached_warning(&cache, key, compute);
        assert_eq!(calls.get(), 1, "the same inputs reuse the answer");

        fs::write(&page, r#"<p class="p-4 dark:bg-gray-900">"#).unwrap();
        assert_ne!(inputs_key(&root), key, "an edited template changes the key");
        cached_warning(&cache, inputs_key(&root), compute);
        assert_eq!(calls.get(), 2, "and recomputes");

        let key = inputs_key(&root);
        fs::write(
            root.join(CSS_BUILT),
            ".p-4{padding:1rem}.dark\\:bg-gray-900{}",
        )
        .unwrap();
        assert_ne!(inputs_key(&root), key, "a rebuilt CSS changes the key");
        let key = inputs_key(&root);
        fs::write(root.join(TEMPLATES).join("new.html"), "<p>").unwrap();
        assert_ne!(inputs_key(&root), key, "a new template changes the key");
        let key = inputs_key(&root);
        let mailer = root.join("templates/mailers/welcome.html");
        fs::create_dir_all(mailer.parent().unwrap()).unwrap();
        fs::write(&mailer, "<p>").unwrap();
        assert_eq!(
            inputs_key(&root),
            key,
            "mailer templates are not checked, so not keyed"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
