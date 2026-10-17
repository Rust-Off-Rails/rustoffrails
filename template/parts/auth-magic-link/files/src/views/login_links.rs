//! Page structs for the magic-link confirm page and its "wrong browser" page.

use askama::Template;

use super::Ctx;
use crate::routes::paths;

/// `GET /login_links/{token}`: "Sign in as …" with one button.
#[derive(Template)]
#[template(path = "login_links/show.html")]
pub struct LoginLinkShow {
    pub ctx: Ctx,
    pub email: String,
    pub action: String,
}

/// The link was opened in another browser than the one that asked for it.
#[derive(Template)]
#[template(path = "login_links/other_browser.html")]
pub struct LoginLinkOtherBrowser {
    pub ctx: Ctx,
}
