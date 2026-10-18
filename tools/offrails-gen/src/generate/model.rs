//! `generate model <Name> field:type…` (`rails generate model`): the create-table migration, the
//! model file (entity, `Params` → `validate()` → `New`, typed and actor-scoped queries), the
//! factory, and model tests (validation units inline, queries against both databases).
//! Spec: .offrails/guides/models.md.

use super::Flags;
use super::app::AppInfo;
use super::fields::{self, Field, Kind};
use super::migration::{self, Owner};
use super::names::Names;
use super::plan::{Anchor, Plan};
use crate::util::{Fail, Result};

/// Who may see a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `TENANCY: per-user`: every record has an owner; lookups take the actor
    /// (`find_for(db, &user, id)`), another user's id is 404.
    User,
    /// No accounts involved: anyone may read and write (`--public`).
    Public,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Public => "public",
        }
    }
}

/// A referenced model, as the generator found it in the app.
#[derive(Debug, Clone)]
pub struct Target {
    pub names: Names,
    /// It has `find_for(db, &user, id)` (else `find(db, id)`).
    pub scoped: bool,
    /// It has `choices_for` / `choices` (a form can offer a `<select>`).
    pub choices: bool,
    /// Its file has `samples: relations` / `samples: related` (we add the `has_many`).
    pub anchors: bool,
    /// The file with its entity: `src/models/<x>/mod.rs`, or `src/models/<x>.rs` (one file).
    pub entity: String,
}

#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub names: Names,
    pub fields: Vec<Field>,
    pub scope: Scope,
    pub owner: Option<Owner>,
    pub krate: String,
    pub targets: Vec<(String, Target)>,
    pub command: String,
}

/// Every string column first: the label of a record in lists and `<select>`s.
pub fn label_field(fields: &[Field]) -> Option<&Field> {
    fields
        .iter()
        .find(|f| matches!(f.kind, Kind::String) && !f.nullable)
        .or_else(|| fields.iter().find(|f| matches!(f.kind, Kind::String)))
}

