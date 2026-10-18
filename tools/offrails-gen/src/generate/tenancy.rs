//! The tenancy suites (.offrails/guides/test-standards.md §4) for scoped resources:
//! `tests/app/guard_test.rs` (verbatim) and `tests/app/tenancy_test.rs` (with anchors each
//! scoped resource fills in). An app that already has its own `tenancy_test.rs` keeps it: the
//! generator prints what to teach it.

use super::Flags;
use super::app::AppInfo;
use super::model::ModelSpec;
use super::plan::Plan;
use super::routes_md;

const GUARD: &str = include_str!("files/guard_test.rs.txt");

/// The guide's guard test, plus the handler files the app's parts installed: their scoping is the
/// part's own suite's (the samples don't mark their lines yet), so a generated guard doesn't fail
/// on code the app didn't write.
fn guard_file(info: &AppInfo) -> String {
    let mut files: Vec<String> = Vec::new();
    for part in crate::parts::installed(&info.dir) {
        let root = info
            .dir
            .join(".offrails/template/parts")
            .join(&part)
            .join("files");
        for f in crate::util::files_under(&root).unwrap_or_default() {
            let handler = [
                "src/controllers/",
                "src/channels/",
                "src/graphql/",
                "src/api/",
            ]
            .iter()
            .any(|d| f.starts_with(d));
            if handler && f.ends_with(".rs") && !files.contains(&f) {
                files.push(f);
            }
        }
    }
    files.sort();
    let list: String = files.iter().map(|f| format!("    \"{f}\",\n")).collect();
    GUARD
        .replace(
            "const MARKER: &str = \"// unscoped:\";\n",
            &format!(
                "const MARKER: &str = \"// unscoped:\";\n\n/// Handler files the app's parts installed (`cargo offrails parts`): the part's own suite tests\n/// their scoping. APP: drop a line once you've reviewed that file's lookups or marked them.\nconst PART_FILES: &[&str] = &[\n{list}];\n"
            ),
        )
        .replace(
            "    for file in &files {\n        let src = fs::read_to_string(file).expect(\"read source\");\n        for (n, line) in src.lines().enumerate() {\n            let code = line.trim_start();",
            "    for file in &files {\n        let rel = file.strip_prefix(root).unwrap_or(file);\n        if PART_FILES\n            .iter()\n            .any(|p| rel == std::path::Path::new(p))\n        {\n            continue;\n        }\n        let src = fs::read_to_string(file).expect(\"read source\");\n        for (n, line) in src.lines().enumerate() {\n            let code = line.trim_start();",
        )
}

