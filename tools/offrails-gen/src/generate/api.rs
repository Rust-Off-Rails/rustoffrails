//! `generate api <name> field:type…`: a versioned JSON resource under `/api/v1` per
//! guides/api.md: bearer tokens (`ApiUser`, scopes), `ApiJson` bodies with JSON 422s,
//! `{"data": …}` envelopes with `page` info, 201/204, another user's record 404, OpenAPI entries
//! (utoipa) checked against docs/openapi.json, and request tests. The first run also writes the
//! API kernel: the `api_tokens` table and model, `controllers/api.rs`, `GET /api/v1/me`,
//! `GET /api/openapi.json`, `app task tokens`, and its tests. The model is generated too,
//! unless `generate model|resource` already made it (then its fields are reused).

use super::Flags;
use super::app::AppInfo;
use super::feature::{self, Mount};
use super::fields::{Field, Kind};
use super::manifest::Manifest;
use super::migration;
use super::model::{ModelSpec, Scope};
use super::names::Names;
use super::plan::Plan;
use crate::util::{Fail, Result};

const MOD: &str = include_str!("files/api_mod.rs.txt");
const TOKEN_MODEL: &str = include_str!("files/api_token_model.rs.txt");
const TOKEN_MIGRATION: &str = include_str!("files/api_token_migration.rs.txt");
const TOKEN_TASK: &str = include_str!("files/api_token_task.rs.txt");
const TESTS: &str = include_str!("files/api_test.rs.txt");
const KERNEL: &str = "src/controllers/api.rs";

/// The kernel's migration stamp: the one already in the app, else a new one.
fn kernel_stamp(info: &AppInfo, flags: &Flags) -> String {
    if let Some(s) = flags.recorded("api_stamp") {
        return s;
    }
    let found = std::fs::read_dir(info.path("migration/src"))
        .ok()
        .and_then(|d| {
            d.flatten().find_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.strip_suffix("_create_api_tokens.rs")
                    .and_then(|r| r.strip_prefix('m'))
                    .map(str::to_owned)
            })
        });
    found.unwrap_or_else(|| migration::stamp(info))
}