impl ModelSpec {
    /// Resolves names, fields, scope and references against the app.
    pub fn resolve(info: &AppInfo, args: &[String], flags: &Flags, command: &str) -> Result<Self> {
        let Some(raw) = args.first() else {
            return Err(Fail::usage(
                "needs a name and fields: Post title:string body:text",
            ));
        };
        let names = Names::new(raw)?;
        let scope = match flags.recorded("scope").as_deref() {
            Some("user") => Scope::User,
            Some("public") => Scope::Public,
            _ => {
                if flags.public {
                    Scope::Public
                } else if info.auth {
                    Scope::User
                } else {
                    return Err(Fail::usage(format!(
                        "this app has no accounts (auth-core isn't installed), so {} can't belong to anyone.\n  \
                         Either add accounts first: cargo offrails add auth-core auth-magic-link (or auth-password),\n  \
                         or pass --public: anyone may read, create, change and delete {} (an internal tool, a demo)",
                        names.lower_plural, names.lower_plural
                    )));
                }
            }
        };
        if scope == Scope::User && !info.auth {
            return Err(Fail::usage(
                "--scope user needs accounts: cargo offrails add auth-core auth-magic-link",
            ));
        }
        let mut reserved = vec!["id", "created_at", "updated_at"];
        if scope == Scope::User {
            reserved.push("user_id");
        }
        let mut fields = fields::parse_all(&args[1..], &reserved)?;
        // A `references{users}` field is the owner: set from the session, never from params.
        let mut owner = None;
        if scope == Scope::User {
            let users: Vec<usize> = fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.target().is_some_and(|t| t.plural == "users"))
                .map(|(i, _)| i)
                .collect();
            match users.as_slice() {
                [] => {
                    owner = Some(Owner {
                        column: "user_id".into(),
                    })
                }
                [one] => {
                    let f = fields.remove(*one);
                    owner = Some(Owner { column: f.column() });
                }
                _ => {
                    return Err(Fail::usage(
                        "more than one references{users} field: the generator sets one owner from the session; add the others by hand",
                    ));
                }
            }
        } else if fields
            .iter()
            .any(|f| f.target().is_some_and(|t| t.plural == "users"))
        {
            return Err(Fail::usage(
                "a --public model can't take a user id from a form; drop the references{users} field or generate it scoped",
            ));
        }
        let mut targets = Vec::new();
        for f in &fields {
            let Some(t) = f.target() else { continue };
            if t.plural == names.plural {
                return Err(Fail::usage(format!(
                    "{}: a model referencing itself isn't generated; add `{}` by hand after generating",
                    f.name,
                    f.column()
                )));
            }
            let path = format!("src/models/{}/", t.singular);
            let Some((entity, src)) = info.model_source(&t.singular) else {
                return Err(Fail::usage(format!(
                    "{}:references: {path} doesn't exist; generate {} first (cargo offrails g model {} …)",
                    f.name, t.model, t.model
                )));
            };
            let target = Target {
                names: t.clone(),
                scoped: src.contains("pub async fn find_for("),
                choices: src.contains("pub async fn choices"),
                anchors: src.lines().any(|l| l.trim() == "// samples: relations")
                    && src.lines().any(|l| l.trim() == "// samples: related"),
                entity,
            };
            if scope == Scope::User && !target.scoped {
                return Err(Fail::usage(format!(
                    "{}:references: {path} has no find_for(db, &user, id), so a user could attach another's {}; \
                     scope it first (.offrails/guides/models.md#scoped-lookups-tenancy)",
                    f.name, t.lower
                )));
            }
            if scope == Scope::Public && target.scoped {
                return Err(Fail::usage(format!(
                    "{}:references: {} is scoped to users, and this model is --public; generate it scoped (drop --public)",
                    f.name, t.model
                )));
            }
            targets.push((f.name.clone(), target));
        }
        Ok(ModelSpec {
            names,
            fields,
            scope,
            owner,
            krate: info.krate.clone(),
            targets,
            command: command.to_owned(),
        })
    }

    pub fn target(&self, field: &Field) -> Option<&Target> {
        self.targets
            .iter()
            .find(|(n, _)| *n == field.name)
            .map(|(_, t)| t)
    }

    pub fn has(&self, pred: impl Fn(&Field) -> bool) -> bool {
        self.fields.iter().any(pred)
    }

    fn uniques(&self) -> Vec<&Field> {
        self.fields.iter().filter(|f| f.unique).collect()
    }

    // ---------- the model files: mod.rs (entity), params.rs, queries.rs ----------

    /// `src/models/<singular>/`: the entity's file, then validation, then queries.
    pub fn dir(&self) -> String {
        format!("src/models/{}", self.names.singular)
    }

    /// `mod.rs`: the entity (it mirrors the migration), its relations and its label, and the
    /// re-exports, so callers write `post::find_for`, `post::PostParams`.
    pub fn entity_file(&self) -> String {
        let n = &self.names;
        let mut s = String::new();
        s.push_str(&format!(
            "//! {}: the entity (it mirrors the migration), its relations and its label. Validation is in\n//! params.rs, the queries in queries.rs; both are re-exported here (`{}::find`, `{}::{}Params`).\n//! Generated by `{}`.\n//! APP: say what a {} is, and cite its spec (product/features/{}.md).\n\nuse sea_orm::entity::prelude::*;\n\nmod params;\nmod queries;\n\npub use params::*;\npub use queries::*;\n\n",
            n.human_plural, n.singular, n.singular, n.model, self.command, n.lower, n.plural
        ));
        s.push_str("#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]\n");
        s.push_str(&format!("#[sea_orm(table_name = \"{}\")]\n", n.plural));
        s.push_str("pub struct Model {\n    #[sea_orm(primary_key)]\n    pub id: i64,\n");
        if let Some(o) = &self.owner {
            s.push_str(&format!(
                "    /// The owner, set from the session (never from params).\n    pub {}: i64,\n",
                o.column
            ));
        }
        for f in &self.fields {
            if let Some(a) = f.entity_attr() {
                s.push_str(&format!("    {a}\n"));
            }
            if matches!(f.kind, Kind::Money) {
                s.push_str("    /// Integer cents: never a float.\n");
            }
            s.push_str(&format!("    pub {}: {},\n", f.column(), f.rust_type()));
        }
        s.push_str("    pub created_at: DateTimeUtc,\n    pub updated_at: DateTimeUtc,\n    // samples: columns\n}\n\n");

        // relations
        s.push_str(
            "#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]\npub enum Relation {\n",
        );
        let mut related = String::new();
        let mut related_done: Vec<String> = Vec::new();
        if let Some(o) = &self.owner {
            s.push_str(&format!(
                "    #[sea_orm(\n        belongs_to = \"super::user::Entity\",\n        from = \"Column::{}\",\n        to = \"super::user::Column::Id\",\n        on_delete = \"Cascade\"\n    )]\n    Owner,\n",
                super::names::camel(&o.column)
            ));
            related.push_str("impl Related<super::user::Entity> for Entity {\n    fn to() -> RelationDef {\n        Relation::Owner.def()\n    }\n}\n\n");
            related_done.push("user".into());
        }
        for f in &self.fields {
            let Some(t) = f.target() else { continue };
            let variant = super::names::camel(&f.name);
            let action = if f.nullable { "SetNull" } else { "Cascade" };
            s.push_str(&format!(
                "    #[sea_orm(\n        belongs_to = \"super::{}::Entity\",\n        from = \"Column::{}\",\n        to = \"super::{}::Column::Id\",\n        on_delete = \"{action}\"\n    )]\n    {variant},\n",
                t.singular,
                f.iden(),
                t.singular
            ));
            if !related_done.contains(&t.singular) {
                related.push_str(&format!(
                    "impl Related<super::{}::Entity> for Entity {{\n    fn to() -> RelationDef {{\n        Relation::{variant}.def()\n    }}\n}}\n\n",
                    t.singular
                ));
                related_done.push(t.singular.clone());
            }
        }
        s.push_str("    // samples: relations\n}\n\n");
        s.push_str(&related);
        s.push_str("// samples: related\n\n");
        s.push_str("/// Must stay empty: callbacks are banned (.offrails/guides/models.md#side-effects-no-callbacks).\nimpl ActiveModelBehavior for ActiveModel {}\n\n");
        if let Some(l) = label_field(&self.fields) {
            let col = l.column();
            let expr = if l.nullable {
                format!(
                    "self.{col}.clone().unwrap_or_else(|| format!(\"{} #{{}}\", self.id))",
                    n.human
                )
            } else {
                format!("self.{col}.clone()")
            };
            s.push_str(&format!(
                "impl Model {{\n    /// How a {} is named in lists and `<select>`s.\n    pub fn label(&self) -> String {{\n        {expr}\n    }}\n}}\n",
                n.lower
            ));
        } else {
            s.push_str(&format!(
                "impl Model {{\n    /// How a {} is named in lists and `<select>`s. APP: a better name than its id.\n    pub fn label(&self) -> String {{\n        format!(\"{} #{{}}\", self.id)\n    }}\n}}\n",
                n.lower, n.human
            ));
        }
        s
    }

    /// `params.rs`: the form's input (`Params`, all strings), `validate()` → `New`, the edit
    /// form's prefill, and the validation unit tests.
    pub fn params_file(&self) -> String {
        let n = &self.names;
        let m = &n.model;
        let user = self.scope == Scope::User;
        let mut s = format!(
            "//! {} input: `{m}Params` (untrusted, all strings) → `validate()` → `New{m}` (typed).\n//! Generated by `{}`.\n\n",
            n.human, self.command
        );
        if self.has(|f| f.kind == Kind::DateTime) {
            s.push_str("use chrono::NaiveDateTime;\n");
        }
        if self.has(|f| {
            matches!(
                f.kind,
                Kind::Decimal { .. } | Kind::DateTime | Kind::Date | Kind::Uuid | Kind::Json
            )
        }) {
            s.push_str("use sea_orm::entity::prelude::*;\n");
        }
        s.push_str("use serde::Deserialize;\n\nuse super::Model;\nuse crate::models::Errors;\n\n");
        s.push_str(&format!(
            "/// Untrusted input from the {} form. This struct IS the permit list{}. All strings, so a\n/// failed submit re-renders exactly what was typed.\n",
            n.lower,
            if user { ": no owner" } else { "" }
        ));
        s.push_str(&format!(
            "#[derive(Debug, Default, Clone, Deserialize)]\npub struct {m}Params {{\n"
        ));
        for f in &self.fields {
            s.push_str(&format!(
                "    #[serde(default)]\n    pub {}: String,\n",
                f.param()
            ));
        }
        s.push_str("}\n\n");
        s.push_str(&format!(
            "/// Validated, typed values. Only `{m}Params::validate` makes one.\n#[derive(Debug, Clone, PartialEq)]\npub struct New{m} {{\n"
        ));
        for f in &self.fields {
            s.push_str(&format!("    pub {}: {},\n", f.column(), f.rust_type()));
        }
        s.push_str("}\n\n");
        s.push_str(&format!("impl {m}Params {{\n"));
        s.push_str("    /// Rails wording: \"can't be blank\", \"is too long (maximum is N characters)\".\n");
        s.push_str(&format!("    pub fn validate(&self) -> Result<New{m}, Errors> {{\n        let mut errors = Errors::default();\n"));
        for f in &self.fields {
            s.push_str(&validate_field(f));
        }
        s.push_str(&format!("        errors.into_result(|| New{m} {{\n"));
        for f in &self.fields {
            let v = new_value(f);
            if v == f.column() {
                s.push_str(&format!("            {v},\n"));
            } else {
                s.push_str(&format!("            {}: {v},\n", f.column()));
            }
        }
        s.push_str("        })\n    }\n}\n\n");
        s.push_str(&format!(
            "/// Prefills the edit form.\nimpl From<&Model> for {m}Params {{\n    fn from(r: &Model) -> Self {{\n        Self {{\n"
        ));
        for f in &self.fields {
            s.push_str(&format!("            {}: {},\n", f.param(), to_param(f)));
        }
        s.push_str("        }\n    }\n}\n\n");
        if self.has(|f| f.kind == Kind::Money) {
            s.push_str(MONEY_FNS);
        }
        s.push_str(&self.unit_tests());
        s
    }

    /// `queries.rs`: every query, scoped to the actor when records belong to users.
    pub fn queries_file(&self) -> String {
        let n = &self.names;
        let user = self.scope == Scope::User;
        let mut s = format!(
            "//! {} queries{}. Generated by `{}`.\n\n",
            n.human_plural,
            if user {
                ": every lookup takes the actor (`find_for(db, &user, id)`), so another user's id is 404"
            } else {
                ""
            },
            self.command
        );
        s.push_str("use chrono::Utc;\n");
        s.push_str(
            "use sea_orm::{ActiveValue::Set, QueryOrder, QuerySelect, entity::prelude::*};\n\n",
        );
        s.push_str(&format!(
            "use super::{{ActiveModel, Column, Entity, Model, New{}}};\nuse crate::error::AppError;\n",
            n.model
        ));
        let mut models = Vec::new();
        if !self.uniques().is_empty() || !self.targets.is_empty() {
            models.push("Errors");
        }
        models.push("Page");
        if user {
            models.push("User");
        }
        s.push_str(&format!(
            "use crate::models::{};\n\n",
            if models.len() == 1 {
                models[0].to_owned()
            } else {
                format!("{{{}}}", models.join(", "))
            }
        ));
        if user {
            s.push_str(&self.user_queries());
        } else {
            s.push_str(&self.public_queries());
        }
        s.trim_end().to_owned() + "\n"
    }

    fn map_unique_fn(&self) -> String {
        let uniques = self.uniques();
        if uniques.is_empty() {
            return String::new();
        }
        let mut arms = String::new();
        for f in &uniques {
            arms.push_str(&format!(
                "        if msg.contains(\"{t}_{c}_key\") || msg.contains(\"{t}.{c}\") {{\n            return AppError::Invalid(Errors::single(\"{name}\", \"has already been taken\"));\n        }}\n",
                t = self.names.plural,
                c = f.column(),
                name = f.name
            ));
        }
        format!(
            "/// A unique index said no: a validation error on its field, not a 500\n/// (.offrails/guides/models.md#validations).\nfn map_unique(e: DbErr) -> AppError {{\n    if let Some(sea_orm::SqlErr::UniqueConstraintViolation(msg)) = e.sql_err() {{\n{arms}        return AppError::Invalid(Errors::single(\"base\", \"has already been taken\"));\n    }}\n    e.into()\n}}\n\n"
        )
    }

    fn set_fields(&self, var: &str) -> String {
        self.fields
            .iter()
            .map(|f| format!("    {var}.{c} = Set(new.{c});\n", c = f.column()))
            .collect()
    }

    fn check_refs_fn(&self) -> String {
        if self.targets.is_empty() {
            return String::new();
        }
        let user = self.scope == Scope::User;
        let mut body = String::new();
        for f in self.fields.iter().filter(|f| f.is_reference()) {
            let Some(t) = self.target(f) else { continue };
            let lookup = if user {
                format!(
                    "crate::models::{}::find_for(db, user, id)",
                    t.names.singular
                )
            } else {
                format!("crate::models::{}::find(db, id)", t.names.singular)
            };
            let open = if f.nullable {
                format!("    if let Some(id) = new.{} {{\n", f.column())
            } else {
                format!("    {{\n        let id = new.{};\n", f.column())
            };
            body.push_str(&format!(
                "{open}        match {lookup}.await {{\n            Ok(_) => {{}}\n            Err(AppError::NotFound) => errors.add(\"{}\", \"must exist\"),\n            Err(e) => return Err(e),\n        }}\n    }}\n",
                f.name
            ));
        }
        let (sig, who) = if user {
            ("db: &impl ConnectionTrait, user: &User", "the user may see")
        } else {
            ("db: &impl ConnectionTrait", "exist")
        };
        format!(
            "/// Every referenced record must be one {who} (a hidden id looks missing).\nasync fn check_references({sig}, new: &New{}) -> Result<(), AppError> {{\n    let mut errors = Errors::default();\n{body}    if errors.is_empty() {{\n        Ok(())\n    }} else {{\n        Err(AppError::Invalid(errors))\n    }}\n}}\n\n",
            self.names.model
        )
    }

    fn literal_sets(&self) -> String {
        self.fields
            .iter()
            .map(|f| format!("        {c}: Set(new.{c}),\n", c = f.column()))
            .collect()
    }

    fn db_err(&self) -> &'static str {
        if self.uniques().is_empty() {
            "AppError::from"
        } else {
            "map_unique"
        }
    }

    fn user_queries(&self) -> String {
        let n = &self.names;
        let m = &n.model;
        let o = self
            .owner
            .as_ref()
            .map(|o| o.column.clone())
            .unwrap_or_else(|| "user_id".into());
        let oc = super::names::camel(&o);
        let refs = if self.targets.is_empty() {
            ""
        } else {
            "    check_references(db, user, &new).await?;\n"
        };
        let mut s = String::new();
        s.push_str("/// Unscoped. Model-private, so a handler can't compile a lookup that skips the actor.\npub(in crate::models) async fn find(db: &impl ConnectionTrait, id: i64) -> Result<Model, AppError> {\n    Entity::find_by_id(id).one(db).await?.ok_or(AppError::NotFound)\n}\n\n");
        s.push_str("/// For jobs and tasks, which act without a request (their args carry ids).\n#[tracing::instrument(skip_all, fields(id))]\npub async fn find_system(db: &impl ConnectionTrait, id: i64) -> Result<Model, AppError> {\n    find(db, id).await\n}\n\n");
        s.push_str(&format!(
            "/// The {} if `user` owns it, else `NotFound` (another user's id looks missing).\n#[tracing::instrument(skip_all, fields(id))]\npub async fn find_for(db: &impl ConnectionTrait, user: &User, id: i64) -> Result<Model, AppError> {{\n    Entity::find_by_id(id)\n        .filter(Column::{oc}.eq(user.id))\n        .one(db)\n        .await?\n        .ok_or(AppError::NotFound)\n}}\n\n",
            n.lower
        ));
        s.push_str(&format!(
            "/// A page of `user`'s {}, newest first, and whether there's another page.\n#[tracing::instrument(skip_all)]\npub async fn all_for(\n    db: &impl ConnectionTrait,\n    user: &User,\n    page: Page,\n) -> Result<(Vec<Model>, bool), AppError> {{\n    let per = page.per();\n    let mut rows = Entity::find()\n        .filter(Column::{oc}.eq(user.id))\n        .order_by_desc(Column::CreatedAt)\n        .order_by_desc(Column::Id)\n        .limit(per + 1)\n        .offset(page.offset())\n        .all(db)\n        .await?;\n    let more = rows.len() as u64 > per;\n    rows.truncate(per as usize);\n    Ok((rows, more))\n}}\n\n",
            n.lower_plural
        ));
        s.push_str(&format!(
            "/// `user`'s {} for a `<select>`: (id, label), at most 200, by label.\n#[tracing::instrument(skip_all)]\npub async fn choices_for(db: &impl ConnectionTrait, user: &User) -> Result<Vec<(i64, String)>, AppError> {{\n    let rows = Entity::find()\n        .filter(Column::{oc}.eq(user.id))\n        .order_by_desc(Column::Id)\n        .limit(200)\n        .all(db)\n        .await?;\n    let mut out: Vec<(i64, String)> = rows.iter().map(|r| (r.id, r.label())).collect();\n    out.sort_by(|a, b| a.1.cmp(&b.1));\n    Ok(out)\n}}\n\n",
            n.lower_plural
        ));
        s.push_str(&self.map_unique_fn());
        s.push_str(&self.check_refs_fn());
        s.push_str(&format!(
            "/// A new {} owned by `user`.\n#[tracing::instrument(skip_all)]\npub async fn create(db: &impl ConnectionTrait, user: &User, new: New{m}) -> Result<Model, AppError> {{\n{refs}    let now = Utc::now();\n    ActiveModel {{\n        {o}: Set(user.id),\n{}        created_at: Set(now),\n        updated_at: Set(now),\n        ..Default::default()\n    }}\n    .insert(db)\n    .await\n    .map_err({})\n}}\n\n",
            n.lower,
            self.literal_sets(),
            self.db_err()
        ));
        s.push_str(&format!(
            "/// Saves `user`'s changes to their own {} (`find_for` already scoped it).\n#[tracing::instrument(skip_all, fields(id = record.id))]\npub async fn update_for(\n    db: &impl ConnectionTrait,\n    user: &User,\n    record: Model,\n    new: New{m},\n) -> Result<Model, AppError> {{\n    if record.{o} != user.id {{\n        return Err(AppError::NotFound);\n    }}\n{refs}    let mut am: ActiveModel = record.into();\n{}    am.updated_at = Set(Utc::now());\n    am.update(db).await.map_err({})\n}}\n\n",
            n.lower,
            self.set_fields("am"),
            self.db_err()
        ));
        s.push_str(&format!(
            "/// Deletes `user`'s own {}.\n#[tracing::instrument(skip_all, fields(id = record.id))]\npub async fn destroy_for(db: &impl ConnectionTrait, user: &User, record: Model) -> Result<(), AppError> {{\n    if record.{o} != user.id {{\n        return Err(AppError::NotFound);\n    }}\n    Entity::delete_by_id(record.id).exec(db).await?;\n    Ok(())\n}}\n\n",
            n.lower
        ));
        s
    }

    fn public_queries(&self) -> String {
        let n = &self.names;
        let m = &n.model;
        let refs = if self.targets.is_empty() {
            ""
        } else {
            "    check_references(db, &new).await?;\n"
        };
        let mut s = String::new();
        s.push_str(&format!(
            "/// Public: anyone may read any {} (`--public`; .offrails/guides/models.md#scoped-lookups-tenancy).\n#[tracing::instrument(skip_all, fields(id))]\npub async fn find(db: &impl ConnectionTrait, id: i64) -> Result<Model, AppError> {{\n    Entity::find_by_id(id).one(db).await?.ok_or(AppError::NotFound)\n}}\n\n",
            n.lower
        ));
        s.push_str(&format!(
            "/// A page of {}, newest first, and whether there's another page.\n#[tracing::instrument(skip_all)]\npub async fn all(db: &impl ConnectionTrait, page: Page) -> Result<(Vec<Model>, bool), AppError> {{\n    let per = page.per();\n    let mut rows = Entity::find()\n        .order_by_desc(Column::CreatedAt)\n        .order_by_desc(Column::Id)\n        .limit(per + 1)\n        .offset(page.offset())\n        .all(db)\n        .await?;\n    let more = rows.len() as u64 > per;\n    rows.truncate(per as usize);\n    Ok((rows, more))\n}}\n\n",
            n.lower_plural
        ));
        s.push_str(&format!(
            "/// {} for a `<select>`: (id, label), at most 200, by label.\n#[tracing::instrument(skip_all)]\npub async fn choices(db: &impl ConnectionTrait) -> Result<Vec<(i64, String)>, AppError> {{\n    let rows = Entity::find().order_by_desc(Column::Id).limit(200).all(db).await?;\n    let mut out: Vec<(i64, String)> = rows.iter().map(|r| (r.id, r.label())).collect();\n    out.sort_by(|a, b| a.1.cmp(&b.1));\n    Ok(out)\n}}\n\n",
            n.human_plural
        ));
        s.push_str(&self.map_unique_fn());
        s.push_str(&self.check_refs_fn());
        s.push_str(&format!(
            "#[tracing::instrument(skip_all)]\npub async fn create(db: &impl ConnectionTrait, new: New{m}) -> Result<Model, AppError> {{\n{refs}    let now = Utc::now();\n    ActiveModel {{\n{}        created_at: Set(now),\n        updated_at: Set(now),\n        ..Default::default()\n    }}\n    .insert(db)\n    .await\n    .map_err({})\n}}\n\n",
            self.literal_sets(),
            self.db_err()
        ));
        s.push_str(&format!(
            "#[tracing::instrument(skip_all, fields(id = record.id))]\npub async fn update(db: &impl ConnectionTrait, record: Model, new: New{m}) -> Result<Model, AppError> {{\n{refs}    let mut am: ActiveModel = record.into();\n{}    am.updated_at = Set(Utc::now());\n    am.update(db).await.map_err({})\n}}\n\n",
            self.set_fields("am"),
            self.db_err()
        ));
        s.push_str(
            "#[tracing::instrument(skip_all, fields(id = record.id))]\npub async fn destroy(db: &impl ConnectionTrait, record: Model) -> Result<(), AppError> {\n    Entity::delete_by_id(record.id).exec(db).await?;\n    Ok(())\n}\n\n",
        );
        s
    }

    /// Validation units, inline (`cargo nextest run` runs them with the rest).
    fn unit_tests(&self) -> String {
        let m = &self.names.model;
        let mut s = String::from("#[cfg(test)]\nmod tests {\n    use super::*;\n\n");
        s.push_str(&format!(
            "    fn valid() -> {m}Params {{\n        {m}Params {{\n"
        ));
        for f in &self.fields {
            s.push_str(&format!(
                "            {}: {}.into(),\n",
                f.param(),
                sample(f, false)
            ));
        }
        s.push_str("        }\n    }\n\n");
        s.push_str("    #[test]\n    fn valid_params_validate() {\n        assert!(valid().validate().is_ok(), \"{:?}\", valid().validate());\n    }\n\n");
        let required: Vec<&Field> = self
            .fields
            .iter()
            .filter(|f| !f.nullable && f.kind != Kind::Boolean)
            .collect();
        if !required.is_empty() {
            s.push_str(&format!(
                "    #[test]\n    fn required_fields_cant_be_blank() {{\n        let errors = {m}Params::default().validate().unwrap_err();\n"
            ));
            for f in &required {
                let msg = if f.is_reference() {
                    "must exist"
                } else {
                    "can't be blank"
                };
                s.push_str(&format!(
                    "        assert_eq!(errors.on(\"{}\"), [\"{msg}\"]);\n",
                    f.name
                ));
            }
            s.push_str("    }\n\n");
        }
        let strings: Vec<&Field> = self
            .fields
            .iter()
            .filter(|f| matches!(f.kind, Kind::String | Kind::Text))
            .collect();
        if !strings.is_empty() {
            s.push_str("    #[test]\n    fn strings_have_a_maximum_length() {\n");
            for f in &strings {
                let max = max_len(f);
                let rest = if self.fields.len() > 1 {
                    "            ..valid()\n"
                } else {
                    ""
                };
                s.push_str(&format!(
                    "        let at_max = {m}Params {{\n            {p}: \"x\".repeat({max}),\n{rest}        }};\n        assert!(at_max.validate().is_ok());\n        let over = {m}Params {{\n            {p}: \"x\".repeat({}),\n{rest}        }};\n        assert_eq!(\n            over.validate().unwrap_err().on(\"{n}\"),\n            [\"is too long (maximum is {max} characters)\"]\n        );\n",
                    max + 1,
                    p = f.param(),
                    n = f.name
                ));
            }
            s.push_str("    }\n\n");
        }
        let typed: Vec<&Field> = self
            .fields
            .iter()
            .filter(|f| garbage(f).is_some())
            .collect();
        if !typed.is_empty() {
            s.push_str("    #[test]\n    fn typed_fields_reject_garbage() {\n");
            for f in &typed {
                let (input, msg) = garbage(f).unwrap_or(("", ""));
                let rest = if self.fields.len() > 1 {
                    "            ..valid()\n"
                } else {
                    ""
                };
                s.push_str(&format!(
                    "        let bad = {m}Params {{\n            {p}: \"{input}\".into(),\n{rest}        }};\n        assert_eq!(bad.validate().unwrap_err().on(\"{n}\"), [\"{msg}\"]);\n",
                    p = f.param(),
                    n = f.name
                ));
            }
            s.push_str("    }\n\n");
        }
        if self.has(|f| f.kind == Kind::Money) {
            s.push_str("    #[test]\n    fn money_round_trips_as_cents() {\n        assert_eq!(parse_money(\"12\"), Some(1200));\n        assert_eq!(parse_money(\"12.3\"), Some(1230));\n        assert_eq!(parse_money(\"-0.05\"), Some(-5));\n        assert_eq!(parse_money(\"1.234\"), None);\n        assert_eq!(parse_money(\"abc\"), None);\n        assert_eq!(money_to_string(1230), \"12.30\");\n        assert_eq!(money_to_string(-5), \"-0.05\");\n    }\n\n");
        }
        s.push_str("}\n");
        s
    }

    // ---------- registration, factory, tests ----------

    pub fn register(&self, plan: &mut Plan) {
        let n = &self.names;
        plan.insert(
            "src/models/mod.rs",
            "model mods",
            format!("pub mod {};\n", n.singular),
        );
        plan.insert(
            "src/models/mod.rs",
            "model aliases",
            format!("pub type {} = {}::Model;\n", n.model, n.singular),
        );
        // The other side of each belongs_to (models.md#associations), when the parent is ours.
        let mut done: Vec<String> = Vec::new();
        for f in &self.fields {
            let Some(t) = self.target(f) else { continue };
            if !t.anchors || done.contains(&t.names.singular) {
                continue;
            }
            done.push(t.names.singular.clone());
            let path = t.entity.clone();
            plan.insert_at(
                &path,
                Anchor::Named("relations".into()),
                format!(
                    "#[sea_orm(has_many = \"super::{}::Entity\")]\n{},\n",
                    n.singular, n.plural_camel
                ),
            );
            plan.insert_at(
                &path,
                Anchor::Named("related".into()),
                format!(
                    "impl Related<super::{}::Entity> for Entity {{\n    fn to() -> RelationDef {{\n        Relation::{}.def()\n    }}\n}}\n\n",
                    n.singular, n.plural_camel
                ),
            );
        }
    }

    /// The factory: valid params through `validate` + `create`, so a test can't build an invalid row.
    pub fn factory(&self) -> String {
        let n = &self.names;
        let k = &self.krate;
        let m = &n.model;
        let user = self.scope == Scope::User;
        let user_arg = if user {
            format!(",\n    user: &{k}::models::User")
        } else {
            String::new()
        };
        let user_pass = if user { ", user" } else { "" };
        let mut params = String::new();
        let mut parents = String::new();
        for f in &self.fields {
            let value = if let Some(t) = self.target(f) {
                if f.nullable {
                    "String::new()".to_owned()
                } else {
                    parents.push_str(&format!(
                        "    let {} = super::{}(db{user_pass}).await;\n",
                        f.name, t.names.singular
                    ));
                    format!("{}.id.to_string()", f.name)
                }
            } else {
                factory_value(f, n)
            };
            params.push_str(&format!("        {}: {value},\n", f.param()));
        }
        let uses_n = params.contains("{n}") || params.contains("n.to_string()");
        let seq = if uses_n {
            "    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);\n    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);\n"
        } else {
            ""
        };
        let owner = if user { " owned by `user`" } else { "" };
        let unused = if parents.is_empty() {
            if user {
                "    let _ = (db, user); // no parent records to create\n"
            } else {
                "    let _ = db; // no parent records to create\n"
            }
        } else {
            ""
        };
        let create = if user {
            format!("{k}::models::{}::create(db, user, new)", n.singular)
        } else {
            format!("{k}::models::{}::create(db, new)", n.singular)
        };
        format!(
            "//! {human} test data (.offrails/guides/testing.md#factories-testsappfactoriesrs): valid params\n//! through `validate` + `create`, so a test can't build an invalid row. Generated by `{cmd}`.\n\n/// Valid {m}Params (unique values numbered), for forms and `{s}_with`.\npub async fn {s}_params(db: &sea_orm::DatabaseConnection{user_arg}) -> {k}::models::{s}::{m}Params {{\n{unused}{seq}{parents}    {k}::models::{s}::{m}Params {{\n{params}    }}\n}}\n\n/// A {lower}{owner}, through `{m}Params::validate` + `{s}::create`.\npub async fn {s}(db: &sea_orm::DatabaseConnection{user_arg}) -> {k}::models::{s}::Model {{\n    let params = {s}_params(db{user_pass}).await;\n    {s}_with(db{user_pass}, params).await\n}}\n\n/// A {lower}{owner} from these params (they must be valid).\npub async fn {s}_with(\n    db: &sea_orm::DatabaseConnection{user_arg},\n    params: {k}::models::{s}::{m}Params,\n) -> {k}::models::{s}::Model {{\n    let new = params.validate().expect(\"valid {lower} params\");\n    {create}.await.expect(\"create {lower}\")\n}}\n\n/// Reads a {lower} back through the entity (tests can't call the model-private `find`).\npub async fn reload_{s}(db: &sea_orm::DatabaseConnection, id: i64) -> Option<{k}::models::{s}::Model> {{\n    use sea_orm::EntityTrait;\n    {k}::models::{s}::Entity::find_by_id(id)\n        .one(db)\n        .await\n        .expect(\"reload {lower}\")\n}}\n",
            s = n.singular,
            lower = n.lower,
            human = n.human_plural,
            cmd = self.command,
        )
    }

    /// `tests/app/models/<singular>_test.rs`: the queries against a real database (both backends).
    pub fn model_test(&self) -> String {
        let n = &self.names;
        let k = &self.krate;
        let s = &n.singular;
        let m = &n.model;
        let user = self.scope == Scope::User;
        let mut out = format!(
            "//! `{s}` (src/models/{s}/) against the database: {}ordering, paging{}.\n//! Generated by `{}`.\n\nuse {k}::error::AppError;\nuse {k}::models::{{Page, {s}}};\n\nuse crate::factories;\nuse crate::support::TestApp;\n\n",
            if user { "scoping, " } else { "" },
            if self.uniques().is_empty() {
                ""
            } else {
                ", uniqueness"
            },
            self.command
        );
        if user {
            out.push_str(&format!(
                "#[tokio::test]\nasync fn find_for_sees_only_the_owners_{p}() {{\n    let app = TestApp::new().await;\n    let alice = factories::user(app.db(), \"alice@example.com\").await;\n    let bob = factories::user(app.db(), \"bob@example.com\").await;\n    let record = factories::{s}(app.db(), &alice).await;\n    assert_eq!({s}::find_for(app.db(), &alice, record.id).await.unwrap(), record);\n    assert!(matches!(\n        {s}::find_for(app.db(), &bob, record.id).await,\n        Err(AppError::NotFound)\n    ));\n    assert!(matches!(\n        {s}::destroy_for(app.db(), &bob, record.clone()).await,\n        Err(AppError::NotFound)\n    ));\n    assert!(factories::reload_{s}(app.db(), record.id).await.is_some());\n}}\n\n",
                p = n.plural
            ));
            out.push_str(&format!(
                "#[tokio::test]\nasync fn all_for_pages_newest_first() {{\n    let app = TestApp::new().await;\n    let alice = factories::user(app.db(), \"alice@example.com\").await;\n    let bob = factories::user(app.db(), \"bob@example.com\").await;\n    let first = factories::{s}(app.db(), &alice).await;\n    let second = factories::{s}(app.db(), &alice).await;\n    let third = factories::{s}(app.db(), &alice).await;\n    factories::{s}(app.db(), &bob).await;\n    let (rows, more) = {s}::all_for(app.db(), &alice, Page::new(1, 2)).await.unwrap();\n    assert_eq!(rows, vec![third, second]);\n    assert!(more);\n    let (rows, more) = {s}::all_for(app.db(), &alice, Page::new(2, 2)).await.unwrap();\n    assert_eq!(rows, vec![first]);\n    assert!(!more);\n}}\n\n"
            ));
            out.push_str(&format!(
                "#[tokio::test]\nasync fn update_for_and_destroy_for_change_the_owners_{s}() {{\n    let app = TestApp::new().await;\n    let alice = factories::user(app.db(), \"alice@example.com\").await;\n    let record = factories::{s}(app.db(), &alice).await;\n    let params = factories::{s}_params(app.db(), &alice).await;\n    let new = params.validate().unwrap();\n    let updated = {s}::update_for(app.db(), &alice, record.clone(), new).await.unwrap();\n    assert_eq!(updated.id, record.id);\n    assert!(updated.updated_at >= record.updated_at);\n    {s}::destroy_for(app.db(), &alice, updated).await.unwrap();\n    assert!(factories::reload_{s}(app.db(), record.id).await.is_none());\n}}\n\n"
            ));
        } else {
            out.push_str(&format!(
                "#[tokio::test]\nasync fn all_pages_newest_first() {{\n    let app = TestApp::new().await;\n    let first = factories::{s}(app.db()).await;\n    let second = factories::{s}(app.db()).await;\n    let third = factories::{s}(app.db()).await;\n    let (rows, more) = {s}::all(app.db(), Page::new(1, 2)).await.unwrap();\n    assert_eq!(rows, vec![third, second]);\n    assert!(more);\n    let (rows, more) = {s}::all(app.db(), Page::new(2, 2)).await.unwrap();\n    assert_eq!(rows, vec![first]);\n    assert!(!more);\n}}\n\n"
            ));
            out.push_str(&format!(
                "#[tokio::test]\nasync fn update_and_destroy() {{\n    let app = TestApp::new().await;\n    let record = factories::{s}(app.db()).await;\n    let new = factories::{s}_params(app.db()).await.validate().unwrap();\n    let updated = {s}::update(app.db(), record.clone(), new).await.unwrap();\n    assert_eq!(updated.id, record.id);\n    {s}::destroy(app.db(), updated).await.unwrap();\n    assert!(matches!({s}::find(app.db(), record.id).await, Err(AppError::NotFound)));\n}}\n\n"
            ));
        }
        let (who, owner_args) = if user {
            (
                "    let alice = factories::user(app.db(), \"alice@example.com\").await;\n",
                ", &alice",
            )
        } else {
            ("", "")
        };
        let update = if user {
            format!("{s}::update_for(app.db(), &alice, record.clone(), new)")
        } else {
            format!("{s}::update(app.db(), record.clone(), new)")
        };
        out.push_str(&format!(
            "/// The edit form shows what was saved: params from a record validate back to it.\n#[tokio::test]\nasync fn the_edit_form_round_trips_a_saved_{s}() {{\n    let app = TestApp::new().await;\n{who}    let record = factories::{s}(app.db(){owner_args}).await;\n    let new = {s}::{m}Params::from(&record).validate().unwrap();\n    let saved = {update}.await.unwrap();\n    assert_eq!(\n        {s}::Model {{\n            updated_at: record.updated_at,\n            ..saved\n        }},\n        record\n    );\n}}\n\n"
        ));
        let (who, owner_args) = if user {
            (
                "    let alice = factories::user(app.db(), \"alice@example.com\").await;\n",
                ", &alice",
            )
        } else {
            ("", "")
        };
        for f in self.uniques() {
            out.push_str(&format!(
                "#[tokio::test]\nasync fn a_duplicate_{c}_is_a_validation_error() {{\n    let app = TestApp::new().await;\n{who}    let taken = factories::{s}(app.db(){owner_args}).await;\n    let mut params = factories::{s}_params(app.db(){owner_args}).await;\n    params.{p} = {value};\n    let new = params.validate().unwrap();\n    match {s}::create(app.db(){owner_args}, new).await {{\n        Err(AppError::Invalid(errors)) => assert_eq!(errors.on(\"{name}\"), [\"has already been taken\"]),\n        other => panic!(\"expected a validation error, got {{other:?}}\"),\n    }}\n}}\n\n",
                c = f.column(),
                p = f.param(),
                name = f.name,
                value = taken_value(f, k, s)
            ));
            if user {
                out.push_str(&format!(
                    "/// Unique per owner: another user may use the same {lower} (and learns nothing).\n#[tokio::test]\nasync fn another_user_may_reuse_a_{c}() {{\n    let app = TestApp::new().await;\n    let alice = factories::user(app.db(), \"alice@example.com\").await;\n    let bob = factories::user(app.db(), \"bob@example.com\").await;\n    let taken = factories::{s}(app.db(), &alice).await;\n    let mut params = factories::{s}_params(app.db(), &bob).await;\n    params.{p} = {value};\n    factories::{s}_with(app.db(), &bob, params).await;\n}}\n\n",
                    c = f.column(),
                    p = f.param(),
                    lower = f.name.replace('_', " "),
                    value = taken_value(f, k, s)
                ));
            }
        }
        if user {
            for f in self.fields.iter().filter(|f| f.is_reference()) {
                let Some(t) = self.target(f) else { continue };
                out.push_str(&format!(
                    "/// A {lower} the user can't see can't be referenced (it looks missing).\n#[tokio::test]\nasync fn another_users_{ts}_cant_be_referenced() {{\n    let app = TestApp::new().await;\n    let alice = factories::user(app.db(), \"alice@example.com\").await;\n    let bob = factories::user(app.db(), \"bob@example.com\").await;\n    let theirs = factories::{ts}(app.db(), &bob).await;\n    let mut params = factories::{s}_params(app.db(), &alice).await;\n    params.{p} = theirs.id.to_string();\n    let new = params.validate().unwrap();\n    match {s}::create(app.db(), &alice, new).await {{\n        Err(AppError::Invalid(errors)) => assert_eq!(errors.on(\"{name}\"), [\"must exist\"]),\n        other => panic!(\"expected a validation error, got {{other:?}}\"),\n    }}\n}}\n\n",
                    lower = t.names.lower,
                    ts = t.names.singular,
                    p = f.param(),
                    name = f.name
                ));
            }
        }
        out
    }

    /// Model + migration + factory + model tests (what `generate model` writes).
    pub fn plan_into(&self, plan: &mut Plan, stamp: &str) {
        let n = &self.names;
        let module = format!("m{stamp}_create_{}", n.plural);
        plan.create(
            format!("migration/src/{module}.rs"),
            migration::create_table(n, &self.fields, self.owner.as_ref(), &self.command),
        );
        migration::register(plan, &module);
        let dir = self.dir();
        plan.create(format!("{dir}/mod.rs"), self.entity_file());
        plan.create(format!("{dir}/params.rs"), self.params_file());
        plan.create(format!("{dir}/queries.rs"), self.queries_file());
        self.register(plan);
        plan.create(
            format!("tests/app/factories/{}.rs", n.singular),
            self.factory(),
        );
        plan.insert(
            "tests/app/factories.rs",
            "factories",
            format!("mod {s};\npub use {s}::*;\n", s = n.singular),
        );
        plan.shared(
            "tests/app/models/mod.rs",
            "//! Model tests against the database (.offrails/guides/test-standards.md#what-each-suite-contains).\n//! One file per model, written by `cargo offrails generate model|resource`.\n\n// samples: model test mods\n",
        );
        plan.insert_for(
            "tests/app/models/mod.rs",
            "tests/app/main.rs",
            "test mods",
            "mod models;\n",
        );
        plan.insert(
            "tests/app/models/mod.rs",
            "model test mods",
            format!("mod {}_test;\n", n.singular),
        );
        plan.create(
            format!("tests/app/models/{}_test.rs", n.singular),
            self.model_test(),
        );
        if self.has(|f| matches!(f.kind, Kind::Decimal { .. })) {
            plan.cargo("rust_decimal", "rust_decimal");
            plan.cargo("sea-orm --features with-rust_decimal", "with-rust_decimal");
        }
    }
}

