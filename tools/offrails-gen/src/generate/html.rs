//! The HTML the generators write lives in two files, `files/html/resource.html` and
//! `files/html/pages.html`, one section per piece (`@@ <name>` starts one), with `__NAME__` tokens.
//! Swapping the markup (a component kit, other design tokens) is editing those files; the golden
//! files then show every changed template.

use super::Flags;
use super::app::AppInfo;
use super::plan::Plan;
use crate::util::{Fail, Result};

/// The layouts a generated page may extend (`--layout`, templates/layouts/<name>.html).
pub const LAYOUTS: [&str; 5] = ["application", "sidebar", "marketing", "auth", "docs"];

/// The layout the generated pages extend: `--layout <name>` (recorded as `page_layout`, so a re-run
/// and `destroy` see the same pages; `layout` is the file layout's version), default `application` (the top nav: guides/views.md#layouts).
pub fn layout(info: &AppInfo, flags: &Flags, plan: &mut Plan) -> Result<String> {
    let name = flags
        .recorded("page_layout")
        .or_else(|| flags.value("layout"))
        .unwrap_or_else(|| "application".to_owned());
    if !LAYOUTS.contains(&name.as_str()) {
        return Err(Fail::usage(format!(
            "--layout {name}: one of {}",
            LAYOUTS.join(", ")
        )));
    }
    if !info.exists(&format!("templates/layouts/{name}.html")) {
        return Err(Fail::usage(format!(
            "--layout {name}: this app has no templates/layouts/{name}.html (copy it from .offrails/template/app/templates/layouts/)"
        )));
    }
    // The pages call the component kit, with the field options and the helpers 0.7.3 added.
    let kit = info
        .read("templates/components/ui.html")
        .unwrap_or_default();
    let helpers = info.read("src/views/helpers.rs").unwrap_or_default();
    if !flags.replan && (!kit.contains("error_key") || !helpers.contains("fn error_key")) {
        return Err(Fail::usage(
            "the generated pages are built from the component kit, and this app's is missing or older than 0.7.3: copy templates/components/ (and its helpers in src/views/helpers.rs: same, error_key, Pager…) from .offrails/template/app/ (CHANGELOG 0.7.3, Polish and proof → App migration)",
        ));
    }
    if name != "application" {
        plan.option("page_layout", name.clone());
    }
    Ok(name)
}

const RESOURCE: &str = include_str!("files/html/resource.html");
const PAGES: &str = include_str!("files/html/pages.html");

fn section(file: &'static str, name: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in file.lines() {
        if let Some(h) = line.strip_prefix("@@ ") {
            if inside {
                break;
            }
            inside = h.trim() == name;
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(inside, "files/html: no section `{name}`");
    out
}

/// `text` with each `__KEY__` replaced by its value, in order.
pub fn fill(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = text.to_owned();
    for (k, v) in vars {
        out = out.replace(&format!("__{k}__"), v);
    }
    out
}

/// A section of `files/html/resource.html`, filled; with its final newline.
pub fn resource(name: &str, vars: &[(&str, &str)]) -> String {
    fill(&section(RESOURCE, name), vars)
}

/// The same, without the final newline (an attribute or a fragment inside a line).
pub fn resource_inline(name: &str, vars: &[(&str, &str)]) -> String {
    resource(name, vars).trim_end_matches('\n').to_owned()
}

/// A section of `files/html/pages.html`, filled.
pub fn pages(name: &str, vars: &[(&str, &str)]) -> String {
    fill(&section(PAGES, name), vars)
}

pub fn pages_inline(name: &str, vars: &[(&str, &str)]) -> String {
    pages(name, vars).trim_end_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_found_and_filled() {
        assert_eq!(
            resource("required", &[]),
            ", required = true\n",
            "a section keeps its leading space and gets its newline"
        );
        assert_eq!(
            resource_inline("index td", &[("S", "post"), ("COL", "title")]),
            "        <td>{{ post.title }}</td>"
        );
        assert!(resource("index", &[("S", "post"), ("P", "posts")]).contains("paths::new_post()"));
        assert!(
            pages("page", &[("TITLE", "About"), ("LAYOUT", "docs")])
                .contains("{% extends \"layouts/docs.html\" %}")
        );
    }
}
