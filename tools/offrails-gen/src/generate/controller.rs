//! `generate controller <name> [action[:/path]…] [--path /x]` (`rails generate controller`): one
//! GET page per action (`index` is `/<name>`, others `/<name>/<action>`, or the path given, with
//! `{params}`: `show:/blog/{slug}`), one handler file per action in `src/controllers/<name>/`,
//! its page struct and template, its routes file (`src/routes/<name>.rs`) and request tests.
//! With accounts the pages need a signed-in user (`--public`: anyone).

use super::Flags;
use super::app::AppInfo;
use super::feature::{self, Mount};
use super::html;
use super::names::{self, Names};
use super::plan::Plan;
use super::resource::RESERVED;
use crate::util::{Fail, Result};

/// One page action: its URL, `paths::` helper, page struct and template.
pub struct Action {
    pub name: String,
    pub url: String,
    pub params: Vec<String>,
    pub path_fn: String,
    pub view: String,
    pub template: String,
}

/// `index`, `summary`, `show:/blog/{slug}`; `--path` is the one action's path.
pub fn actions(n: &Names, raw: &[String], path: Option<&str>) -> Result<Vec<Action>> {
    let mut list: Vec<(String, Option<String>)> = raw
        .iter()
        .map(|a| match a.split_once(':') {
            Some((name, p)) => (names::snake(name), Some(p.to_owned())),
            None => (names::snake(a), None),
        })
        .collect();
    if list.is_empty() {
        list.push(("index".into(), None));
    }
    if let Some(p) = path {
        if list.len() != 1 {
            return Err(Fail::usage(
                "--path is the path of a controller's one action; with several, give each its own: show:/blog/{slug}",
            ));
        }
        if list[0].1.is_some() {
            return Err(Fail::usage(
                "the action has a path already (action:/path); drop --path",
            ));
        }
        list[0].1 = Some(p.to_owned());
    }
    let mut out: Vec<Action> = Vec::new();
    for (a, given) in list {
        if a.is_empty() || names::RUST_KEYWORDS.contains(&a.as_str()) || a == "new" || a == "mod" {
            return Err(Fail::usage(format!(
                "action '{a}' can't be a handler name (a Rust keyword, `new` or `mod`); pick another"
            )));
        }
        if out.iter().any(|o| o.name == a) {
            return Err(Fail::usage(format!("action '{a}' is given twice")));
        }
        let url = match &given {
            Some(p) => p.clone(),
            None if a == "index" => format!("/{}", n.kebab),
            None => format!("/{}/{}", n.kebab, a.replace('_', "-")),
        };
        let params = feature::check_path(&url)?;
        if params.iter().any(|p| p == "ctx" || p == "user") {
            return Err(Fail::usage(format!(
                "{url}: `ctx` and `user` are the handler's own names; call the parameter something else"
            )));
        }
        let path_fn = if a == "index" {
            n.singular.clone()
        } else {
            format!("{}_{a}", n.singular)
        };
        out.push(Action {
            view: format!("{}{}", n.model, names::camel(&a)),
            template: format!("{}/{a}.html", n.singular),
            url,
            params,
            path_fn,
            name: a,
        });
    }
    Ok(out)
}