const MONEY_FNS: &str = "/// `\"12.30\"` → `1230` cents; at most two decimals, an optional `-`. `None`: not an amount.\npub fn parse_money(raw: &str) -> Option<i64> {\n    let raw = raw.trim().replace(',', \"\");\n    let (negative, digits) = match raw.strip_prefix('-') {\n        Some(rest) => (true, rest.to_owned()),\n        None => (false, raw),\n    };\n    let (whole, frac) = digits.split_once('.').unwrap_or((&digits, \"\"));\n    let ok = |s: &str| s.chars().all(|c| c.is_ascii_digit());\n    if whole.is_empty() && frac.is_empty() || !ok(whole) || !ok(frac) || frac.len() > 2 {\n        return None;\n    }\n    let whole: i64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };\n    let frac: i64 = format!(\"{frac:0<2}\").parse().ok()?;\n    let cents = whole.checked_mul(100)?.checked_add(frac)?;\n    Some(if negative { -cents } else { cents })\n}\n\n/// `1230` cents → `\"12.30\"`.\npub fn money_to_string(cents: i64) -> String {\n    let sign = if cents < 0 { \"-\" } else { \"\" };\n    let abs = cents.unsigned_abs();\n    format!(\"{sign}{}.{:02}\", abs / 100, abs % 100)\n}\n\n";

