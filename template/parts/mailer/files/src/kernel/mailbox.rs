//! The dev mailbox's storage (Swoosh's local mailbox): in development `Mailer::Local` stores every
//! email in `mailbox_messages`. It is a table, not process memory, so mail sent by a request, a
//! job worker or a task all lands in one place. The dev-mailbox sample's UI (`kernel/mailbox_web.rs`)
//! reads it at `/dev/mailbox`. Spec: .offrails/guides/mailers.md#the-dev-mailbox
//!
//! The table exists in every environment (one schema everywhere) and is written only by
//! `Mailer::Local`, which production never selects unless `MAILER=local` says so.
//! GUIDE: .offrails/guides/mailers.md#the-dev-mailbox-kernelmailboxrs-swooshs-local-mailbox

use chrono::Utc;
use lettre::message::Mailbox;
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde_json::json;

use crate::kernel::mailer::Email;

pub use message::{Column, Entity, Model};

pub mod message {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "mailbox_messages")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub to_addr: String,
        pub from_addr: String,
        pub cc: Json,
        pub bcc: Json,
        pub reply_to: Option<String>,
        pub subject: String,
        #[sea_orm(column_type = "Text")]
        pub html: String,
        #[sea_orm(column_type = "Text")]
        pub text: String,
        pub headers: Json,
        #[sea_orm(column_type = "Text")]
        pub eml: String,
        pub attachments: Json,
        pub read_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// How many messages development keeps (`trim`, run by the jobs sample's cleanup job).
pub const KEEP: u64 = 500; // APP: how many messages development keeps

/// Stores what `Mailer::Local` just built: the decoded parts for the viewer, the exact RFC 822
/// message for Raw and `.eml`. Bcc is kept separately (it never appears in the sent headers).
pub async fn store(
    db: &impl ConnectionTrait,
    from: &Mailbox,
    email: &Email,
    message: &lettre::Message,
) -> Result<i64, DbErr> {
    let eml = String::from_utf8_lossy(&message.formatted()).into_owned();
    let row = message::ActiveModel {
        id: NotSet,
        to_addr: Set(email.to.trim().to_lowercase()),
        from_addr: Set(from.to_string()),
        cc: Set(json!(email.cc)),
        bcc: Set(json!(email.bcc)),
        reply_to: Set(email.reply_to.clone()),
        subject: Set(email.subject.clone()),
        html: Set(email.html.clone()),
        text: Set(email.text.clone()),
        headers: Set(json!(parse_headers(&eml))),
        eml: Set(eml),
        attachments: Set(json!(email.attachments)),
        read_at: Set(None),
        created_at: Set(Utc::now()),
    };
    Ok(Entity::insert(row).exec(db).await?.last_insert_id)
}

/// The newest `limit` messages, optionally only those whose subject, recipient, sender or text
/// contains `q` (case-insensitive; `%` and `_` are literal).
pub async fn search(db: &impl ConnectionTrait, q: &str, limit: u64) -> Result<Vec<Model>, DbErr> {
    use sea_orm::sea_query::{Expr, ExprTrait, Func, LikeExpr};
    let mut query = Entity::find().order_by_desc(Column::Id).limit(limit);
    let q = q.trim();
    if !q.is_empty() {
        let pattern = format!("%{}%", escape_like(&q.to_lowercase()));
        let like = |col: Column| {
            Expr::expr(Func::lower(Expr::col(col)))
                .like(LikeExpr::new(pattern.clone()).escape('\\'))
        };
        query = query.filter(
            like(Column::Subject)
                .or(like(Column::ToAddr))
                .or(like(Column::FromAddr))
                .or(like(Column::Text)),
        );
    }
    query.all(db).await
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// What `/dev/mailbox.json` and tests narrow by. Every field is optional.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Exact recipient (compared lowercased).
    pub to: Option<String>,
    /// Subject contains (case-insensitive; `%` and `_` are literal).
    pub subject: Option<String>,
    /// Only ids above this cursor.
    pub since: Option<i64>,
}

/// The newest `limit` messages matching `f`.
pub async fn filter(
    db: &impl ConnectionTrait,
    f: &Filter,
    limit: u64,
) -> Result<Vec<Model>, DbErr> {
    use sea_orm::sea_query::{Expr, ExprTrait, Func, LikeExpr};
    let mut query = Entity::find().order_by_desc(Column::Id).limit(limit);
    if let Some(to) = f.to.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        query = query.filter(Column::ToAddr.eq(to.to_lowercase()));
    }
    if let Some(subject) = f
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let pattern = format!("%{}%", escape_like(&subject.to_lowercase()));
        query = query.filter(
            Expr::expr(Func::lower(Expr::col(Column::Subject)))
                .like(LikeExpr::new(pattern).escape('\\')),
        );
    }
    if let Some(since) = f.since {
        query = query.filter(Column::Id.gt(since));
    }
    query.all(db).await
}

