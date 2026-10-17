//! The read-only production view's admins are auth-core's accounts. The dev-dashboard sample ships
//! this file, and its optional wires compile it only when auth-core is installed: `pub mod
//! accounts;` in `kernel/dashboard/mod.rs` and the `pub use` in `access.rs` that replaces
//! `no_accounts`. Without auth-core this file is not part of the build.
//!
//! GUIDE: .offrails/guides/dev-tools.md

use axum::http::request::Parts;

use crate::{controllers::auth, models::user, state::AppState};

/// The app has accounts: `/admin/dashboard` is gated by them (and by `APP_ADMIN_PASSWORD` for
/// people who aren't signed in, when it is set).
pub const HAS_ACCOUNTS: bool = true;

/// `Some(true)` for a signed-in admin, `Some(false)` for a signed-in non-admin, `None` when
/// nobody is signed in. The session lookup is auth-core's own (expiry, revocation).
pub async fn admin_from_session(parts: &mut Parts, state: &AppState) -> Option<bool> {
    match auth::current_user(parts, state).await {
        Ok(Some(u)) => Some(is_admin(&u)),
        // WHY: a database error counts as "nobody signed in": the Basic gate still applies.
        Ok(None) | Err(_) => None,
    }
}

/// APP: who is an admin. auth-core has no roles, so nobody is until you add one (an `admin`
/// column, a role table, an allowlist of addresses in config). Until then signed-in people get
/// 404 and only the Basic password (`APP_ADMIN_PASSWORD`, signed out) opens the view.
pub fn is_admin(_user: &user::Model) -> bool {
    false
}