/// `generate controller <name> [action…]`.
pub fn plan(info: &AppInfo, args: &[String], flags: &Flags) -> Result<Plan> {
    info.require_htmx("generate controller")?;
    let n = Names::plain(&args[0])?;
    if RESERVED.contains(&n.singular.as_str()) {
        return Err(Fail::usage(format!(
            "a controller named '{}' already exists in every app (or another generator owns it); pick another name",
            n.singular
        )));
    }
    let mut plan = Plan::new("controller", &n.singular, args);
    let layout = super::html::layout(info, flags, &mut plan)?;
    let path = flags.recorded("path").or_else(|| flags.value("path"));
    let acts = actions(&n, &args[1..], path.as_deref())?;
    if let Some(p) = &path {
        plan.option("path", p.clone());
    }
    let user = match flags.recorded("scope").as_deref() {
        Some(s) => s == "user",
        None => info.auth && !flags.public,
    };
    plan.option("scope", if user { "user" } else { "public" });
    if flags.recorded("scope").is_none() {
        feature::check_free(
            info,
            Some(&n.singular),
            &acts.iter().map(|a| a.path_fn.clone()).collect::<Vec<_>>(),
            &acts.iter().map(|a| a.url.clone()).collect::<Vec<_>>(),
        )?;
    }
    let nav = flags.nav || flags.recorded("nav").as_deref() == Some("yes");
    if nav && !acts[0].params.is_empty() {
        return Err(Fail::usage(format!(
            "--nav links to the first action, and {} takes parameters; link to it from a page that knows them",
            acts[0].url
        )));
    }
    let c = &n.singular;
    let cmd = plan.command();
    let k = &info.krate;

    // src/controllers/<c>/mod.rs, then one file per action.
    let mods: String = acts.iter().map(|a| format!("mod {};\n", a.name)).collect();
    let uses: String = acts
        .iter()
        .map(|a| format!("pub use {a}::{a};\n", a = a.name))
        .collect();
    plan.create(
        format!("src/controllers/{c}/mod.rs"),
        format!(
            "//! {}: {}, one file per action. Generated by `{cmd}`.\n//! APP: what these pages are for (product/features/{c}.md); load their data in each handler.\n\n{mods}\n{uses}",
            n.human,
            acts.iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    let mut views = format!(
        "//! Page structs for {}. Generated by `{cmd}`.\n\nuse askama::Template;\n\nuse super::Ctx;\n// The layout links with `paths::`.\nuse crate::routes::paths;\n\n",
        n.lower
    );
    let mut tests = format!(
        "//! {} (src/controllers/{c}/). Generated by `{cmd}`.\n//! APP: assert what each page shows, citing its AC (`/// AC-{c}-1`).\n\nuse {k}::routes::paths;\n\n{}use crate::support::TestApp;\n\n",
        n.human,
        if user { "use crate::factories;\n" } else { "" }
    );
    let mut paths = String::new();
    let mut mounts = Vec::new();
    let mut rows = Vec::new();
    for a in &acts {
        let title = if a.name == "index" {
            n.human.clone()
        } else {
            format!("{} {}", n.human, a.name.replace('_', " "))
        };
        let auth = if user {
            "    RequireUser(_user): RequireUser, // unscoped: reads nothing yet (APP: scope what it loads with the user)\n"
        } else {
            ""
        };
        let extract = feature::extractor(&a.params)
            .map(|e| format!("    {e},\n"))
            .unwrap_or_default();
        let fields: String = a.params.iter().map(|p| format!("        {p},\n")).collect();
        let axum_use = if a.params.is_empty() {
            "use axum::response::{IntoResponse, Response};\n".to_owned()
        } else {
            "use axum::{\n    extract::Path,\n    response::{IntoResponse, Response},\n};\n"
                .to_owned()
        };
        plan.create(
            format!("src/controllers/{c}/{}.rs", a.name),
            format!(
                "//! `GET {url}`. Generated by `{cmd}`.\n\n{axum_use}\n{auth_use}use crate::error::AppError;\nuse crate::views::{{self, Ctx, {c}::{view}}};\n\n/// `GET {url}`\n#[tracing::instrument(skip_all)]\npub async fn {name}(\n    ctx: Ctx,\n{auth}{extract}) -> Result<Response, AppError> {{\n    Ok(views::render({view} {{\n        ctx: ctx.titled(\"{title}\"){ni},\n{fields}    }})?\n    .into_response())\n}}\n",
                url = a.url,
                name = a.name,
                view = a.view,
                auth_use = if user {
                    "use crate::controllers::auth::RequireUser;\n"
                } else {
                    ""
                },
                ni = if user { ".noindex()" } else { "" }
            ),
        );
        let struct_fields: String = a
            .params
            .iter()
            .map(|p| {
                format!(
                    "    /// From the path (`{{{p}}}`): untrusted; Askama escapes it.\n    pub {p}: {},\n",
                    feature::param_type(p)
                )
            })
            .collect();
        views.push_str(&format!(
            "/// `GET {}`\n#[derive(Template)]\n#[template(path = \"{}\")]\npub struct {} {{\n    pub ctx: Ctx,\n{struct_fields}}}\n\n",
            a.url, a.template, a.view
        ));
        let shows: String = a
            .params
            .iter()
            .map(|p| html::pages_inline("param", &[("PARAM", p)]))
            .collect();
        let shows = if shows.is_empty() {
            String::new()
        } else {
            format!(" for{shows}")
        };
        plan.create(
            format!("templates/{}", a.template),
            html::pages(
                "action",
                &[
                    ("TITLE", &title),
                    ("SHOWS", &shows),
                    ("TEMPLATE", &a.template),
                    ("LAYOUT", &layout),
                ],
            ),
        );
        let setup = if user {
            "    let me = factories::user(app.db(), \"me@example.com\").await;\n    app.sign_in_as(&me).await;\n"
        } else {
            ""
        };
        let call = format!("paths::{}({})", a.path_fn, feature::sample_args(&a.params));
        let echo = if a.params.iter().any(|p| feature::param_type(p) == "String") {
            "\n        .assert_text(\"example\")"
        } else {
            ""
        };
        tests.push_str(&format!(
            "#[tokio::test]\nasync fn {name}_renders() {{\n    let app = TestApp::new().await;\n{setup}    app.get(&{call})\n        .await\n        .assert_ok()\n        .assert_text(\"{title}\"){echo};\n}}\n\n",
            name = a.name,
        ));
        if user {
            tests.push_str(&format!(
                "/// SECURITY: signed out, the page sends you to sign in.\n#[tokio::test]\nasync fn {name}_needs_a_signed_in_user() {{\n    let app = TestApp::new().await;\n    app.get(&{call})\n        .await\n        .assert_redirect_to(&paths::new_session());\n}}\n\n",
                name = a.name,
            ));
        }
        mounts.push(Mount::new(
            a.url.clone(),
            &[("get", &format!("crate::controllers::{c}::{}", a.name))],
        ));
        if !paths.is_empty() {
            paths.push('\n');
        }
        paths.push_str(&feature::helper(&a.path_fn, &a.url));
        rows.push((
            "GET".to_owned(),
            a.url.clone(),
            format!("{c}::{}", a.name),
            if user { "User" } else { "Public" }.to_owned(),
        ));
    }
    plan.insert(
        "src/controllers/mod.rs",
        "controller mods",
        format!("pub mod {c};\n"),
    );
    plan.create(
        format!("src/views/{c}.rs"),
        views.trim_end().to_owned() + "\n",
    );
    plan.insert("src/views/mod.rs", "view mods", format!("pub mod {c};\n"));
    feature::plan_into(
        &mut plan,
        &feature::Routes {
            module: c.clone(),
            what: format!("{} (`cargo offrails generate controller {c}`)", n.human),
            mounts,
            paths,
            rows,
            shared: false,
        },
    );
    plan.option("nav", if nav { "yes" } else { "no" });
    if nav {
        plan.insert(
            "templates/shared/_nav.html",
            "nav links",
            nav_link(&acts[0].path_fn, &n.human),
        );
    }
    if user {
        for a in acts.iter().filter(|a| !a.params.is_empty()) {
            not_tenant_row(info, &mut plan, flags, &a.url, &format!("{c}_test"));
        }
    }
    plan.create(
        format!("tests/app/{c}_test.rs"),
        tests.trim_end().to_owned() + "\n",
    );
    plan.insert("tests/app/main.rs", "test mods", format!("mod {c}_test;\n"));
    plan.next.push(format!(
        "fill each page: load its data in src/controllers/{c}/<action>.rs (scoped to the user), add fields to its struct in src/views/{c}.rs, write templates/{c}/"
    ));
    plan.next.push(
        "bin/tailwindcss -i assets/app.css -o static/app.css --minify   # new classes (bin/dev does it live)".into(),
    );
    plan.next.push(format!(
        "commit now (git add -A && git commit -m \"generate controller {c}\"), before you change anything"
    ));
    Ok(plan)
}

/// A signed-in page with a path parameter isn't a tenant's record: the tenancy sweep (which signs
/// in and swaps ids) is told so, when the suite is the generators'.
pub fn not_tenant_row(info: &AppInfo, plan: &mut Plan, flags: &Flags, url: &str, test: &str) {
    let tenancy = info.read("tests/app/tenancy_test.rs");
    let ours = tenancy
        .as_deref()
        .is_some_and(|t| t.contains("// samples: not tenant ids"));
    let key = format!("tenancy_row {url}");
    if ours || flags.recorded(&key).is_some() {
        plan.option(&key, "yes");
        plan.insert(
            "tests/app/tenancy_test.rs",
            "not tenant ids",
            format!("(\"{url}\", \"a page, not a tenant's record: {test}\"),\n"),
        );
    } else if tenancy.is_some() {
        plan.next.push(format!(
            "tests/app/tenancy_test.rs: list (\"{url}\", \"a page, not a record: {test}\") in NOT_TENANT_IDS (its parameter isn't a tenant's id)"
        ));
    }
}

/// A link in the main nav (`templates/shared/_nav.html`; full paths: partials resolve names in
/// each including struct's module).
pub fn nav_link(path_fn: &str, label: &str) -> String {
    html::resource("nav link", &[("PATH_FN", path_fn), ("LABEL", label)])
}