/// The longest a string field may be.
pub fn max_len(f: &Field) -> usize {
    match f.kind {
        Kind::Text => 100_000,
        _ => 255,
    }
}

/// The validation lines for one field (a local named like the column).
fn validate_field(f: &Field) -> String {
    let p = f.param();
    let c = f.column();
    let key = &f.name;
    match &f.kind {
        Kind::String | Kind::Text => {
            let max = max_len(f);
            if f.nullable {
                format!(
                    "        let {c} = self.{p}.trim();\n        if {c}.chars().count() > {max} {{\n            errors.add(\"{key}\", \"is too long (maximum is {max} characters)\");\n        }}\n"
                )
            } else {
                format!(
                    "        let {c} = self.{p}.trim();\n        if {c}.is_empty() {{\n            errors.add(\"{key}\", \"can't be blank\");\n        }} else if {c}.chars().count() > {max} {{\n            errors.add(\"{key}\", \"is too long (maximum is {max} characters)\");\n        }}\n"
                )
            }
        }
        Kind::Boolean => {
            if f.nullable {
                format!(
                    "        let {c} = match self.{p}.trim() {{\n            \"\" => None,\n            \"true\" | \"on\" | \"1\" => Some(true),\n            \"false\" | \"off\" | \"0\" => Some(false),\n            _ => {{\n                errors.add(\"{key}\", \"is not included in the list\");\n                None\n            }}\n        }};\n"
                )
            } else {
                format!(
                    "        // A checkbox sends nothing when unchecked.\n        let {c} = matches!(self.{p}.trim(), \"true\" | \"on\" | \"1\");\n"
                )
            }
        }
        _ => {
            let (parse, msg, placeholder) = parse_of(f);
            let blank = if f.is_reference() {
                "must exist"
            } else {
                "can't be blank"
            };
            if f.nullable {
                format!(
                    "        let {c} = match self.{p}.trim() {{\n            \"\" => None,\n            raw => match {parse} {{\n                Some(v) => Some(v),\n                None => {{\n                    errors.add(\"{key}\", \"{msg}\");\n                    None\n                }}\n            }},\n        }};\n"
                )
            } else {
                format!(
                    "        let {c} = match self.{p}.trim() {{\n            \"\" => {{\n                errors.add(\"{key}\", \"{blank}\");\n                {placeholder}\n            }}\n            raw => match {parse} {{\n                Some(v) => v,\n                None => {{\n                    errors.add(\"{key}\", \"{msg}\");\n                    {placeholder}\n                }}\n            }},\n        }};\n"
                )
            }
        }
    }
}

