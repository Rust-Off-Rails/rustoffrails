//! `generate migration <name> [field:type…]` (`rails generate migration`), and the create-table
//! migration `model` and `resource` write. 64-bit ids and foreign keys (`offrails-check [ids]`),
//! every `up` with a `down` that undoes it, registered at the anchors in `migration/src/lib.rs`
//! (healed first if `sea-orm-cli migrate generate` removed them).

use std::time::{SystemTime, UNIX_EPOCH};

use super::Flags;
use super::app::AppInfo;
use super::fields::{self, Field};
use super::names::{self, Names};
use super::plan::{Anchor, Plan};
use crate::util::{Fail, Result};

/// The owner column a scoped table gets (`user_id`, or a `references{users}` field's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// `user_id` / `author_id`.
    pub column: String,
}

/// `YYYYMMDD_HHMMSS` now (UTC), after every migration already in the app.
pub fn stamp(info: &AppInfo) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut now = format_stamp(secs);
    let latest = existing(info).into_iter().max();
    if let Some(latest) = latest
        && latest >= now
    {
        // Same second (two generators in a row) or a clock behind the parts' stamps.
        now = bump(&latest);
    }
    now
}

/// The stamps of the app's migrations (`m20261016_000000_…`).
fn existing(info: &AppInfo) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(info.path("migration/src")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let rest = n.strip_prefix('m')?;
            (rest.len() > 15 && rest.as_bytes()[8] == b'_').then(|| rest[..15].to_owned())
        })
        .collect()
}

