//! The database, as a schema browser sees it: tables with row counts (and size on Postgres),
//! columns and types, indexes, foreign keys. Read-only, development only, and never a row's data.
//!
//! Catalog SQL lives here, in the kernel, with a branch per backend (guides/database.md#escape-hatch):
//! SQLite's `pragma_*` table functions, Postgres' `information_schema` and `pg_catalog`.

use sea_orm::{ConnectionTrait, DbBackend, DbErr, QueryResult, Statement};

pub struct Column {
    pub name: String,
    pub ty: String,
    pub nullable: bool,
    pub pk: bool,
}

pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

pub struct ForeignKey {
    pub columns: Vec<String>,
    pub table: String,
    pub to: Vec<String>,
    pub on_delete: String,
}

pub struct Table {
    pub name: String,
    pub rows: Option<i64>,
    pub bytes: Option<i64>,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub fks: Vec<ForeignKey>,
}

impl Table {
    /// Foreign-key columns no index starts with (the primary key counts as an index): every
    /// lookup or cascade from the parent scans this table.
    pub fn unindexed_fks(&self) -> Vec<&ForeignKey> {
        let pk: Vec<&str> = self
            .columns
            .iter()
            .filter(|c| c.pk)
            .map(|c| c.name.as_str())
            .collect();
        self.fks
            .iter()
            .filter(|fk| {
                let lead = &fk.columns;
                let covered_by = |cols: &[&str]| {
                    cols.len() >= lead.len() && lead.iter().zip(cols).all(|(a, b)| a == b)
                };
                !(covered_by(&pk)
                    || self.indexes.iter().any(|i| {
                        covered_by(&i.columns.iter().map(String::as_str).collect::<Vec<_>>())
                    }))
            })
            .collect()
    }
}