fn tenancy_file(info: &AppInfo, own_prefix: &str) -> String {
    let k = &info.krate;
    // Id routes the parts already have (sessions, sockets…): not a tenant's record; their own
    // suites test them.
    let routes = routes_md::all(&info.dir);
    let mut not_tenant = String::new();
    let mut seen: Vec<String> = Vec::new();
    for r in routes {
        if r.access == "user"
            && r.path.contains('{')
            && !r.path.starts_with(own_prefix)
            && !seen.contains(&r.path)
        {
            not_tenant.push_str(&format!(
                "    (\"{}\", \"not a record of a tenant-owned model ({}): its part's suite tests it\"),\n",
                r.path, r.handler
            ));
            seen.push(r.path);
        }
    }
    format!(
        "//! Tenant isolation suite (.offrails/guides/test-standards.md §4), generated from `ROUTES`.\n//! Written by `cargo offrails generate`; each scoped resource adds itself at the `samples:`\n//! anchors. Keep its shape when you edit it: a new route with an id fails until `fill` knows it.\n\nuse axum::http::StatusCode;\nuse {k}::models::User;\nuse {k}::routes::{{Access, ROUTES}};\n\nuse crate::factories;\nuse crate::support::TestApp;\n\n// (1) One record of every tenant-owned model.\nstruct Tenant {{\n    user: User,\n    // samples: tenant fields\n}}\n\nasync fn tenant(app: &TestApp, name: &str) -> Tenant {{\n    let db = app.db();\n    let user = factories::user(db, &format!(\"{{name}}@example.com\")).await;\n    // samples: tenant records\n    Tenant {{\n        user,\n        // samples: tenant init\n    }}\n}}\n\n// (2) (method, route) → status when A uses B's ids, and the AC that says so. Everything else: 404.\n// Strict isolation unless a spec shares something.\nconst EXPECTED: &[(&str, &str, StatusCode, &str)] = &[\n    // samples: tenant expected\n];\n\n// (3) Ids that aren't a tenant's record, and where they're tested instead.\nconst NOT_TENANT_IDS: &[(&str, &str)] = &[\n{not_tenant}    // samples: not tenant ids\n];\n\n// (4) Whose id each placeholder is. Panics on one it doesn't know.\nfn fill(template: &str, t: &Tenant) -> String {{\n    let mut path = template.to_owned();\n    // samples: tenant fill\n    assert!(\n        !path.contains('{{'),\n        \"{{template}} takes an id this suite doesn't know. Teach `fill` in \\\n         tests/app/tenancy_test.rs whose record it is, or list it in NOT_TENANT_IDS with a reason.\"\n    );\n    path\n}}\n\nfn in_scope(template: &str, access: Access) -> bool {{\n    access == Access::User\n        && template.contains('{{')\n        && !NOT_TENANT_IDS.iter().any(|(t, _)| *t == template)\n}}\n\n// (5) A request that would succeed if it were allowed: the lookup is scoped before the body is\n// read, so an empty form is enough.\nasync fn call(app: &TestApp, method: &str, template: &str, path: &str) -> StatusCode {{\n    match method {{\n        \"GET\" => app.get(path).await.status,\n        \"POST\" => app.post_form(path, &[]).await.status,\n        m => panic!(\"{{m}} {{template}}: teach `call` this method\"),\n    }}\n}}\n\n#[tokio::test]\nasync fn another_tenants_ids_answer_404_or_their_specd_status_on_every_route() {{\n    let app = TestApp::new().await;\n    let a = tenant(&app, \"alice\").await;\n    let b = tenant(&app, \"bob\").await;\n    app.sign_in_as(&a.user).await;\n    let mut checked = 0;\n    for (method, template, handler, access) in ROUTES {{\n        if !in_scope(template, *access) {{\n            continue;\n        }}\n        let (want, why) = EXPECTED\n            .iter()\n            .find(|(m, t, _, _)| m == method && t == template)\n            .map(|(_, _, s, why)| (*s, *why))\n            .unwrap_or((StatusCode::NOT_FOUND, \"B's records don't exist for A\"));\n        let got = call(&app, method, template, &fill(template, &b)).await;\n        assert_eq!(\n            got, want,\n            \"{{method}} {{template}} ({{handler}}) with another tenant's ids answered \\\n            {{got}}, want {{want}} ({{why}}). Look records up with `<model>::find_for(db, &user, id)`.\"\n        );\n        checked += 1;\n    }}\n    assert!(checked > 0, \"no id route was checked; is ROUTES complete?\");\n    // Nothing of B's changed.\n    // samples: tenant unchanged\n}}\n\n#[tokio::test]\nasync fn every_expected_entry_is_a_real_route() {{\n    for (method, template, _, why) in EXPECTED {{\n        assert!(\n            ROUTES\n                .iter()\n                .any(|(m, t, _, a)| m == method && t == template && in_scope(t, *a)),\n            \"EXPECTED lists {{method}} {{template}} ({{why}}), which is not an in-scope route\"\n        );\n    }}\n    for (template, why) in NOT_TENANT_IDS {{\n        assert!(\n            ROUTES.iter().any(|(_, t, _, _)| t == template),\n            \"NOT_TENANT_IDS lists {{template}} ({{why}}), which is not a route\"\n        );\n    }}\n}}\n\n#[tokio::test]\nasync fn own_ids_work_so_the_404s_are_not_a_blanket() {{\n    let app = TestApp::new().await;\n    let a = tenant(&app, \"alice\").await;\n    app.sign_in_as(&a.user).await;\n    for (method, template, _, access) in ROUTES {{\n        if *method == \"GET\" && in_scope(template, *access) {{\n            let got = call(&app, method, template, &fill(template, &a)).await;\n            assert!(\n                got.is_success() || got.is_redirection(),\n                \"GET {{template}} with A's own ids answered {{got}}\"\n            );\n        }}\n    }}\n}}\n"
    )
}

