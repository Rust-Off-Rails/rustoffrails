//! `generate page <name> [--path /x] [--nav]`: a static public page (About, Pricing, Terms): a
//! handler in the shared `controllers/pages.rs`, its struct in `views/pages.rs`, its template,
//! the route (router, `paths::`, `ROUTES`), a sitemap entry and a test in `tests/app/pages_test.rs`.

use super::Flags;
use super::app::AppInfo;
use super::controller::nav_link;
use super::names::{self, Names};
use super::plan::Plan;
use super::resource::{check_free, routes_table};
use crate::util::{Fail, Result};

const CONTROLLER: &str = "//! Static pages (`generate page`): About, Pricing, Terms… Public, no data.\n\nuse axum::response::{IntoResponse, Response};\n\nuse crate::error::AppError;\nuse crate::views::{self, Ctx, pages};\n\n// samples: page actions\n";

const VIEWS: &str = "//! Page structs for the static pages (`generate page`).\n\nuse askama::Template;\n\nuse super::Ctx;\n// The layout links with `paths::`.\nuse crate::routes::paths;\n\n// samples: page views\n";

/// `generate page <name>`.
pub fn plan(info: &AppInfo, args: &[String], flags: &Flags) -> Result<Plan> {
    info.require_htmx("generate page")?;
    let n = Names::plain(&args[0])?;
    if args.len() > 1 {
        return Err(Fail::usage(
            "generate page takes one name (one page per run); options: --path /x --nav",
        ));
    }
    let name = &n.singular;
    let mut plan = Plan::new("page", name, args);
    let url = flags
        .recorded("path")
        .or_else(|| flags.value("path"))
        .unwrap_or_else(|| format!("/{}", name.replace('_', "-")));
    if !url.starts_with('/') || url.contains('{') || url.contains(char::is_whitespace) {
        return Err(Fail::usage(format!(
            "--path {url}: a page's path starts with / and takes no parameters"
        )));
    }
    plan.option("path", url.clone());
    if flags.recorded("path").is_none() {
        check_free(info, std::slice::from_ref(name), std::slice::from_ref(&url))?;
    }
    let view = format!("Pages{}", n.model);
    let title = names::human(name);
    let k = &info.krate;
    let cmd = plan.command();

    plan.shared("src/controllers/pages.rs", CONTROLLER);
    plan.insert_for(
        "src/controllers/pages.rs",
        "src/controllers/mod.rs",
        "controller mods",
        "pub mod pages;\n",
    );
    plan.insert(
        "src/controllers/pages.rs",
        "page actions",
        format!(
            "/// `GET {url}` (`{cmd}`)\n#[tracing::instrument(skip_all)]\npub async fn {name}(ctx: Ctx) -> Result<Response, AppError> {{\n    // APP: a description for search results and link previews.\n    Ok(views::render(pages::{view} {{\n        ctx: ctx.titled(\"{title}\"),\n    }})?\n    .into_response())\n}}\n\n"
        ),
    );
    plan.shared("src/views/pages.rs", VIEWS);
    plan.insert_for(
        "src/views/pages.rs",
        "src/views/mod.rs",
        "view mods",
        "pub mod pages;\n",
    );
    plan.insert(
        "src/views/pages.rs",
        "page views",
        format!(
            "/// `GET {url}`\n#[derive(Template)]\n#[template(path = \"pages/{name}.html\")]\npub struct {view} {{\n    pub ctx: Ctx,\n}}\n\n"
        ),
    );
    plan.create(
        format!("templates/pages/{name}.html"),
        format!(
            "{{% extends \"layouts/application.html\" %}}\n{{% block content %}}\n<article class=\"py-6\">\n  <h1 class=\"text-3xl font-semibold tracking-tight\">{title}</h1>\n  <p class=\"mt-4 text-gray-700 dark:text-gray-300\">APP: the {lower} page's words (templates/pages/{name}.html).</p>\n</article>\n{{% endblock %}}\n",
            lower = title.to_lowercase()
        ),
    );
    plan.insert(
        "src/routes.rs",
        "routes",
        format!(
            "// {title} (cargo offrails generate page {name})\napp = app.route(\"{url}\", get(crate::controllers::pages::{name}));\n\n"
        ),
    );
    plan.insert(
        "src/routes.rs",
        "paths",
        format!("pub fn {name}() -> String {{\n    \"{url}\".into()\n}}\n"),
    );
    plan.insert(
        "src/routes.rs",
        "routes table",
        routes_table(
            &[("GET".into(), url.clone(), format!("pages::{name}"))],
            "Public",
        ),
    );
    plan.insert(
        "src/controllers/meta.rs",
        "sitemap paths",
        format!("paths::{name}(),\n"),
    );
    let nav = flags.nav || flags.recorded("nav").as_deref() == Some("yes");
    plan.option("nav", if nav { "yes" } else { "no" });
    if nav {
        plan.insert(
            "templates/shared/_nav.html",
            "nav links",
            nav_link(name, &title),
        );
    }
    plan.shared(
        "tests/app/pages_test.rs",
        format!(
            "//! The static pages (`generate page`): each renders, publicly, and is in the sitemap.\n\nuse {k}::routes::paths;\n\nuse crate::support::TestApp;\n\n// samples: page tests\n"
        ),
    );
    plan.insert_for(
        "tests/app/pages_test.rs",
        "tests/app/main.rs",
        "test mods",
        "mod pages_test;\n",
    );
    plan.insert(
        "tests/app/pages_test.rs",
        "page tests",
        format!(
            "#[tokio::test]\nasync fn {name}_renders_publicly_and_is_in_the_sitemap() {{\n    let app = TestApp::new().await;\n    app.get(&paths::{name}())\n        .await\n        .assert_ok()\n        .assert_contains(\"<title>{title} · \")\n        .assert_text(\"{title}\");\n    app.get(\"/sitemap.xml\")\n        .await\n        .assert_ok()\n        .assert_contains(&format!(\"{{}}</loc>\", paths::{name}()));\n}}\n\n"
        ),
    );
    plan.next.push(format!(
        "write the page: templates/pages/{name}.html (and a `.described(\"…\")` in src/controllers/pages.rs)"
    ));
    plan.next.push(
        "bin/tailwindcss -i assets/app.css -o static/app.css --minify   # new classes (bin/dev does it live)".into(),
    );
    Ok(plan)
}
