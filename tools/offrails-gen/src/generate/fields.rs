//! `name:type` arguments: one column each, mapped to its SeaORM migration helper, Rust type, form
//! control, validation and factory value (generators/scaffold.md#field-syntax).
//!
//! Syntax: `name:type`, then any of `?` (nullable; on the name or the type; `:optional` is the
//! same without a shell glob character), `!` or `:unique`, `:index`. `references` names its model (`author:references{users}`, default: the field name).
//! `decimal{precision.scale}` (or `{p,s}` quoted; default 10.2). Examples: `title:string`, `slug:string:unique`,
//! `published_at:datetime?`, `price:money`, `book:references`, `weight:decimal{8,3}?`.

use super::names::{self, Names};
use crate::util::{Fail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    String,
    Text,
    Integer,
    BigInt,
    Boolean,
    Decimal {
        precision: u32,
        scale: u32,
    },
    /// Integer cents in `<name>_cents` (models.md#enums-money-time-ids): never floats.
    Money,
    DateTime,
    Date,
    Uuid,
    Json,
    /// A `<name>_id` foreign key to `target`'s table, indexed.
    References {
        target: Box<Names>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// As typed, snake_case: `title`, `price` (money), `book` (references).
    pub name: String,
    pub kind: Kind,
    pub nullable: bool,
    pub unique: bool,
    pub index: bool,
}

pub const TYPES: &str = "string, text, integer, bigint, boolean, decimal{p.s}, money, datetime, date, uuid, json, references{model}";

impl Field {
    pub fn parse(raw: &str) -> Result<Field> {
        let usage = |why: &str| {
            Fail::usage(format!(
                "field '{raw}': {why}\n  syntax: name:type[?][:unique][:index], types: {TYPES}"
            ))
        };
        let mut parts = raw.split(':');
        let mut name = parts.next().unwrap_or_default().trim().to_owned();
        let Some(ty_raw) = parts.next() else {
            return Err(usage("no type"));
        };
        let mut nullable = false;
        let mut unique = false;
        let mut index = false;
        if let Some(n) = name.strip_suffix('?') {
            name = n.to_owned();
            nullable = true;
        }
        let mut ty = ty_raw.trim().to_owned();
        loop {
            if let Some(t) = ty.strip_suffix('?') {
                ty = t.to_owned();
                nullable = true;
            } else if let Some(t) = ty.strip_suffix('!') {
                ty = t.to_owned();
                unique = true;
            } else {
                break;
            }
        }
        for m in parts {
            match m.trim().trim_end_matches('?') {
                "unique" | "uniq" => unique = true,
                "index" => index = true,
                // Shell-safe spellings of `?` (zsh globs an unquoted `?`).
                "optional" | "null" | "nullable" => nullable = true,
                other => return Err(usage(&format!("unknown modifier ':{other}'"))),
            }
            if m.ends_with('?') {
                nullable = true;
            }
        }
        let (base, arg) = match ty.split_once('{') {
            Some((b, rest)) => {
                let Some(arg) = rest.strip_suffix('}') else {
                    return Err(usage("a '{' without its '}'"));
                };
                (b.trim().to_ascii_lowercase(), Some(arg.trim().to_owned()))
            }
            None => (ty.trim().to_ascii_lowercase(), None),
        };
        let snake = names::snake(&name);
        if snake.is_empty() || snake.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(usage("the name must start with a letter"));
        }
        if snake != name {
            return Err(usage(&format!("names are snake_case: write '{snake}'")));
        }
        if names::RUST_KEYWORDS.contains(&name.as_str()) {
            return Err(usage("the name is a Rust keyword; pick another"));
        }
        let kind = match base.as_str() {
            "string" | "str" => Kind::String,
            "text" => Kind::Text,
            "integer" | "int" => Kind::Integer,
            "bigint" | "big_integer" => Kind::BigInt,
            "boolean" | "bool" => Kind::Boolean,
            "decimal" => {
                let (precision, scale) = match &arg {
                    None => (10, 2),
                    Some(a) => {
                        // `{8,3}` or the shell-safe `{8.3}` (a shell brace-expands `{8,3}` unquoted).
                        let nums: Vec<&str> = a.split([',', '.']).map(str::trim).collect();
                        match nums.as_slice() {
                            [p, s] => match (p.parse::<u32>(), s.parse::<u32>()) {
                                (Ok(p), Ok(s)) if (1..=28).contains(&p) && s <= p => (p, s),
                                _ => {
                                    return Err(usage(
                                        "decimal{precision,scale}: 1 <= precision <= 28, scale <= precision",
                                    ));
                                }
                            },
                            _ => {
                                return Err(usage(
                                    "decimal takes {precision,scale}, e.g. decimal{10,2}",
                                ));
                            }
                        }
                    }
                };
                Kind::Decimal { precision, scale }
            }
            "money" => Kind::Money,
            "datetime" | "timestamp" | "timestamptz" => Kind::DateTime,
            "date" => Kind::Date,
            "uuid" => Kind::Uuid,
            "json" => Kind::Json,
            "references" | "reference" | "belongs_to" => {
                let target = Box::new(Names::new(arg.as_deref().unwrap_or(&name))?);
                Kind::References { target }
            }
            "rich_text" | "attachment" | "enum" => {
                return Err(usage(&format!(
                    "'{base}' isn't generated yet; use text (rich_text), or follow .offrails/generators/scaffold.md for {base}"
                )));
            }
            other => return Err(usage(&format!("unknown type '{other}'"))),
        };
        if arg.is_some() && !matches!(kind, Kind::Decimal { .. } | Kind::References { .. }) {
            return Err(usage("only decimal and references take {…}"));
        }
        let field = Field {
            name,
            kind,
            nullable,
            unique,
            index,
        };
        if matches!(field.kind, Kind::Integer) && field.column().ends_with("_id") {
            return Err(usage(
                "a 32-bit *_id column breaks Postgres (offrails-check [ids]); use references, or bigint",
            ));
        }
        if matches!(field.kind, Kind::Json | Kind::Text) && field.unique {
            return Err(usage("text and json columns can't be unique; use string"));
        }
        Ok(field)
    }

    /// The column and entity field: `price_cents` (money), `book_id` (references), else the name.
    pub fn column(&self) -> String {
        match &self.kind {
            Kind::Money => format!("{}_cents", self.name),
            Kind::References { .. } => format!("{}_id", self.name),
            _ => self.name.clone(),
        }
    }

    /// The form input's `name` and the `Params` field: `book_id` for references, else the name.
    pub fn param(&self) -> String {
        match &self.kind {
            Kind::References { .. } => format!("{}_id", self.name),
            _ => self.name.clone(),
        }
    }

    /// The `DeriveIden` variant: `PriceCents`.
    pub fn iden(&self) -> String {
        names::camel(&self.column())
    }

    pub fn human(&self) -> String {
        names::human(&self.name)
    }

    /// The entity field's Rust type.
    pub fn rust_type(&self) -> String {
        let t = self.value_type();
        if self.nullable {
            format!("Option<{t}>")
        } else {
            t.to_owned()
        }
    }

    /// The non-optional Rust type (names from `sea_orm::entity::prelude`).
    pub fn value_type(&self) -> &'static str {
        match self.kind {
            Kind::String | Kind::Text => "String",
            Kind::Integer => "i32",
            Kind::BigInt | Kind::Money | Kind::References { .. } => "i64",
            Kind::Boolean => "bool",
            Kind::Decimal { .. } => "Decimal",
            Kind::DateTime => "DateTimeUtc",
            Kind::Date => "Date",
            Kind::Uuid => "Uuid",
            Kind::Json => "Json",
        }
    }

    /// `#[sea_orm(...)]` for the entity field, when the Rust type alone doesn't say it.
    pub fn entity_attr(&self) -> Option<String> {
        match self.kind {
            Kind::Text => Some(r#"#[sea_orm(column_type = "Text")]"#.into()),
            Kind::Decimal { precision, scale } => Some(format!(
                r#"#[sea_orm(column_type = "Decimal(Some(({precision}, {scale})))")]"#
            )),
            _ => None,
        }
    }

    /// The `sea_orm_migration::schema` call for the column.
    pub fn migration_col(&self, table: &str) -> String {
        let null = if self.nullable { "_null" } else { "" };
        let col = format!("{table}::{}", self.iden());
        match self.kind {
            Kind::String => format!("string{null}({col})"),
            Kind::Text => format!("text{null}({col})"),
            Kind::Integer => format!("integer{null}({col})"),
            Kind::BigInt | Kind::Money | Kind::References { .. } => {
                format!("big_integer{null}({col})")
            }
            Kind::Boolean => format!("boolean{null}({col})"),
            Kind::Decimal { precision, scale } => {
                format!("decimal_len{null}({col}, {precision}, {scale})")
            }
            Kind::DateTime => format!("timestamp_with_time_zone{null}({col})"),
            Kind::Date => format!("date{null}({col})"),
            Kind::Uuid => format!("uuid{null}({col})"),
            Kind::Json => format!("json{null}({col})"),
        }
    }

    /// A column added to a table with rows needs a default when it's required.
    pub fn add_column_default(&self) -> Option<&'static str> {
        if self.nullable {
            return None;
        }
        match self.kind {
            Kind::String | Kind::Text => Some("\"\""),
            Kind::Integer | Kind::BigInt | Kind::Money => Some("0"),
            Kind::Boolean => Some("false"),
            _ => None,
        }
    }

    pub fn is_reference(&self) -> bool {
        matches!(self.kind, Kind::References { .. })
    }

    pub fn target(&self) -> Option<&Names> {
        match &self.kind {
            Kind::References { target } => Some(target),
            _ => None,
        }
    }

    /// The argument as it would be typed again (`book:references{books}?`).
    pub fn spec(&self) -> String {
        let ty = match &self.kind {
            Kind::String => "string".to_owned(),
            Kind::Text => "text".to_owned(),
            Kind::Integer => "integer".to_owned(),
            Kind::BigInt => "bigint".to_owned(),
            Kind::Boolean => "boolean".to_owned(),
            Kind::Decimal { precision, scale } => format!("decimal{{{precision},{scale}}}"),
            Kind::Money => "money".to_owned(),
            Kind::DateTime => "datetime".to_owned(),
            Kind::Date => "date".to_owned(),
            Kind::Uuid => "uuid".to_owned(),
            Kind::Json => "json".to_owned(),
            Kind::References { target } => format!("references{{{}}}", target.plural),
        };
        let mut s = format!("{}:{ty}", self.name);
        if self.nullable {
            s.push('?');
        }
        if self.unique {
            s.push_str(":unique");
        }
        if self.index {
            s.push_str(":index");
        }
        s
    }
}