/// The suites, and this resource's place in them.
pub fn plan_into(info: &AppInfo, plan: &mut Plan, spec: &ModelSpec, flags: &Flags) {
    let n = &spec.names;
    let k = &info.krate;
    let own = format!("/{}/", n.kebab);
    // Whose file it is is decided once, at the first generation, and recorded: a suite the app
    // already had is never the generator's to delete.
    let whose = |path: &str, key: &str| -> String {
        flags.recorded(key).unwrap_or_else(|| {
            if info.exists(path) && !flags.manifest_shared.contains(path) {
                "app".into()
            } else {
                "gen".into()
            }
        })
    };
    let guard = whose("tests/app/guard_test.rs", "guard");
    let tenancy = whose("tests/app/tenancy_test.rs", "tenancy");
    plan.option("guard", guard.clone());
    plan.option("tenancy", tenancy.clone());
    let ours = tenancy == "gen"
        || info
            .read("tests/app/tenancy_test.rs")
            .is_some_and(|t| t.contains("// samples: tenant fill"));
    if guard == "gen" {
        plan.shared("tests/app/guard_test.rs", guard_file(info));
        plan.insert_for(
            "tests/app/guard_test.rs",
            "tests/app/main.rs",
            "test mods",
            "mod guard_test;\n",
        );
    }
    if !ours {
        plan.next.push(format!(
            "tests/app/tenancy_test.rs is your own: add a {m} to `Tenant` (factories::{s}(db, &user)), teach `fill` \
             that `{own}{{id}}` is `t.{s}.id`, and assert B's {lower} is unchanged",
            m = n.model,
            s = n.singular,
            lower = n.lower
        ));
        return;
    }
    let path = "tests/app/tenancy_test.rs";
    if tenancy == "gen" {
        plan.shared(path, tenancy_file(info, &own));
        plan.insert_for(
            path,
            "tests/app/main.rs",
            "test mods",
            "mod tenancy_test;\n",
        );
    }
    let s = &n.singular;
    plan.insert(
        path,
        "tenant fields",
        format!("{s}: {k}::models::{},\n", n.model),
    );
    plan.insert(
        path,
        "tenant records",
        format!("let {s} = factories::{s}(db, &user).await;\n"),
    );
    plan.insert(path, "tenant init", format!("{s},\n"));
    plan.insert(
        path,
        "tenant fill",
        format!(
            "if template.starts_with(\"{own}\") {{\n    path = path.replace(\"{{id}}\", &t.{s}.id.to_string());\n}}\n"
        ),
    );
    plan.insert(
        path,
        "tenant unchanged",
        format!(
            "assert_eq!(\n    factories::reload_{s}(app.db(), b.{s}.id).await.as_ref(),\n    Some(&b.{s})\n);\n"
        ),
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_guard_template_has_the_lines_part_exemptions_hook_into() {
        assert!(super::GUARD.contains("const MARKER: &str = \"// unscoped:\";\n"));
        assert!(super::GUARD.contains("    for file in &files {\n        let src = fs::read_to_string(file).expect(\"read source\");\n        for (n, line) in src.lines().enumerate() {\n            let code = line.trim_start();"));
    }
}