/// Only plain identifiers are ever interpolated (a `COUNT(*)` needs the table name in the SQL).
// SECURITY: names come from the catalog, and still only [A-Za-z0-9_] pass; nothing from a request
// reaches this SQL (test: only_plain_identifiers_reach_catalog_sql).
pub fn plain_ident(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn st(backend: DbBackend, sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(backend, sql, values)
}

fn get_s(r: &QueryResult, col: &str) -> String {
    r.try_get::<String>("", col).unwrap_or_default()
}

fn get_i(r: &QueryResult, col: &str) -> i64 {
    r.try_get::<i64>("", col)
        .or_else(|_| r.try_get::<i32>("", col).map(i64::from))
        .unwrap_or(0)
}

/// Table names, sorted.
pub async fn table_names(db: &impl ConnectionTrait) -> Result<Vec<String>, DbErr> {
    let backend = db.get_database_backend();
    let sql = match backend {
        DbBackend::Postgres => {
            "SELECT table_name::text AS name FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_type = 'BASE TABLE' ORDER BY 1"
        }
        _ => {
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY 1"
        }
    };
    let rows = db.query_all_raw(st(backend, sql, vec![])).await?;
    Ok(rows
        .iter()
        .map(|r| get_s(r, "name"))
        .filter(|n| plain_ident(n))
        .collect())
}

/// Row count (exact; a development database is small enough).
pub async fn row_count(db: &impl ConnectionTrait, table: &str) -> Option<i64> {
    if !plain_ident(table) {
        return None;
    }
    let backend = db.get_database_backend();
    let sql = format!("SELECT COUNT(*) AS n FROM \"{table}\"");
    let row = db.query_one_raw(st(backend, &sql, vec![])).await.ok()??;
    Some(get_i(&row, "n"))
}

/// Everything about every table.
pub async fn tables(db: &impl ConnectionTrait) -> Result<Vec<Table>, DbErr> {
    let mut out = Vec::new();
    for name in table_names(db).await? {
        out.push(table(db, &name).await?);
    }
    Ok(out)
}

pub async fn table(db: &impl ConnectionTrait, name: &str) -> Result<Table, DbErr> {
    let backend = db.get_database_backend();
    let mut t = Table {
        name: name.to_owned(),
        rows: row_count(db, name).await,
        bytes: None,
        columns: Vec::new(),
        indexes: Vec::new(),
        fks: Vec::new(),
    };
    match backend {
        DbBackend::Postgres => {
            let n = || vec![sea_orm::Value::from(name.to_owned())];
            for r in db
                .query_all_raw(st(
                    backend,
                    "SELECT column_name::text AS name, data_type::text AS ty, is_nullable::text AS nullable \
                     FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = $1 \
                     ORDER BY ordinal_position",
                    n(),
                ))
                .await?
            {
                t.columns.push(Column {
                    name: get_s(&r, "name"),
                    ty: get_s(&r, "ty"),
                    nullable: get_s(&r, "nullable") == "YES",
                    pk: false,
                });
            }
            let pk: Vec<String> = db
                .query_all_raw(st(
                    backend,
                    "SELECT kcu.column_name::text AS name FROM information_schema.table_constraints tc \
                     JOIN information_schema.key_column_usage kcu \
                       ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                     WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = current_schema() \
                       AND tc.table_name = $1 ORDER BY kcu.ordinal_position",
                    n(),
                ))
                .await?
                .iter()
                .map(|r| get_s(r, "name"))
                .collect();
            for c in &mut t.columns {
                c.pk = pk.contains(&c.name);
            }
            for r in db
                .query_all_raw(st(
                    backend,
                    "SELECT i.relname::text AS name, ix.indisunique AS uniq, \
                       array_to_string(ARRAY(SELECT a.attname FROM unnest(ix.indkey) WITH ORDINALITY k(attnum, ord) \
                         JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum ORDER BY k.ord), ',') AS cols \
                     FROM pg_index ix JOIN pg_class t ON t.oid = ix.indrelid JOIN pg_class i ON i.oid = ix.indexrelid \
                     JOIN pg_namespace ns ON ns.oid = t.relnamespace \
                     WHERE t.relname = $1 AND ns.nspname = current_schema() ORDER BY 1",
                    n(),
                ))
                .await?
            {
                t.indexes.push(Index {
                    name: get_s(&r, "name"),
                    columns: get_s(&r, "cols").split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect(),
                    unique: r.try_get::<bool>("", "uniq").unwrap_or(false),
                });
            }
            for r in db
                .query_all_raw(st(
                    backend,
                    "SELECT con.conname::text AS name, cl.relname::text AS ref_table, \
                       array_to_string(ARRAY(SELECT a.attname FROM unnest(con.conkey) WITH ORDINALITY k(attnum, ord) \
                         JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum ORDER BY k.ord), ',') AS cols, \
                       array_to_string(ARRAY(SELECT a.attname FROM unnest(con.confkey) WITH ORDINALITY k(attnum, ord) \
                         JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum ORDER BY k.ord), ',') AS ref_cols, \
                       con.confdeltype::text AS del \
                     FROM pg_constraint con JOIN pg_class t ON t.oid = con.conrelid \
                     JOIN pg_namespace ns ON ns.oid = t.relnamespace JOIN pg_class cl ON cl.oid = con.confrelid \
                     WHERE con.contype = 'f' AND t.relname = $1 AND ns.nspname = current_schema() ORDER BY 1",
                    n(),
                ))
                .await?
            {
                t.fks.push(ForeignKey {
                    columns: get_s(&r, "cols").split(',').map(str::to_owned).collect(),
                    table: get_s(&r, "ref_table"),
                    to: get_s(&r, "ref_cols").split(',').map(str::to_owned).collect(),
                    on_delete: match get_s(&r, "del").as_str() {
                        "c" => "cascade",
                        "n" => "set null",
                        "d" => "set default",
                        "r" => "restrict",
                        _ => "no action",
                    }
                    .to_owned(),
                });
            }
            t.bytes = db
                .query_one_raw(st(
                    backend,
                    "SELECT pg_total_relation_size(c.oid)::bigint AS n FROM pg_class c \
                     JOIN pg_namespace ns ON ns.oid = c.relnamespace WHERE c.relname = $1 AND ns.nspname = current_schema()",
                    n(),
                ))
                .await
                .ok()
                .flatten()
                .map(|r| get_i(&r, "n"));
        }
        _ => {
            let n = || vec![sea_orm::Value::from(name.to_owned())];
            for r in db
                .query_all_raw(st(
                    backend,
                    "SELECT name, type AS ty, \"notnull\" AS nn, pk FROM pragma_table_info(?)",
                    n(),
                ))
                .await?
            {
                t.columns.push(Column {
                    name: get_s(&r, "name"),
                    ty: get_s(&r, "ty").to_lowercase(),
                    nullable: get_i(&r, "nn") == 0,
                    pk: get_i(&r, "pk") > 0,
                });
            }
            let indexes = db
                .query_all_raw(st(
                    backend,
                    "SELECT name, \"unique\" AS uniq FROM pragma_index_list(?) ORDER BY name",
                    n(),
                ))
                .await?;
            for r in indexes {
                let iname = get_s(&r, "name");
                let cols = db
                    .query_all_raw(st(
                        backend,
                        "SELECT name FROM pragma_index_info(?) ORDER BY seqno",
                        vec![sea_orm::Value::from(iname.clone())],
                    ))
                    .await?
                    .iter()
                    .map(|c| get_s(c, "name"))
                    .collect();
                t.indexes.push(Index {
                    name: iname,
                    columns: cols,
                    unique: get_i(&r, "uniq") == 1,
                });
            }
            let mut fks: Vec<(i64, ForeignKey)> = Vec::new();
            for r in db
                .query_all_raw(st(
                    backend,
                    "SELECT id, \"table\" AS ref_table, \"from\" AS col, \"to\" AS ref_col, on_delete \
                     FROM pragma_foreign_key_list(?) ORDER BY id, seq",
                    n(),
                ))
                .await?
            {
                let id = get_i(&r, "id");
                let (col, to) = (get_s(&r, "col"), get_s(&r, "ref_col"));
                match fks.iter_mut().find(|(i, _)| *i == id) {
                    Some((_, fk)) => {
                        fk.columns.push(col);
                        fk.to.push(to);
                    }
                    None => fks.push((
                        id,
                        ForeignKey {
                            columns: vec![col],
                            table: get_s(&r, "ref_table"),
                            to: vec![to],
                            on_delete: get_s(&r, "on_delete").to_lowercase(),
                        },
                    )),
                }
            }
            t.fks = fks.into_iter().map(|(_, fk)| fk).collect();
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_identifiers_reach_catalog_sql() {
        assert!(plain_ident("lab_orders"));
        assert!(!plain_ident("x\"; DROP TABLE y; --"));
        assert!(!plain_ident(""));
        assert!(!plain_ident("has space"));
    }

    #[test]
    fn an_index_or_primary_key_covers_a_foreign_key() {
        let t = |indexes: Vec<Vec<&str>>| Table {
            name: "items".into(),
            rows: None,
            bytes: None,
            columns: vec![Column {
                name: "id".into(),
                ty: "bigint".into(),
                nullable: false,
                pk: true,
            }],
            indexes: indexes
                .into_iter()
                .map(|c| Index {
                    name: "i".into(),
                    columns: c.into_iter().map(str::to_owned).collect(),
                    unique: false,
                })
                .collect(),
            fks: vec![ForeignKey {
                columns: vec!["order_id".into()],
                table: "orders".into(),
                to: vec!["id".into()],
                on_delete: "cascade".into(),
            }],
        };
        assert_eq!(t(vec![]).unindexed_fks().len(), 1);
        assert_eq!(t(vec![vec!["order_id"]]).unindexed_fks().len(), 0);
        assert_eq!(t(vec![vec!["order_id", "sku"]]).unindexed_fks().len(), 0);
        assert_eq!(t(vec![vec!["sku", "order_id"]]).unindexed_fks().len(), 1);
    }
}