/// Parses every field and checks them together: no duplicates, no reserved names.
pub fn parse_all(raw: &[String], reserved: &[&str]) -> Result<Vec<Field>> {
    let mut out: Vec<Field> = Vec::new();
    for r in raw {
        let f = Field::parse(r)?;
        for taken in [f.name.clone(), f.column(), f.param()] {
            if reserved.contains(&taken.as_str()) {
                return Err(Fail::usage(format!(
                    "field '{r}': '{taken}' is set by the generator (reserved: {})",
                    reserved.join(", ")
                )));
            }
            if out
                .iter()
                .any(|o| o.name == taken || o.column() == taken || o.param() == taken)
            {
                return Err(Fail::usage(format!(
                    "field '{r}': '{taken}' is given twice"
                )));
            }
        }
        out.push(f);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_type_and_modifier() {
        let f = Field::parse("published_at:datetime?").unwrap();
        assert_eq!((f.kind.clone(), f.nullable), (Kind::DateTime, true));
        assert_eq!(f.rust_type(), "Option<DateTimeUtc>");
        let f = Field::parse("slug:string:unique").unwrap();
        assert!(f.unique && !f.nullable);
        let f = Field::parse("email:string!").unwrap();
        assert!(f.unique);
        let f = Field::parse("note?:text").unwrap();
        assert!(f.nullable);
        let f = Field::parse("published_at:datetime:optional").unwrap();
        assert!(f.nullable);
        let f = Field::parse("price:money").unwrap();
        assert_eq!(
            (f.column(), f.param()),
            ("price_cents".into(), "price".into())
        );
        assert_eq!(f.migration_col("Items"), "big_integer(Items::PriceCents)");
        assert_eq!(
            Field::parse("w:decimal{8.3}").unwrap().migration_col("I"),
            "decimal_len(I::W, 8, 3)"
        );
        let f = Field::parse("weight:decimal{8,3}?").unwrap();
        assert_eq!(
            f.migration_col("Items"),
            "decimal_len_null(Items::Weight, 8, 3)"
        );
        let f = Field::parse("author:references{users}").unwrap();
        assert_eq!(f.column(), "author_id");
        assert_eq!(f.target().unwrap().singular, "user");
        let f = Field::parse("book:references:index").unwrap();
        assert_eq!(f.target().unwrap().plural, "books");
        assert_eq!(f.spec(), "book:references{books}:index");
    }

    #[test]
    fn refuses_what_would_break() {
        for bad in [
            "title",
            "Title:string",
            "type:string",
            "x:float",
            "owner_id:integer",
            "body:text:unique",
            "x:string{3}",
            "x:decimal{40,2}",
            "x:string:sorted",
        ] {
            assert!(Field::parse(bad).is_err(), "{bad}");
        }
        assert!(parse_all(&["id:string".into()], &["id"]).is_err());
        assert!(parse_all(&["a:string".into(), "a:text".into()], &[]).is_err());
        assert!(parse_all(&["book:references".into(), "book_id:bigint".into()], &[]).is_err());
    }
}