pub async fn find(db: &impl ConnectionTrait, id: i64) -> Result<Option<Model>, DbErr> {
    Entity::find_by_id(id).one(db).await
}

/// The newest message, optionally for one recipient (compared lowercased).
pub async fn latest(db: &impl ConnectionTrait, to: Option<&str>) -> Result<Option<Model>, DbErr> {
    let mut query = Entity::find();
    if let Some(to) = to.map(str::trim).filter(|t| !t.is_empty()) {
        query = query.filter(Column::ToAddr.eq(to.to_lowercase()));
    }
    query.order_by_desc(Column::Id).one(db).await
}

pub async fn mark_read(db: &impl ConnectionTrait, id: i64) -> Result<(), DbErr> {
    Entity::update_many()
        .col_expr(Column::ReadAt, sea_orm::sea_query::Expr::value(Utc::now()))
        .filter(Column::Id.eq(id))
        .filter(Column::ReadAt.is_null())
        .exec(db)
        .await?;
    Ok(())
}

/// Whether a message was there to delete.
pub async fn delete(db: &impl ConnectionTrait, id: i64) -> Result<bool, DbErr> {
    Ok(Entity::delete_by_id(id).exec(db).await?.rows_affected > 0)
}

pub async fn clear(db: &impl ConnectionTrait) -> Result<u64, DbErr> {
    Ok(Entity::delete_many().exec(db).await?.rows_affected)
}

/// (total, unread).
pub async fn counts(db: &impl ConnectionTrait) -> Result<(u64, u64), DbErr> {
    let total = Entity::find().count(db).await?;
    let unread = Entity::find()
        .filter(Column::ReadAt.is_null())
        .count(db)
        .await?;
    Ok((total, unread))
}

/// The newest id, or 0 for an empty mailbox: the live inbox's cursor.
pub async fn newest_id(db: &impl ConnectionTrait) -> Result<i64, DbErr> {
    let id: Option<i64> = Entity::find()
        .select_only()
        .column(Column::Id)
        .order_by_desc(Column::Id)
        .limit(1)
        .into_tuple()
        .one(db)
        .await?;
    Ok(id.unwrap_or(0))
}

/// Keeps the newest `keep` messages. `OFFSET` without `LIMIT` is a syntax error on SQLite, so the
/// cutoff is found with `offset(keep).limit(1)`.
pub async fn trim(db: &impl ConnectionTrait, keep: u64) -> Result<u64, DbErr> {
    let cutoff: Option<i64> = Entity::find()
        .select_only()
        .column(Column::Id)
        .order_by_desc(Column::Id)
        .offset(keep)
        .limit(1)
        .into_tuple()
        .one(db)
        .await?;
    let Some(cutoff) = cutoff else { return Ok(0) };
    Ok(Entity::delete_many()
        .filter(Column::Id.lte(cutoff))
        .exec(db)
        .await?
        .rows_affected)
}

/// What an agent reads instead of the raw message (`/dev/mailbox/latest.json`): quoted-printable
/// wraps long lines in the `.eml`, which cuts tokens out of links.
pub fn summary_json(m: &Model) -> serde_json::Value {
    let attachments: Vec<String> = m
        .attachments
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x["filename"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": m.id,
        "to": m.to_addr,
        "from": m.from_addr,
        "cc": m.cc,
        "bcc": m.bcc,
        "reply_to": m.reply_to,
        "subject": m.subject,
        "text": m.text,
        "links": links_in(&m.text, &m.html),
        "codes": codes_in(&m.text),
        "attachments": attachments,
        "read": m.read_at.is_some(),
        "created_at": m.created_at.to_rfc3339(),
    })
}

/// `[[name, value], …]` from a formatted message's header block, folded lines unfolded.
pub fn parse_headers(eml: &str) -> Vec<(String, String)> {
    let head = eml.split("\r\n\r\n").next().unwrap_or(eml);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in head.split("\r\n") {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    out
}

/// Every http(s) link, in order, deduplicated: from the text part, then any `href` only the HTML
/// part has. What makes a magic link one click (or one `jq`) away.
pub fn links_in(text: &str, html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |url: &str| {
        let url = url.trim_end_matches(['.', ',', ')', ']', '}', ';', ':', '!', '?', '\'', '"']);
        if (url.starts_with("http://") || url.starts_with("https://"))
            && url.len() > "https://".len()
            && !out.iter().any(|u| u == url)
        {
            out.push(url.to_owned());
        }
    };
    for word in text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"')) {
        push(word.trim_start_matches(['(', '[', '{', '\'']));
    }
    let mut rest = html;
    while let Some(i) = rest.find("href=") {
        rest = &rest[i + 5..];
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let value = rest[1..].split(quote).next().unwrap_or_default();
        push(&value.replace("&amp;", "&"));
    }
    out
}