/// (expression on `raw` giving `Option<T>`, message when it's `None`, placeholder value).
fn parse_of(f: &Field) -> (String, &'static str, &'static str) {
    match &f.kind {
        Kind::Integer => ("raw.parse::<i32>().ok()".into(), "is not a number", "0"),
        Kind::BigInt => ("raw.parse::<i64>().ok()".into(), "is not a number", "0"),
        Kind::References { .. } => (
            "raw.parse::<i64>().ok().filter(|id| *id > 0)".into(),
            "must exist",
            "0",
        ),
        Kind::Money => ("parse_money(raw)".into(), "is not an amount", "0"),
        Kind::Decimal { precision, scale } => {
            let limit = 10_i64.pow(precision - scale);
            (
                format!(
                    "raw.parse::<Decimal>()\n                .ok()\n                .filter(|d| d.round_dp({scale}) == *d && d.abs() < Decimal::from({limit}_i64))"
                ),
                "is not a number with that many digits",
                "Decimal::ZERO",
            )
        }
        Kind::DateTime => (
            "NaiveDateTime::parse_from_str(raw, \"%Y-%m-%dT%H:%M\")\n                .or_else(|_| NaiveDateTime::parse_from_str(raw, \"%Y-%m-%dT%H:%M:%S\"))\n                .ok()\n                .map(|t| t.and_utc())".into(),
            "is not a date and time",
            "DateTimeUtc::default()",
        ),
        Kind::Date => (
            "Date::parse_from_str(raw, \"%Y-%m-%d\").ok()".into(),
            "is not a date",
            "Date::default()",
        ),
        Kind::Uuid => ("Uuid::parse_str(raw).ok()".into(), "is not a UUID", "Uuid::nil()"),
        Kind::Json => (
            "serde_json::from_str::<Json>(raw).ok()".into(),
            "is not valid JSON",
            "Json::Null",
        ),
        Kind::String | Kind::Text | Kind::Boolean => (String::new(), "", ""),
    }
}