fn format_stamp(secs: u64) -> String {
    let date = crate::util::date_of(secs).replace('-', "");
    let s = secs % 86_400;
    format!("{date}_{:02}{:02}{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// One second after `stamp` (`20261016_005959` → `20261016_010000`), wrapping at a day.
fn bump(stamp: &str) -> String {
    let (date, time) = stamp.split_at(9);
    let t: u32 = time.parse().unwrap_or(0);
    let (h, m, s) = (t / 10_000, (t / 100) % 100, t % 100);
    let total = h * 3600 + m * 60 + s + 1;
    if total >= 86_400 {
        return format!("{date}235959");
    }
    format!(
        "{date}{:02}{:02}{:02}",
        total / 3600,
        (total / 60) % 60,
        total % 60
    )
}

/// The `DeriveIden` enum of a table: `blog_posts` → `BlogPosts`.
pub fn iden(table: &str) -> String {
    names::camel(table)
}

/// Registers migration module `module` in `migration/src/lib.rs`.
pub fn register(plan: &mut Plan, module: &str) {
    plan.insert(
        "migration/src/lib.rs",
        "migration mods",
        format!("mod {module};\n"),
    );
    plan.insert(
        "migration/src/lib.rs",
        "migrations",
        format!("Box::new({module}::Migration),\n"),
    );
    plan.migrations = true;
}

/// The create-table migration for `names` with `fields` (+ the owner column, timestamps).
pub fn create_table(
    names: &Names,
    fields: &[Field],
    owner: Option<&Owner>,
    command: &str,
) -> String {
    let t = iden(&names.plural);
    let table = &names.plural;
    let mut cols = format!("                .col(big_pk_auto({t}::Id))\n");
    let mut variants = vec!["Table".to_owned(), "Id".to_owned()];
    let mut fks = String::new();
    let mut indexes = String::new();
    let mut others: Vec<String> = Vec::new();
    let ref_idens = |target: &str, others: &mut Vec<String>| {
        let r = iden(target);
        if r != t && !others.contains(&r) {
            others.push(r.clone());
        }
        r
    };
    let index = |name: &str, columns: &[String], unique: bool| {
        let cols: String = columns
            .iter()
            .map(|c| format!("                .col({t}::{c})\n"))
            .collect();
        format!(
            "        m.create_index(\n            Index::create()\n                .name(\"{name}\")\n                .table({t}::Table)\n{cols}{}                .to_owned(),\n        )\n        .await?;\n",
            if unique {
                "                .unique()\n"
            } else {
                ""
            }
        )
    };
    let fk = |column: &str, target: &str, nullable: bool| {
        let c = names::camel(column);
        let action = if nullable { "SetNull" } else { "Cascade" };
        format!(
            "                .foreign_key(\n                    ForeignKey::create()\n                        .name(\"{table}_{column}_fkey\")\n                        .from({t}::Table, {t}::{c})\n                        .to({target}::Table, {target}::Id)\n                        .on_delete(ForeignKeyAction::{action}),\n                )\n"
        )
    };
    if let Some(o) = owner {
        let c = names::camel(&o.column);
        cols.push_str(&format!("                .col(big_integer({t}::{c}))\n"));
        variants.push(c.clone());
        let users = ref_idens("users", &mut others);
        fks.push_str(&fk(&o.column, &users, false));
        indexes.push_str(&index(&format!("{table}_{}_idx", o.column), &[c], false));
    }
    for f in fields {
        if owner.is_some_and(|o| o.column == f.column()) {
            continue;
        }
        cols.push_str(&format!("                .col({})\n", f.migration_col(&t)));
        variants.push(f.iden());
        if let Some(target) = f.target() {
            let r = ref_idens(&target.plural, &mut others);
            fks.push_str(&fk(&f.column(), &r, f.nullable));
        }
        if f.unique {
            let mut columns = Vec::new();
            if let Some(o) = owner {
                // Unique per owner: a global index would tell one tenant another's values.
                columns.push(names::camel(&o.column));
            }
            columns.push(f.iden());
            indexes.push_str(&index(
                &format!("{table}_{}_key", f.column()),
                &columns,
                true,
            ));
        } else if f.index || f.is_reference() {
            indexes.push_str(&index(
                &format!("{table}_{}_idx", f.column()),
                &[f.iden()],
                false,
            ));
        }
    }
    cols.push_str(&format!(
        "                .col(timestamp_with_time_zone({t}::CreatedAt))\n                .col(timestamp_with_time_zone({t}::UpdatedAt))\n"
    ));
    variants.push("CreatedAt".into());
    variants.push("UpdatedAt".into());
    let mut idens = format!(
        "#[derive(DeriveIden)]\nenum {t} {{\n{}}}\n",
        variants
            .iter()
            .map(|v| format!("    {v},\n"))
            .collect::<String>()
    );
    for o in &others {
        idens.push_str(&format!(
            "\n#[derive(DeriveIden)]\nenum {o} {{\n    Table,\n    Id,\n}}\n"
        ));
    }
    let up_tail = if indexes.is_empty() {
        "        .await\n".to_owned()
    } else {
        format!("        .await?;\n{indexes}        Ok(())\n")
    };
    format!(
        "//! Creates `{table}`. Generated by `{command}`.\n//! APP: committed migrations are append-only: change the table with a new migration\n//! (.offrails/guides/migrations.md#rules).\n\nuse sea_orm_migration::{{prelude::*, schema::*}};\n\n#[derive(DeriveMigrationName)]\npub struct Migration;\n\n#[async_trait::async_trait]\nimpl MigrationTrait for Migration {{\n    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {{\n        m.create_table(\n            Table::create()\n                .table({t}::Table)\n                .if_not_exists()\n{cols}{fks}                .to_owned(),\n        )\n{up_tail}    }}\n\n    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {{\n        m.drop_table(Table::drop().table({t}::Table).to_owned())\n            .await\n    }}\n}}\n\n{idens}"
    )
}

/// What a migration name asks for (Rails' conventions).
enum Shape {
    Create(String),
    Add(String),
    Remove(String),
    Blank,
}

fn shape(name: &str) -> Shape {
    if let Some(t) = name.strip_prefix("create_") {
        return Shape::Create(t.to_owned());
    }
    if let Some(rest) = name.strip_prefix("add_")
        && let Some((_, t)) = rest.rsplit_once("_to_")
    {
        return Shape::Add(t.to_owned());
    }
    if let Some(rest) = name.strip_prefix("remove_")
        && let Some((_, t)) = rest.rsplit_once("_from_")
    {
        return Shape::Remove(t.to_owned());
    }
    Shape::Blank
}

/// Whether some migration defines the table's iden enum (it exists, or will).
fn table_known(info: &AppInfo, table: &str) -> bool {
    let needle = format!("enum {} {{", iden(table));
    std::fs::read_dir(info.path("migration/src"))
        .map(|d| {
            d.flatten()
                .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains(&needle)))
        })
        .unwrap_or(false)
}

fn alter_add(t: &str, f: &Field) -> String {
    let mut col = f.migration_col(t);
    if let Some(d) = f.add_column_default() {
        col = format!("{col}.default({d}).take()");
    }
    format!(
        "        m.alter_table(\n            Table::alter()\n                .table({t}::Table)\n                .add_column({col})\n                .to_owned(),\n        )\n        .await?;\n"
    )
}

fn alter_drop(t: &str, f: &Field) -> String {
    format!(
        "        m.alter_table(\n            Table::alter()\n                .table({t}::Table)\n                .drop_column({t}::{})\n                .to_owned(),\n        )\n        .await?;\n",
        f.iden()
    )
}