/// The API kernel: written once (shared), removed with the last API resource.
fn kernel(info: &AppInfo, plan: &mut Plan, flags: &Flags) {
    let k = &info.krate;
    let stamp = kernel_stamp(info, flags);
    plan.option("api_stamp", stamp.clone());
    plan.cargo("utoipa --features axum_extras,chrono", "utoipa");
    let module = format!("m{stamp}_create_api_tokens");
    let mig = format!("migration/src/{module}.rs");
    plan.shared(&mig, TOKEN_MIGRATION);
    plan.insert_for(
        &mig,
        "migration/src/lib.rs",
        "migration mods",
        format!("mod {module};\n"),
    );
    plan.insert_for(
        &mig,
        "migration/src/lib.rs",
        "migrations",
        format!("Box::new({module}::Migration),\n"),
    );
    plan.migrations = true;
    let model = "src/models/api_token.rs";
    plan.shared(model, TOKEN_MODEL);
    plan.insert_for(
        model,
        "src/models/mod.rs",
        "model mods",
        "pub mod api_token;\n",
    );
    plan.shared(KERNEL, MOD);
    plan.insert_for(
        KERNEL,
        "src/controllers/mod.rs",
        "controller mods",
        "pub mod api;\n",
    );
    feature::plan_into(
        plan,
        &feature::Routes {
            module: "api".into(),
            what: "The API's own routes (`cargo offrails generate api`; .offrails/guides/api.md)".into(),
            mounts: vec![
                Mount::new("/api/openapi.json", &[("get", "crate::controllers::api::openapi_json")]),
                Mount::new("/api/v1/me", &[("get", "crate::controllers::api::me")]),
            ],
            paths: "pub fn api_openapi() -> String {\n    \"/api/openapi.json\".into()\n}\n\npub fn api_me() -> String {\n    \"/api/v1/me\".into()\n}\n".into(),
            rows: vec![
                ("GET".into(), "/api/openapi.json".into(), "api::openapi_json".into(), "Public".into()),
                ("GET".into(), "/api/v1/me".into(), "api::me".into(), "User".into()),
            ],
            shared: true,
        },
    );
    let task = "src/tasks/tokens.rs";
    plan.shared(task, TOKEN_TASK);
    plan.insert_for(task, "src/tasks/mod.rs", "task mods", "pub mod tokens;\n");
    plan.insert_for(
        task,
        "src/tasks/mod.rs",
        "tasks",
        "\"tokens\" => tokens::run(state, args).await,\n",
    );
    let tests = "tests/app/api_test.rs";
    plan.shared(tests, TESTS.replace("__CRATE__", k));
    plan.insert_for(tests, "tests/app/main.rs", "test mods", "mod api_test;\n");
    plan.insert_for(
        KERNEL,
        "tests/app/support.rs",
        "test support",
        format!(
            "/// A real API token for `user` (read+write, or read-only), for `Authorization: Bearer`.\npub async fn api_token_for(&self, user: &{k}::models::User, write: bool) -> String {{\n    let (_, raw) =\n        {k}::models::api_token::issue(self.db(), user, \"test\", write, None, chrono::Utc::now())\n            .await\n            .expect(\"issue an api token\");\n    raw\n}}\n\n"
        ),
    );
    plan.byproduct(KERNEL, "docs/openapi.json");
    // Every line the kernel wires in is marked `api`: it goes with the last API resource.
    for owner in [
        mig.as_str(),
        model,
        KERNEL,
        "src/routes/api.rs",
        task,
        tests,
    ] {
        plan.shared_tags.insert(owner.to_owned(), "api".into());
    }
    // Apps made before 0.7.2 have no JSON request helper in TestApp.
    let helper = match flags.recorded("json_helper").as_deref() {
        Some(v) => v == "inserted",
        None => !info
            .read("tests/app/support.rs")
            .unwrap_or_default()
            .contains("pub async fn send_json("),
    };
    plan.option("json_helper", if helper { "inserted" } else { "template" });
    if helper {
        let template = include_str!("../../../../template/app/tests/app/support.rs");
        let start = template
            .find("    /// A JSON request (`body` serialized")
            .unwrap_or(0);
        let end = template[start..]
            .find("    /// Status and headers without reading")
            .map_or(template.len(), |e| start + e);
        let text: String = template[start..end]
            .lines()
            .map(|l| format!("{}\n", l.strip_prefix("    ").unwrap_or(l)))
            .collect();
        plan.insert_for(KERNEL, "tests/app/support.rs", "test support", text);
    }
}

/// (JSON input type, how to put it into the `Params` string `p.<param>`).
fn input_of(f: &Field) -> (&'static str, String) {
    let p = f.param();
    match f.kind {
        Kind::Integer | Kind::BigInt | Kind::References { .. } => {
            ("Option<i64>", "v.to_string()".into())
        }
        Kind::Boolean if f.nullable => ("Option<bool>", "v.to_string()".into()),
        Kind::Boolean => (
            "Option<bool>",
            "if v { \"true\".into() } else { String::new() }".into(),
        ),
        Kind::Json => ("Option<serde_json::Value>", "v.to_string()".into()),
        _ => {
            let _ = p;
            ("Option<String>", "v".into())
        }
    }
}

/// (JSON output field type, value from the model `r`).
fn output_of(f: &Field) -> (String, String) {
    let c = f.column();
    let wrap = |t: &str| {
        if f.nullable {
            format!("Option<{t}>")
        } else {
            t.to_owned()
        }
    };
    match &f.kind {
        Kind::String | Kind::Text => (wrap("String"), format!("r.{c}.clone()")),
        Kind::Integer => (wrap("i32"), format!("r.{c}")),
        Kind::BigInt | Kind::Money | Kind::References { .. } => (wrap("i64"), format!("r.{c}")),
        Kind::Boolean => (wrap("bool"), format!("r.{c}")),
        Kind::Decimal { scale, .. } => (
            wrap("String"),
            if f.nullable {
                format!("r.{c}.map(|d| d.round_dp({scale}).to_string())")
            } else {
                format!("r.{c}.round_dp({scale}).to_string()")
            },
        ),
        Kind::DateTime => (wrap("chrono::DateTime<chrono::Utc>"), format!("r.{c}")),
        Kind::Date => (wrap("chrono::NaiveDate"), format!("r.{c}")),
        Kind::Uuid => (
            wrap("String"),
            if f.nullable {
                format!("r.{c}.map(|u| u.to_string())")
            } else {
                format!("r.{c}.to_string()")
            },
        ),
        Kind::Json => (wrap("serde_json::Value"), format!("r.{c}.clone()")),
    }
}