/// The `New` field value from the validated local.
fn new_value(f: &Field) -> String {
    let c = f.column();
    match f.kind {
        Kind::String | Kind::Text if f.nullable => {
            format!("(!{c}.is_empty()).then(|| {c}.to_owned())")
        }
        Kind::String | Kind::Text => format!("{c}.to_owned()"),
        Kind::Json if !f.nullable => format!("{c}.clone()"),
        _ => c,
    }
}

/// The `Params` string for a saved value (the edit form's prefill).
fn to_param(f: &Field) -> String {
    let c = f.column();
    let opt = f.nullable;
    match f.kind {
        Kind::String | Kind::Text => {
            if opt {
                format!("r.{c}.clone().unwrap_or_default()")
            } else {
                format!("r.{c}.clone()")
            }
        }
        Kind::Boolean => {
            if opt {
                format!("r.{c}.map(|b| b.to_string()).unwrap_or_default()")
            } else {
                format!("if r.{c} {{ \"true\".into() }} else {{ String::new() }}")
            }
        }
        Kind::Money => {
            if opt {
                format!("r.{c}.map(money_to_string).unwrap_or_default()")
            } else {
                format!("money_to_string(r.{c})")
            }
        }
        Kind::DateTime => {
            if opt {
                format!(
                    "r.{c}.map(|t| t.format(\"%Y-%m-%dT%H:%M\").to_string()).unwrap_or_default()"
                )
            } else {
                format!("r.{c}.format(\"%Y-%m-%dT%H:%M\").to_string()")
            }
        }
        Kind::Date => {
            if opt {
                format!("r.{c}.map(|d| d.format(\"%Y-%m-%d\").to_string()).unwrap_or_default()")
            } else {
                format!("r.{c}.format(\"%Y-%m-%d\").to_string()")
            }
        }
        Kind::Json => {
            if opt {
                format!("r.{c}.as_ref().map(|v| v.to_string()).unwrap_or_default()")
            } else {
                format!("r.{c}.to_string()")
            }
        }
        // WHY: SQLite stores decimals as REAL; round back to the column's scale.
        Kind::Decimal { scale, .. } => {
            if opt {
                format!("r.{c}.map(|d| d.round_dp({scale}).to_string()).unwrap_or_default()")
            } else {
                format!("r.{c}.round_dp({scale}).to_string()")
            }
        }
        _ => {
            if opt {
                format!("r.{c}.map(|v| v.to_string()).unwrap_or_default()")
            } else {
                format!("r.{c}.to_string()")
            }
        }
    }
}