fn add_index(table: &str, t: &str, f: &Field) -> Option<(String, String)> {
    if !(f.unique || f.index || f.is_reference()) {
        return None;
    }
    let (suffix, unique) = if f.unique {
        ("key", ".unique()")
    } else {
        ("idx", "")
    };
    let name = format!("{table}_{}_{suffix}", f.column());
    let create = format!(
        "        m.create_index(\n            Index::create()\n                .name(\"{name}\")\n                .table({t}::Table)\n                .col({t}::{}){unique}\n                .to_owned(),\n        )\n        .await?;\n",
        f.iden()
    );
    let drop = format!(
        "        m.drop_index(Index::drop().name(\"{name}\").table({t}::Table).to_owned())\n            .await?;\n"
    );
    Some((create, drop))
}

/// An `add_<x>_to_<table>` / `remove_<x>_from_<table>` migration body (`add` true: up adds).
fn alter_columns(table: &str, fields: &[Field], add: bool, command: &str) -> Result<String> {
    let t = iden(table);
    let mut adds = String::new();
    let mut drops = String::new();
    for f in fields {
        if !f.nullable && f.add_column_default().is_none() {
            return Err(Fail::usage(format!(
                "{}: a required {} column can't be added to a table that may have rows (no default); \
                 add it as `{}?`, backfill it, then make it required in another migration",
                f.name,
                f.spec(),
                f.spec()
            )));
        }
        let idx = add_index(table, &t, f);
        adds.push_str(&alter_add(&t, f));
        if let Some((create, _)) = &idx {
            adds.push_str(create);
        }
        if let Some((_, drop)) = &idx {
            drops.push_str(drop);
        }
        drops.push_str(&alter_drop(&t, f));
    }
    let (up, down) = if add { (adds, drops) } else { (drops, adds) };
    let refs = if fields.iter().any(Field::is_reference) {
        "        // WHY: no FOREIGN KEY constraint: SQLite can't add one to an existing table. The model\n        // checks the reference exists (and belongs to the actor) before saving.\n"
    } else {
        ""
    };
    let variants: String = fields
        .iter()
        .map(|f| format!("    {},\n", f.iden()))
        .collect();
    Ok(format!(
        "//! {} `{table}` columns. Generated by `{command}`.\n//! One ALTER per column: SQLite runs one change per statement.\n\nuse sea_orm_migration::{{prelude::*, schema::*}};\n\n#[derive(DeriveMigrationName)]\npub struct Migration;\n\n#[async_trait::async_trait]\nimpl MigrationTrait for Migration {{\n    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {{\n{refs}{up}        Ok(())\n    }}\n\n    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {{\n{down}        Ok(())\n    }}\n}}\n\n#[derive(DeriveIden)]\nenum {t} {{\n    Table,\n{variants}}}\n",
        if add { "Adds" } else { "Removes" }
    ))
}

fn blank(name: &str, command: &str) -> String {
    format!(
        "//! `{name}`. Generated by `{command}`.\n//! APP: write `up`, and a `down` that restores exactly the schema before it\n//! (.offrails/guides/migrations.md#rules). Prove both: db migrate, db rollback, db migrate.\n\nuse sea_orm_migration::prelude::*;\n\n#[derive(DeriveMigrationName)]\npub struct Migration;\n\n#[async_trait::async_trait]\nimpl MigrationTrait for Migration {{\n    async fn up(&self, _m: &SchemaManager) -> Result<(), DbErr> {{\n        // APP: the change.\n        Ok(())\n    }}\n\n    async fn down(&self, _m: &SchemaManager) -> Result<(), DbErr> {{\n        // APP: its exact undo.\n        Ok(())\n    }}\n}}\n"
    )
}