/// One-time codes in a text part ("Your code is 482913", "Use 482-913", "code: X7K2PQ"): a token of
/// 4 to 8 digits (digits may be grouped by one space or dash), or of 6 to 10 capitals and digits
/// with at least one of each, on a line that talks about a code. A digit-only token alone on its
/// line counts too. Years and prices don't: they're not on such a line.
pub fn codes_in(text: &str) -> Vec<String> {
    // APP: words that mark a line as talking about a code (add your product's own, e.g. "PIN").
    const WORDS: &[&str] = &[
        "code",
        "otp",
        "one-time",
        "passcode",
        "pin",
        "verification",
        "verify",
        "token",
    ];
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        let talks_about_a_code = WORDS.iter().any(|w| lower.contains(w));
        let alone = line.trim();
        let mut candidates: Vec<String> = Vec::new();
        if is_digit_code(&compact(alone)) && alone.split_whitespace().count() <= 2 {
            candidates.push(compact(alone));
        }
        if talks_about_a_code {
            let words: Vec<&str> = line
                .split(|c: char| c.is_whitespace() || matches!(c, ':' | ',' | '(' | ')' | '"'))
                .map(|w| w.trim_end_matches(['.', '!', ';']))
                .filter(|w| !w.is_empty())
                .collect();
            for (i, w) in words.iter().enumerate() {
                let joined = words.get(i + 1).map(|n| format!("{w}{n}"));
                if let Some(j) = joined.filter(|j| {
                    w.len() == 3 && w.chars().all(|c| c.is_ascii_digit()) && is_digit_code(j)
                }) {
                    candidates.push(j);
                } else if is_digit_code(&compact(w)) || is_mixed_code(w) {
                    candidates.push(compact(w));
                }
            }
        }
        for c in candidates {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

/// "482-913" → "482913".
fn compact(s: &str) -> String {
    let groups: Vec<&str> = s.split(['-', ' ']).collect();
    if groups.len() > 1
        && groups
            .iter()
            .all(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()))
    {
        groups.concat()
    } else {
        s.to_owned()
    }
}

fn is_digit_code(s: &str) -> bool {
    (4..=8).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit())
}

fn is_mixed_code(s: &str) -> bool {
    (6..=10).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars().any(|c| c.is_ascii_uppercase())
}

/// The first `max` characters of a text body on one line, for the inbox list.
pub fn snippet(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_come_from_text_then_html_hrefs_without_trailing_punctuation() {
        let links = links_in(
            "Sign in: https://app.example.com/session/abc?t=1. Or (https://example.com/help).",
            r#"<a href="https://app.example.com/session/abc?t=1">x</a><a href='https://example.com/a?x=1&amp;y=2'>y</a><a href="mailto:a@example.com">m</a>"#,
        );
        assert_eq!(
            links,
            [
                "https://app.example.com/session/abc?t=1",
                "https://example.com/help",
                "https://example.com/a?x=1&y=2"
            ]
        );
    }

    #[test]
    fn codes_are_found_where_a_line_talks_about_a_code() {
        assert_eq!(codes_in("Your verification code is 482913."), ["482913"]);
        assert_eq!(codes_in("Use code 482-913 to sign in"), ["482913"]);
        assert_eq!(codes_in("Your one-time code: 482 913"), ["482913"]);
        assert_eq!(codes_in("Enter this code:\n\n    7310\n"), ["7310"]);
        assert_eq!(codes_in("Recovery token X7K2PQ9A"), ["X7K2PQ9A"]);
    }

    #[test]
    fn years_prices_and_words_are_not_codes() {
        assert!(codes_in("Since 2019 we have shipped 12000 orders for $4500.").is_empty());
        assert!(codes_in("Your CODE review is READY now").is_empty());
        assert!(codes_in("Call 555-0100 for help").is_empty());
    }

    #[test]
    fn headers_unfold() {
        let eml = "Subject: a very\r\n long subject\r\nTo: a@example.com\r\n\r\nbody: not a header";
        assert_eq!(
            parse_headers(eml),
            [
                ("Subject".to_string(), "a very long subject".to_string()),
                ("To".to_string(), "a@example.com".to_string())
            ]
        );
    }

    #[test]
    fn like_wildcards_are_literal() {
        assert_eq!(escape_like("50%_off\\"), "50\\%\\_off\\\\");
    }

    #[test]
    fn snippets_are_one_line_and_capped() {
        assert_eq!(snippet("Hi\n\nthere", 20), "Hi there");
        assert_eq!(snippet("abcdefghij", 5), "abcd…");
    }
}