/// `generate api <name> field:type…`.
pub fn plan(info: &AppInfo, args: &[String], flags: &Flags) -> Result<Plan> {
    if !info.auth {
        return Err(Fail::usage(
            "generate api needs accounts (tokens belong to users): cargo offrails add auth-core auth-magic-link (or auth-password)",
        ));
    }
    if flags.public {
        return Err(Fail::usage(
            "an API resource is the token owner's: --public isn't offered (api.md: never 403 another's record, scope it)",
        ));
    }
    let names = Names::new(args.first().map(String::as_str).unwrap_or(""))?;
    let mut plan = Plan::new("api", &names.plural, args);
    // The model: ours from an earlier generate model/resource (same fields), or new.
    let manifest = Manifest::load(&info.dir)?;
    let earlier = manifest
        .find("resource", &names.plural)
        .or_else(|| manifest.find("model", &names.singular))
        .cloned();
    let model_path = format!("src/models/{}/", names.singular);
    let reuse = match flags.recorded("model").as_deref() {
        Some(m) => m == "reused",
        None => info.model_source(&names.singular).is_some(),
    };
    plan.option("model", if reuse { "reused" } else { "new" });
    let model_args: Vec<String> = if reuse {
        let Some(e) = &earlier else {
            return Err(Fail::usage(format!(
                "{model_path} exists but no generator wrote it (docs/generated.toml), so its fields are unknown: \
                 write the API by hand from .offrails/guides/api.md, or pick another name"
            )));
        };
        if args.len() > 1 && args[1..] != e.args[1..] {
            return Err(Fail::usage(format!(
                "{model_path} was generated with {}; give the same fields, or none to reuse them",
                e.args[1..].join(" ")
            )));
        }
        let mut a = vec![args[0].clone()];
        a.extend_from_slice(&e.args[1..]);
        a
    } else {
        args.to_vec()
    };
    let mut model_flags = flags.clone();
    if reuse && let Some(e) = &earlier {
        model_flags.recorded = e.options.clone();
    }
    let spec = ModelSpec::resolve(info, &model_args, &model_flags, &plan.command())?;
    if spec.scope != Scope::User {
        return Err(Fail::usage(format!(
            "{} is --public; the API serves records that belong to the token's user",
            names.model
        )));
    }
    let fresh = flags.recorded("stamp").is_none();
    let k = &info.krate;
    let n = &spec.names;
    let (s, p, m, pc, kb) = (&n.singular, &n.plural, &n.model, &n.plural_camel, &n.kebab);
    if fresh {
        feature::check_free(
            info,
            Some(&format!("api_v1_{p}")),
            &[format!("api_{p}"), format!("api_{s}")],
            &[format!("/api/v1/{kb}")],
        )?;
    }
    let stamp = flags
        .recorded("stamp")
        .unwrap_or_else(|| migration::stamp(info));
    plan.option("stamp", stamp.clone());
    plan.option("scope", "user");
    if !reuse {
        spec.plan_into(&mut plan, &stamp);
    }
    kernel(info, &mut plan, flags);
    let cmd = plan.command();

    // ---------- views: the JSON bodies ----------
    let mut input_fields = String::new();
    let mut overlay = String::new();
    let mut out_fields = String::new();
    let mut out_values = String::new();
    for f in &spec.fields {
        let (ty, conv) = input_of(f);
        let pa = f.param();
        let doc = match f.kind {
            Kind::Money => "    /// An amount: \"12.30\".\n",
            Kind::DateTime => "    /// \"2026-01-15T09:30\" (UTC).\n",
            Kind::Date => "    /// \"2026-01-15\".\n",
            Kind::Decimal { .. } => "    /// A decimal as a string: \"12.50\".\n",
            _ => "",
        };
        input_fields.push_str(&format!(
            "{doc}    #[serde(default)]\n    pub {pa}: {ty},\n"
        ));
        overlay.push_str(&format!(
            "        if let Some(v) = self.{pa} {{\n            p.{pa} = {conv};\n        }}\n"
        ));
        let (oty, val) = output_of(f);
        out_fields.push_str(&format!("    pub {}: {oty},\n", f.column()));
        out_values.push_str(&format!("            {}: {val},\n", f.column()));
    }
    let views_path = format!("src/views/{p}_json.rs");
    plan.create(
        &views_path,
        format!(
            "//! The `/api/v1/{kb}` bodies (guides/api.md#responses). Generated by `{cmd}`.\n//! Frozen once clients use them: add fields freely, rename or remove only in /api/v2.\n\nuse serde::{{Deserialize, Serialize}};\nuse utoipa::ToSchema;\n\nuse crate::controllers::api::PageInfo;\nuse crate::models::{s}::{{self, {m}Params}};\n\n/// One {lower}, as the API shows it (never the entity: new columns don't leak).\n#[derive(Debug, Clone, Serialize, ToSchema)]\npub struct {m}Json {{\n    pub id: i64,\n{out_fields}    pub created_at: chrono::DateTime<chrono::Utc>,\n    pub updated_at: chrono::DateTime<chrono::Utc>,\n}}\n\nimpl From<&{s}::Model> for {m}Json {{\n    fn from(r: &{s}::Model) -> Self {{\n        Self {{\n            id: r.id,\n{out_values}            created_at: r.created_at,\n            updated_at: r.updated_at,\n        }}\n    }}\n}}\n\n/// `{{\"data\": {{…}}}}`\n#[derive(Debug, Clone, Serialize, ToSchema)]\npub struct {m}Body {{\n    pub data: {m}Json,\n}}\n\n/// `{{\"data\": [...], \"page\": {{…}}}}`\n#[derive(Debug, Clone, Serialize, ToSchema)]\npub struct {pc}Page {{\n    pub data: Vec<{m}Json>,\n    pub page: PageInfo,\n}}\n\n/// The JSON a client sends. Absent fields keep their value on update. No owner: it's the token's.\n#[derive(Debug, Clone, Default, Deserialize, ToSchema)]\npub struct {m}Input {{\n{input_fields}}}\n\nimpl {m}Input {{\n    /// Onto `base` (empty on create, the saved values on update), then `validate()` as the form does.\n    pub fn onto(self, mut p: {m}Params) -> {m}Params {{\n{overlay}        p\n    }}\n}}\n",
            lower = n.lower
        ),
    );
    plan.insert(
        "src/views/mod.rs",
        "view mods",
        format!("pub mod {p}_json;\n"),
    );

    // ---------- the controller ----------
    let base = format!("/api/v1/{kb}");
    plan.create(
        format!("src/controllers/api_v1_{p}.rs"),
        format!(
            "//! `{base}`: the token user's own {lower_p} (guides/api.md). Generated by `{cmd}`.\n//! Every lookup goes through `{s}::find_for`, so another user's id is 404; writes need the\n//! `write` scope (403).\n\nuse axum::{{\n    Json,\n    extract::{{Path, State}},\n    http::StatusCode,\n    response::{{IntoResponse, Response}},\n}};\n\nuse crate::{{\n    controllers::api::{{ApiJson, ApiUser, ErrorBody, InvalidBody, PageInfo}},\n    error::AppError,\n    models::{{Page, {s}, {s}::{m}Params}},\n    state::AppState,\n    views::{p}_json::{{{m}Body, {m}Input, {m}Json, {pc}Page}},\n}};\n\n#[utoipa::path(\n    get,\n    path = \"{base}\",\n    tag = \"{p}\",\n    security((\"bearer\" = [])),\n    params(\n        (\"page\" = Option<u64>, Query, description = \"1-based\"),\n        (\"per\" = Option<u64>, Query, description = \"1 to 100, default 25\")\n    ),\n    responses((status = 200, body = {pc}Page), (status = 401, body = ErrorBody), (status = 422, body = InvalidBody))\n)]\n#[tracing::instrument(skip_all)]\npub async fn index(State(s): State<AppState>, me: ApiUser, page: Page) -> Result<Json<{pc}Page>, AppError> {{\n    let (rows, more) = {s}::all_for(&s.db, &me.user, page).await?;\n    Ok(Json({pc}Page {{\n        data: rows.iter().map({m}Json::from).collect(),\n        page: PageInfo {{\n            page: page.number(),\n            per: page.per(),\n            has_more: more,\n        }},\n    }}))\n}}\n\n#[utoipa::path(\n    get,\n    path = \"{base}/{{id}}\",\n    tag = \"{p}\",\n    security((\"bearer\" = [])),\n    params((\"id\" = i64, Path)),\n    responses((status = 200, body = {m}Body), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody))\n)]\n#[tracing::instrument(skip_all)]\npub async fn show(State(s): State<AppState>, me: ApiUser, Path(id): Path<i64>) -> Result<Json<{m}Body>, AppError> {{\n    let record = {s}::find_for(&s.db, &me.user, id).await?;\n    Ok(Json({m}Body {{\n        data: {m}Json::from(&record),\n    }}))\n}}\n\n#[utoipa::path(\n    post,\n    path = \"{base}\",\n    tag = \"{p}\",\n    security((\"bearer\" = [])),\n    request_body = {m}Input,\n    responses((status = 201, body = {m}Body), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 422, body = InvalidBody))\n)]\n#[tracing::instrument(skip_all)]\npub async fn create(\n    State(s): State<AppState>,\n    me: ApiUser,\n    ApiJson(input): ApiJson<{m}Input>,\n) -> Result<Response, AppError> {{\n    me.require_write()?;\n    let new = input.onto({m}Params::default()).validate().map_err(AppError::Invalid)?;\n    let record = {s}::create(&s.db, &me.user, new).await?;\n    Ok((\n        StatusCode::CREATED,\n        Json({m}Body {{\n            data: {m}Json::from(&record),\n        }}),\n    )\n        .into_response())\n}}\n\n#[utoipa::path(\n    patch,\n    path = \"{base}/{{id}}\",\n    tag = \"{p}\",\n    security((\"bearer\" = [])),\n    params((\"id\" = i64, Path)),\n    request_body = {m}Input,\n    responses((status = 200, body = {m}Body), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 422, body = InvalidBody))\n)]\n#[tracing::instrument(skip_all)]\npub async fn update(\n    State(s): State<AppState>,\n    me: ApiUser,\n    Path(id): Path<i64>,\n    ApiJson(input): ApiJson<{m}Input>,\n) -> Result<Json<{m}Body>, AppError> {{\n    me.require_write()?;\n    let record = {s}::find_for(&s.db, &me.user, id).await?;\n    let new = input.onto({m}Params::from(&record)).validate().map_err(AppError::Invalid)?;\n    let record = {s}::update_for(&s.db, &me.user, record, new).await?;\n    Ok(Json({m}Body {{\n        data: {m}Json::from(&record),\n    }}))\n}}\n\n#[utoipa::path(\n    delete,\n    path = \"{base}/{{id}}\",\n    tag = \"{p}\",\n    security((\"bearer\" = [])),\n    params((\"id\" = i64, Path)),\n    responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody))\n)]\n#[tracing::instrument(skip_all)]\npub async fn destroy(State(s): State<AppState>, me: ApiUser, Path(id): Path<i64>) -> Result<StatusCode, AppError> {{\n    me.require_write()?;\n    let record = {s}::find_for(&s.db, &me.user, id).await?;\n    {s}::destroy_for(&s.db, &me.user, record).await?;\n    Ok(StatusCode::NO_CONTENT)\n}}\n",
            lower_p = n.lower_plural
        ),
    );
    plan.insert(
        "src/controllers/mod.rs",
        "controller mods",
        format!("pub mod api_v1_{p};\n"),
    );
    let h = format!("crate::controllers::api_v1_{p}");
    plan.insert(
        KERNEL,
        "api paths",
        format!("{h}::index,\n{h}::show,\n{h}::create,\n{h}::update,\n{h}::destroy,\n"),
    );
    let vj = format!("crate::views::{p}_json");
    plan.insert(
        KERNEL,
        "api schemas",
        format!("{vj}::{m}Json,\n{vj}::{m}Body,\n{vj}::{pc}Page,\n{vj}::{m}Input,\n"),
    );
    let h = |a: &str| format!("crate::controllers::api_v1_{p}::{a}");
    feature::plan_into(
        &mut plan,
        &feature::Routes {
            module: format!("api_v1_{p}"),
            what: format!(
                "The {} API (`cargo offrails generate api {p}`)",
                n.human_plural
            ),
            mounts: vec![
                Mount::new(
                    base.clone(),
                    &[("get", &h("index")), ("post", &h("create"))],
                ),
                Mount::new(
                    format!("{base}/{{id}}"),
                    &[
                        ("get", &h("show")),
                        ("patch", &h("update")),
                        ("delete", &h("destroy")),
                    ],
                ),
            ],
            paths: format!(
                "pub fn api_{p}() -> String {{\n    \"{base}\".into()\n}}\n\npub fn api_{s}(id: i64) -> String {{\n    format!(\"{base}/{{id}}\")\n}}\n"
            ),
            rows: [
                ("GET", base.clone(), "index"),
                ("POST", base.clone(), "create"),
                ("GET", format!("{base}/{{id}}"), "show"),
                ("PATCH", format!("{base}/{{id}}"), "update"),
                ("DELETE", format!("{base}/{{id}}"), "destroy"),
            ]
            .into_iter()
            .map(|(m, path, a)| {
                (
                    m.to_owned(),
                    path,
                    format!("api_v1_{p}::{a}"),
                    "User".to_owned(),
                )
            })
            .collect(),
            shared: false,
        },
    );
    // The tenancy sweep signs in with a cookie; API routes take a token and test their own 404s.
    let tenancy = info.read("tests/app/tenancy_test.rs");
    let tenancy_ours = tenancy
        .as_deref()
        .is_some_and(|t| t.contains("// samples: not tenant ids"))
        || flags.manifest_shared.contains("tests/app/tenancy_test.rs");
    if tenancy_ours || flags.recorded("tenancy_row").is_some() {
        plan.option("tenancy_row", "yes");
        plan.insert(
            "tests/app/tenancy_test.rs",
            "not tenant ids",
            format!(
                "(\"{base}/{{id}}\", \"a bearer-token API route: api_{p}_test::another_users_{s}_is_404\"),\n"
            ),
        );
    } else if tenancy.is_some() {
        plan.next.push(format!(
            "tests/app/tenancy_test.rs: list (\"{base}/{{id}}\", \"bearer-token route: api_{p}_test\") in NOT_TENANT_IDS (the sweep signs in with a cookie)"
        ));
    }

    // ---------- tests ----------
    let body_fields: String = spec
        .fields
        .iter()
        .map(|f| {
            let pa = f.param();
            let v = match f.kind {
                Kind::Integer | Kind::BigInt | Kind::References { .. } => {
                    format!("params.{pa}.parse::<i64>().ok()")
                }
                Kind::Boolean if f.nullable => format!("params.{pa}.parse::<bool>().ok()"),
                Kind::Boolean => format!("params.{pa} == \"true\""),
                Kind::Json => {
                    format!("serde_json::from_str::<serde_json::Value>(&params.{pa}).ok()")
                }
                _ => format!("params.{pa}.clone()"),
            };
            format!("        \"{pa}\": {v},\n")
        })
        .collect();
    let label = super::model::label_field(&spec.fields);
    let invalid = spec
        .fields
        .iter()
        .find(|f| !f.nullable && matches!(f.kind, Kind::String | Kind::Text));
    let owner = spec.owner.as_ref().map_or("user_id", |o| o.column.as_str());
    let mut tests = format!(
        "//! `{base}` (src/controllers/api_v1_{p}.rs). Generated by `{cmd}`.\n//! The request matrix per endpoint (api.md#testing): happy path, 422 with the exact body, 401 without\n//! a token, 404 for another user's record, 403 for a read-only token.\n//! APP: cite each acceptance criterion (`/// AC-{p}-1`) on the test that proves it.\n\nuse axum::http::StatusCode;\nuse {k}::models::{s};\nuse {k}::routes::paths;\nuse serde_json::{{Value, json}};\n\nuse crate::factories;\nuse crate::support::TestApp;\n\n/// The JSON a client would send for these (valid) params.\nfn body(params: &{s}::{m}Params) -> Value {{\n    json!({{\n{body_fields}    }})\n}}\n\nfn bearer(token: &str) -> String {{\n    format!(\"Bearer {{token}}\")\n}}\n\n"
    );
    tests.push_str(&format!(
        "#[tokio::test]\nasync fn index_lists_only_the_token_users_{p}_with_page_info() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let them = factories::user(app.db(), \"them@example.com\").await;\n    let mine = factories::{s}(app.db(), &me).await;\n    let theirs = factories::{s}(app.db(), &them).await;\n    let token = app.api_token_for(&me, false).await;\n    let res = app\n        .send_json(\"GET\", &paths::api_{p}(), None, &[(\"authorization\", bearer(&token).as_str())])\n        .await;\n    res.assert_ok();\n    let page: Value = res.json();\n    let ids: Vec<i64> = page[\"data\"].as_array().unwrap().iter().map(|r| r[\"id\"].as_i64().unwrap()).collect();\n    assert_eq!(ids, vec![mine.id]);\n    assert!(!ids.contains(&theirs.id));\n    assert_eq!(page[\"page\"], json!({{ \"page\": 1, \"per\": 25, \"has_more\": false }}));\n}}\n\n/// SECURITY: no token, no data.\n#[tokio::test]\nasync fn every_endpoint_needs_a_token() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let record = factories::{s}(app.db(), &me).await;\n    for (method, path) in [\n        (\"GET\", paths::api_{p}()),\n        (\"POST\", paths::api_{p}()),\n        (\"GET\", paths::api_{s}(record.id)),\n        (\"PATCH\", paths::api_{s}(record.id)),\n        (\"DELETE\", paths::api_{s}(record.id)),\n    ] {{\n        app.send_json(method, &path, Some(&json!({{}})), &[])\n            .await\n            .assert_status(StatusCode::UNAUTHORIZED);\n    }}\n}}\n\n#[tokio::test]\nasync fn show_returns_the_{s}_and_404s_a_missing_one() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let record = factories::{s}(app.db(), &me).await;\n    let token = app.api_token_for(&me, false).await;\n    let bearer = bearer(&token);\n    let auth = [(\"authorization\", bearer.as_str())];\n    let res = app.send_json(\"GET\", &paths::api_{s}(record.id), None, &auth).await;\n    res.assert_ok();\n    assert_eq!(res.json::<Value>()[\"data\"][\"id\"], json!(record.id));\n    let res = app.send_json(\"GET\", &paths::api_{s}(record.id + 1000), None, &auth).await;\n    res.assert_status(StatusCode::NOT_FOUND);\n    assert_eq!(res.json::<Value>(), json!({{ \"error\": \"not_found\" }}));\n}}\n\n#[tokio::test]\nasync fn create_is_201_with_the_body_and_saves_it() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let token = app.api_token_for(&me, true).await;\n    let params = factories::{s}_params(app.db(), &me).await;\n    let res = app\n        .send_json(\"POST\", &paths::api_{p}(), Some(&body(&params)), &[(\"authorization\", bearer(&token).as_str())])\n        .await;\n    res.assert_status(StatusCode::CREATED);\n    let id = res.json::<Value>()[\"data\"][\"id\"].as_i64().unwrap();\n    let saved = factories::reload_{s}(app.db(), id).await.expect(\"saved\");\n    assert_eq!(saved.{owner}, me.id);\n}}\n\n/// SECURITY: a read-only token can't write anything.\n#[tokio::test]\nasync fn a_read_only_token_cant_write() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let record = factories::{s}(app.db(), &me).await;\n    let token = app.api_token_for(&me, false).await;\n    let params = factories::{s}_params(app.db(), &me).await;\n    let auth = bearer(&token);\n    for (method, path) in [\n        (\"POST\", paths::api_{p}()),\n        (\"PATCH\", paths::api_{s}(record.id)),\n        (\"DELETE\", paths::api_{s}(record.id)),\n    ] {{\n        app.send_json(method, &path, Some(&body(&params)), &[(\"authorization\", auth.as_str())])\n            .await\n            .assert_status(StatusCode::FORBIDDEN);\n    }}\n    assert_eq!(factories::reload_{s}(app.db(), record.id).await, Some(record));\n}}\n\n#[tokio::test]\nasync fn a_body_that_isnt_json_is_a_422() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let token = app.api_token_for(&me, true).await;\n    let res = app\n        .send_json(\"POST\", &paths::api_{p}(), Some(&json!(\"not an object\")), &[(\"authorization\", bearer(&token).as_str())])\n        .await;\n    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY);\n    assert_eq!(res.json::<Value>(), json!({{ \"errors\": {{ \"body\": [\"is not valid JSON\"] }} }}));\n}}\n\n"
    ));
    if let Some(f) = invalid {
        tests.push_str(&format!(
            "#[tokio::test]\nasync fn invalid_input_is_a_422_with_the_errors() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let token = app.api_token_for(&me, true).await;\n    let mut params = factories::{s}_params(app.db(), &me).await;\n    params.{pa} = String::new();\n    let res = app\n        .send_json(\"POST\", &paths::api_{p}(), Some(&body(&params)), &[(\"authorization\", bearer(&token).as_str())])\n        .await;\n    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY);\n    assert_eq!(res.json::<Value>()[\"errors\"][\"{name}\"], json!([\"can't be blank\"]));\n}}\n\n",
            pa = f.param(),
            name = f.name
        ));
    }
    let patch = match label {
        Some(f) => format!(
            "    let res = app\n        .send_json(\"PATCH\", &paths::api_{s}(record.id), Some(&json!({{ \"{pa}\": \"Changed\" }})), &[(\"authorization\", auth.as_str())])\n        .await;\n    res.assert_ok();\n    assert_eq!(res.json::<Value>()[\"data\"][\"{c}\"], json!(\"Changed\"));\n",
            pa = f.param(),
            c = f.column()
        ),
        None => format!(
            "    app.send_json(\"PATCH\", &paths::api_{s}(record.id), Some(&json!({{}})), &[(\"authorization\", auth.as_str())])\n        .await\n        .assert_ok();\n"
        ),
    };
    tests.push_str(&format!(
        "#[tokio::test]\nasync fn update_changes_what_was_sent_and_destroy_is_204() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let record = factories::{s}(app.db(), &me).await;\n    let token = app.api_token_for(&me, true).await;\n    let auth = bearer(&token);\n{patch}    app.send_json(\"DELETE\", &paths::api_{s}(record.id), None, &[(\"authorization\", auth.as_str())])\n        .await\n        .assert_status(StatusCode::NO_CONTENT);\n    assert!(factories::reload_{s}(app.db(), record.id).await.is_none());\n}}\n\n/// SECURITY: another user's {lower} is 404 for every method (it looks missing), and stays as it was.\n#[tokio::test]\nasync fn another_users_{s}_is_404() {{\n    let app = TestApp::new().await;\n    let me = factories::user(app.db(), \"me@example.com\").await;\n    let them = factories::user(app.db(), \"them@example.com\").await;\n    let theirs = factories::{s}(app.db(), &them).await;\n    let token = app.api_token_for(&me, true).await;\n    let auth = bearer(&token);\n    let params = factories::{s}_params(app.db(), &me).await;\n    for method in [\"GET\", \"PATCH\", \"DELETE\"] {{\n        app.send_json(method, &paths::api_{s}(theirs.id), Some(&body(&params)), &[(\"authorization\", auth.as_str())])\n            .await\n            .assert_status(StatusCode::NOT_FOUND);\n    }}\n    assert_eq!(factories::reload_{s}(app.db(), theirs.id).await, Some(theirs));\n}}\n",
        lower = n.lower
    ));
    plan.create(format!("tests/app/api_{p}_test.rs"), tests);
    plan.insert(
        "tests/app/main.rs",
        "test mods",
        format!("mod api_{p}_test;\n"),
    );
    if !reuse {
        plan.next.push(
            "cargo run -q -- db migrate   # then prove down(): db rollback && db migrate".into(),
        );
    } else {
        plan.next
            .push("cargo run -q -- db migrate   # the api_tokens table, if it's new".into());
    }
    plan.next.push(
        "UPDATE_SCHEMAS=1 cargo nextest run openapi   # writes docs/openapi.json; commit it (the contract)".into(),
    );
    plan.next.push(
        "a token: cargo run -q -- task tokens create <email> [name] [--read-only]; then curl -H \"Authorization: Bearer <token>\" localhost:3000/api/v1/…".into(),
    );
    if !info
        .read("AGENTS.md")
        .unwrap_or_default()
        .contains("API:            rest")
    {
        plan.next
            .push("AGENTS.md: set `API: rest` (the app now has a public API)".into());
    }
    if !info
        .read("tests/app/smoke_test.rs")
        .unwrap_or_default()
        .contains("request_method")
    {
        plan.next.push(
            "tests/app/smoke_test.rs and security_test.rs sweep ROUTES with POST: send each row's own method (TestApp::request_method; CHANGELOG 0.7.2 App migration)".into(),
        );
    }
    Ok(plan)
}