/// `generate migration <name> [field:type…]`.
pub fn plan(info: &AppInfo, args: &[String], flags: &Flags) -> Result<Plan> {
    let Some(raw) = args.first() else {
        return Err(Fail::usage(
            "generate migration needs a name: create_<table> | add_<x>_to_<table> | remove_<x>_from_<table> | <anything>",
        ));
    };
    let name = names::snake(raw);
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(Fail::usage(format!(
            "'{raw}' isn't a usable migration name (letters, digits and _, starting with a letter)"
        )));
    }
    let fields = fields::parse_all(&args[1..], &["id", "created_at", "updated_at"])?;
    let mut plan = Plan::new("migration", &name, args);
    let stamp = flags.recorded("stamp").unwrap_or_else(|| stamp(info));
    plan.option("stamp", stamp.clone());
    let module = format!("m{stamp}_{name}");
    let command = plan.command();
    let content = match shape(&name) {
        Shape::Create(table) => {
            let names = Names::new(&table)?;
            if names.plural != table {
                return Err(Fail::usage(format!(
                    "tables are plural: create_{} (or `generate model {}`, which writes the model too)",
                    names.plural, names.model
                )));
            }
            if fields.is_empty() {
                plan.next.push(format!(
                    "add the columns to migration/src/{module}.rs (or re-run with field:type arguments)"
                ));
            }
            create_table(&names, &fields, None, &command)
        }
        Shape::Add(table) | Shape::Remove(table) if fields.is_empty() => {
            return Err(Fail::usage(format!(
                "{name} needs the columns: field:type … (the table is `{table}`)"
            )));
        }
        Shape::Add(table) => {
            if !table_known(info, &table) {
                return Err(Fail::usage(format!(
                    "no migration creates `{table}` (no `enum {} {{` in migration/src); check the table name",
                    iden(&table)
                )));
            }
            let body = alter_columns(&table, &fields, true, &command)?;
            entity_columns(info, &mut plan, &table, &fields);
            body
        }
        Shape::Remove(table) => {
            if !table_known(info, &table) {
                return Err(Fail::usage(format!(
                    "no migration creates `{table}` (no `enum {} {{` in migration/src)",
                    iden(&table)
                )));
            }
            plan.next.push(format!(
                "remove {} from the entity, params, forms and tests of `{table}` (src/models/)",
                fields
                    .iter()
                    .map(|f| f.column())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            alter_columns(&table, &fields, false, &command)?
        }
        Shape::Blank => {
            if !fields.is_empty() {
                return Err(Fail::usage(format!(
                    "fields only go with create_<table>, add_<x>_to_<table> or remove_<x>_from_<table>, not '{name}'"
                )));
            }
            blank(&name, &command)
        }
    };
    plan.create(format!("migration/src/{module}.rs"), content);
    register(&mut plan, &module);
    plan.next.insert(
        0,
        "cargo run -q -- db migrate && cargo run -q -- db rollback && cargo run -q -- db migrate   # prove down() too"
            .into(),
    );
    Ok(plan)
}

/// `add_x_to_<table>`: the new columns go into the generated entity too (at `samples: columns`).
fn entity_columns(info: &AppInfo, plan: &mut Plan, table: &str, fields: &[Field]) {
    let Ok(names) = Names::new(table) else { return };
    let Some((path, text)) = info.model_source(&names.singular) else {
        plan.next.push(format!(
            "add the new column(s) to the `{table}` entity (src/models/): the entity mirrors the migration"
        ));
        return;
    };
    let has_anchor = text.lines().any(|l| l.trim() == "// samples: columns");
    if has_anchor {
        let text: String = fields
            .iter()
            .map(|f| {
                let attr = f
                    .entity_attr()
                    .map(|a| format!("{a}\n"))
                    .unwrap_or_default();
                format!("{attr}pub {}: {},\n", f.column(), f.rust_type())
            })
            .collect();
        plan.insert_at(&path, Anchor::Named("columns".into()), text);
        plan.next.push(format!(
            "{path}: the entity has the new column(s); add them to {}Params, validate(), the form and the factory if people edit them",
            names.model
        ));
    } else {
        plan.next.push(format!(
            "add the new column(s) to the `{table}` entity (src/models/): the entity mirrors the migration"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_sort_after_and_bump() {
        assert_eq!(bump("20261016_005959"), "20261016_010000");
        assert_eq!(format_stamp(0), "19700101_000000");
        assert_eq!(format_stamp(86_399), "19700101_235959");
    }

    #[test]
    fn create_table_uses_64_bit_ids_and_names_its_constraints() {
        let names = Names::new("comments").unwrap();
        let fields = vec![
            Field::parse("post:references").unwrap(),
            Field::parse("body:text").unwrap(),
            Field::parse("slug:string:unique").unwrap(),
        ];
        let owner = Owner {
            column: "user_id".into(),
        };
        let m = create_table(
            &names,
            &fields,
            Some(&owner),
            "cargo offrails generate model comment",
        );
        assert!(m.contains(".col(big_pk_auto(Comments::Id))"));
        assert!(m.contains(".col(big_integer(Comments::PostId))"));
        assert!(m.contains(".name(\"comments_post_id_fkey\")"));
        assert!(m.contains(".to(Posts::Table, Posts::Id)"));
        assert!(m.contains(".name(\"comments_slug_key\")"));
        assert!(m.contains(".col(Comments::UserId)\n                .col(Comments::Slug)\n                .unique()"));
        assert!(m.contains("enum Users {\n    Table,\n    Id,\n}"));
        assert!(
            !m.contains("integer(Comments::PostId)") || m.contains("big_integer(Comments::PostId)")
        );
    }
}