/// A valid typed-in value, for unit tests (`unique`: not used here).
pub fn sample(f: &Field, _unique: bool) -> String {
    match &f.kind {
        Kind::String => format!("\"{}\"", f.human()),
        Kind::Text => format!("\"{} text\"", f.human()),
        Kind::Integer | Kind::BigInt => "\"42\"".into(),
        Kind::Boolean => "\"true\"".into(),
        Kind::Decimal { scale, .. } => {
            if *scale == 0 {
                "\"12\"".into()
            } else {
                format!("\"12.{}\"", "5".repeat(*scale as usize))
            }
        }
        Kind::Money => "\"19.99\"".into(),
        Kind::DateTime => "\"2026-01-15T09:30\"".into(),
        Kind::Date => "\"2026-01-15\"".into(),
        Kind::Uuid => "\"67e55044-10b1-426f-9247-bb680e5fe0c8\"".into(),
        Kind::Json => "r#\"{\"key\": \"value\"}\"#".into(),
        Kind::References { .. } => "\"1\"".into(),
    }
}

/// (input that doesn't parse, its message), for typed fields.
fn garbage(f: &Field) -> Option<(&'static str, &'static str)> {
    match &f.kind {
        Kind::Integer | Kind::BigInt => Some(("twelve", "is not a number")),
        Kind::Money => Some(("12.345", "is not an amount")),
        Kind::Decimal { .. } => Some(("1.2.3", "is not a number with that many digits")),
        Kind::DateTime => Some(("yesterday", "is not a date and time")),
        Kind::Date => Some(("2026-02-30", "is not a date")),
        Kind::Uuid => Some(("not-a-uuid", "is not a UUID")),
        Kind::Json => Some(("{oops", "is not valid JSON")),
        Kind::References { .. } => Some(("abc", "must exist")),
        _ => None,
    }
}

