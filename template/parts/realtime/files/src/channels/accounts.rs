//! People are the auth-core sample's accounts: the session cookie auth-core sets, its `sessions`
//! table, its idle and absolute expiry. The realtime sample ships this file, and its optional
//! wires compile it only when auth-core is installed: `pub mod accounts;` in `channels/mod.rs`,
//! the `pub use` in `identity.rs` that replaces the development guests, and
//! `channels::session_ended` in `auth::sessions_ended` (so signing out closes sockets at once).
//! Without auth-core this file is not part of the build; delete it if you never add accounts.
//!
//! GUIDE: .offrails/guides/channels.md#the-app-side

use axum::http::HeaderMap;
use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use super::identity::{Person, Signed};
use crate::error::AppError;
use crate::kernel::session as auth_cookie;
use crate::models::{session, user};
use crate::state::AppState;

/// Accounts sign in through auth-core (`/session/new`); there are no development guests.
pub const DEV_GUESTS: bool = false;

/// What others in a room see. auth-core's users have no name column, so this is a neutral
/// "Member 42": never the address, and never a name guessed from it (`ada.lovelace@…` would
/// show "Ada Lovelace", and many addresses are a full name, an employer or worse).
/// APP: once users have a display name (a `name` column people chose), return it here, falling
/// back to this.
// SECURITY: presence and messages carry this name, never an email or anything derived from one
// (test: an_accounts_presence_shows_a_name_never_the_email).
pub fn display_name(u: &user::Model) -> String {
    format!("Member {}", u.id)
}

fn signed(s: session::Model, u: user::Model) -> Signed {
    Signed {
        person: Person {
            id: u.id,
            name: display_name(&u),
        },
        session_id: s.id,
    }
}

/// The request's auth-core session → person. Production reads only the `__Host-` cookie.
pub async fn current(s: &AppState, headers: &HeaderMap) -> Result<Option<Signed>, AppError> {
    let Some(raw) = auth_cookie::read_session(headers, s.config.is_production()) else {
        return Ok(None);
    };
    let now = Utc::now();
    let Some((session, u)) = session::find_by_token(&s.db, &raw, now).await? else {
        return Ok(None);
    };
    session::touch(&s.db, &session, now).await?;
    Ok(Some(signed(session, u)))
}

/// A session id (from a socket token) → person, while that session is live: the same idle and
/// absolute expiry as the cookie lookup.
pub async fn by_session(s: &AppState, session_id: i64) -> Result<Option<Signed>, AppError> {
    let now = Utc::now();
    let found = session::Entity::find_by_id(session_id)
        .filter(session::Column::LastSeenAt.gt(now - Duration::days(auth_cookie::IDLE_DAYS)))
        .filter(session::Column::CreatedAt.gt(now - Duration::days(auth_cookie::ABSOLUTE_DAYS)))
        .find_also_related(user::Entity)
        .one(&s.db)
        .await?;
    Ok(found.and_then(|(session, u)| u.map(|u| signed(session, u))))
}

/// Is this session still live (not signed out, revoked or expired)? Fails closed: a database
/// error counts as ended.
pub async fn session_live(s: &AppState, session_id: Option<i64>) -> bool {
    match session_id {
        Some(id) => matches!(by_session(s, id).await, Ok(Some(_))),
        None => false,
    }
}