/// A factory value (a `String` expression; `n` numbers unique ones).
fn factory_value(f: &Field, names: &Names) -> String {
    let unique = f.unique;
    let name = f.name.as_str();
    match &f.kind {
        Kind::String => {
            if name.contains("email") {
                "format!(\"person{n}@example.com\")".into()
            } else if name.contains("slug") {
                format!("format!(\"{}-{{n}}\")", names.singular.replace('_', "-"))
            } else if name.contains("url") {
                "format!(\"https://example.com/{n}\")".into()
            } else {
                format!("format!(\"{} {{n}}\")", f.human())
            }
        }
        Kind::Text => format!("format!(\"{} {{n}}, long enough to read.\")", f.human()),
        Kind::Integer | Kind::BigInt if unique => "n.to_string()".into(),
        Kind::Uuid => "uuid::Uuid::new_v4().to_string()".into(),
        _ if unique => "n.to_string()".into(),
        _ => format!("{}.into()", sample(f, false)),
    }
}

/// The `Params` value equal to `taken`'s, for the duplicate test.
fn taken_value(f: &Field, k: &str, s: &str) -> String {
    let c = f.column();
    match f.kind {
        Kind::Money if f.nullable => {
            format!("taken.{c}.map({k}::models::{s}::money_to_string).unwrap_or_default()")
        }
        Kind::Money => format!("{k}::models::{s}::money_to_string(taken.{c})"),
        Kind::Decimal { scale, .. } if f.nullable => {
            format!("taken.{c}.map(|d| d.round_dp({scale}).to_string()).unwrap_or_default()")
        }
        Kind::Decimal { scale, .. } => format!("taken.{c}.round_dp({scale}).to_string()"),
        Kind::DateTime if f.nullable => format!(
            "taken.{c}.map(|t| t.format(\"%Y-%m-%dT%H:%M\").to_string()).unwrap_or_default()"
        ),
        Kind::DateTime => format!("taken.{c}.format(\"%Y-%m-%dT%H:%M\").to_string()"),
        Kind::String if f.nullable => format!("taken.{c}.clone().unwrap_or_default()"),
        Kind::String => format!("taken.{c}.clone()"),
        _ if f.nullable => format!("taken.{c}.map(|v| v.to_string()).unwrap_or_default()"),
        _ => format!("taken.{c}.to_string()"),
    }
}

/// `generate model <Name> field:type…`.
pub fn plan(info: &AppInfo, args: &[String], flags: &Flags) -> Result<Plan> {
    let name = super::names::Names::new(args.first().map(String::as_str).unwrap_or(""))?;
    let mut plan = Plan::new("model", &name.singular, args);
    let spec = ModelSpec::resolve(info, args, flags, &plan.command())?;
    let stamp = flags
        .recorded("stamp")
        .unwrap_or_else(|| migration::stamp(info));
    plan.option("stamp", stamp.clone());
    plan.option("scope", spec.scope.as_str());
    spec.plan_into(&mut plan, &stamp);
    plan.next
        .push("cargo run -q -- db migrate   # then prove down(): db rollback && db migrate".into());
    plan.next.push(format!(
        "src/models/{}/: adjust the `// APP:` lines (what a {} is, validation limits, the label)",
        spec.names.singular, spec.names.lower
    ));
    Ok(plan)
}
