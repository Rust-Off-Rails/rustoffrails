//! auth-magic-link: request, the link, the code, single use, expiry, browser binding, sign-up on
//! first use, rate limits, re-auth by emailed code. On SQLite and Postgres.

use axum::http::{StatusCode, header};
use chrono::{Duration, Utc};
use offrails_host::routes::paths;
use offrails_host::{
    config::{AppEnv, Config, MagicLinkConfig, Signup},
    controllers::login_links,
    kernel::mailer::Email,
    models::{
        auth_event::{self, Kind},
        auth_throttle, login_link, session, user,
    },
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait, IntoActiveModel};

use crate::auth_core_test::{LogCapture, age_sessions, app_with_signup};
use crate::factories;
use crate::support::{TestApp, TestResponse};

pub async fn request_link(app: &TestApp, email: &str) -> TestResponse {
    app.post_form("/login_links", &[("email", email)]).await
}

/// Asks for a link and returns the email.
pub async fn mail_for(app: &TestApp, email: &str) -> Email {
    request_link(app, email)
        .await
        .assert_redirect_to("/login_links/sent");
    app.settle().await;
    app.last_email_to(email).expect("a sign-in email")
}

pub fn code_in(mail: &Email) -> String {
    mail.text
        .lines()
        .find_map(|l| l.strip_prefix("Your code: "))
        .expect("a code in the email")
        .trim()
        .to_owned()
}

/// Most tests here sign in with addresses that have no account yet, so they run with public
/// sign-up on (the first use creates the account), whatever the app's `DEFAULT_SIGNUP`. The
/// invite-only tests at the end run with it off.
pub async fn open_app() -> TestApp {
    app_with_signup(Signup::Open).await
}

pub fn signed_in(app: &TestApp) -> bool {
    app.cookie("session_token").is_some()
}

#[tokio::test]
async fn a_link_signs_in_and_creates_the_account_on_first_use() {
    let app = open_app().await;
    let mail = mail_for(&app, "Ada@Example.com ").await;
    assert_eq!(mail.to, "ada@example.com");
    assert!(
        user::find_by_email(app.db(), "ada@example.com")
            .await
            .unwrap()
            .is_none(),
        "nothing until used"
    );
    let link = app.link_in(&mail, "/login_links/");
    app.post_form(&link, &[])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    assert!(signed_in(&app));
    let ada = user::find_by_email(app.db(), "ada@example.com")
        .await
        .unwrap()
        .unwrap();
    assert!(ada.is_verified(), "reading the email proves the address");
    assert_eq!(user::Entity::find().all(app.db()).await.unwrap().len(), 1);
    let kinds = auth_event::kinds_for(app.db(), ada.id).await.unwrap();
    assert!(kinds.contains(&Kind::SignedUp) && kinds.contains(&Kind::SignedIn));
    assert_eq!(
        session::all_for(app.db(), &ada).await.unwrap()[0].sign_in_method,
        "magic_link"
    );
}

#[tokio::test]
async fn known_and_unknown_addresses_get_identical_responses() {
    for mode in [Signup::Open, Signup::Invite] {
        let app = app_with_signup(mode).await;
        factories::user(app.db(), "ada@example.com").await;
        let before = app.query_count();
        let known = request_link(&app, "ada@example.com").await;
        let known_queries = app.query_count() - before;
        app.settle().await;
        let before = app.query_count();
        let unknown = request_link(&app, "nobody@example.com").await;
        let unknown_queries = app.query_count() - before;
        assert_eq!(known.status, unknown.status);
        assert_eq!(known.location(), unknown.location());
        assert_eq!(known.body, unknown.body);
        let names = |r: &TestResponse| {
            r.set_cookies()
                .iter()
                .map(|c| c.split('=').next().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&known), names(&unknown));
        // The request does the same work for both; whether to send is decided after it.
        assert_eq!(known_queries, unknown_queries, "{mode:?}");
        app.settle().await;
        assert!(app.last_email_to("ada@example.com").is_some());
        // Open: the first use creates the account, so both get an email. Invite: only accounts.
        assert_eq!(
            app.last_email_to("nobody@example.com").is_some(),
            mode == Signup::Open
        );
    }
}

#[tokio::test]
async fn invalid_addresses_are_a_422_with_the_error_on_the_field() {
    let app = open_app().await;
    request_link(&app, "not an address")
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_contains("Email is invalid")
        .assert_contains("aria-invalid=\"true\"");
}

#[tokio::test]
async fn the_email_has_an_absolute_link_and_a_code() {
    let app = open_app().await;
    let mail = mail_for(&app, "ada@example.com").await;
    assert!(mail.text.contains("http://localhost:3000/login_links/"));
    assert!(mail.html.contains("http://localhost:3000/login_links/"));
    let code = code_in(&mail);
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));
}

#[tokio::test]
async fn links_follow_the_app_url_being_served_not_a_stale_port() {
    // Development resolves APP_URL to the port actually served (config.rs), so a link never
    // points at a stale localhost port; the mailer builds from that resolved URL.
    let config = Config {
        app_url: offrails_host::config::resolve_app_url(
            AppEnv::Development,
            Some("http://localhost:3000"),
            3960,
            false,
        ),
        ..Config::test()
    };
    let app = TestApp::with_config(config).await;
    // An existing account, so the email goes out whatever `DEFAULT_SIGNUP` is (invite-only
    // sends links only to accounts).
    factories::user(app.db(), "ada@example.com").await;
    let mail = mail_for(&app, "ada@example.com").await;
    assert!(
        mail.text.contains("http://localhost:3960/login_links/"),
        "{}",
        mail.text
    );
    assert!(!mail.text.contains("localhost:3000"));
}

#[tokio::test]
async fn opening_a_link_twice_then_posting_still_signs_in() {
    let app = open_app().await;
    let link = app.link_in(&mail_for(&app, "ada@example.com").await, "/login_links/");
    // Mail scanners and link previews GET the URL: that must not use it up.
    for _ in 0..2 {
        let page = app.get(&link).await;
        page.assert_ok()
            .assert_contains("Continue as")
            .assert_contains("ada@example.com");
        assert_eq!(page.headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(
            page.headers.get(header::REFERRER_POLICY).unwrap(),
            "no-referrer"
        );
    }
    assert!(!signed_in(&app));
    app.post_form(&link, &[])
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn a_link_works_once_even_when_used_twice_at_the_same_time() {
    let app = open_app().await;
    let link = app.link_in(&mail_for(&app, "ada@example.com").await, "/login_links/");
    let (a, b) = tokio::join!(app.post_form(&link, &[]), app.post_form(&link, &[]));
    let wins = [&a, &b]
        .iter()
        .filter(|r| r.status == StatusCode::SEE_OTHER)
        .count();
    assert_eq!(
        wins, 1,
        "exactly one request signs in: {} / {}",
        a.status, b.status
    );
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let ada = user::find_by_email(app.db(), "ada@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session::all_for(app.db(), &ada).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_link_opened_in_another_browser_is_refused_by_default() {
    // Login CSRF: an attacker asks for a link to *their* account and gets the victim to open it.
    let app = open_app().await;
    let link = app.link_in(
        &mail_for(&app, "mallory@example.com").await,
        "/login_links/",
    );
    app.clear_cookies(); // the victim's browser
    let page = app.get(&link).await;
    page.assert_status(StatusCode::FORBIDDEN)
        .assert_text("type the 6-digit code");
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::FORBIDDEN);
    assert!(!signed_in(&app));
}

#[tokio::test]
async fn any_browser_can_be_allowed_by_config() {
    let app = TestApp::with_config(Config {
        magic_link: MagicLinkConfig { any_browser: true },
        ..Config::test()
    })
    .await;
    // An existing account, so this holds whatever `DEFAULT_SIGNUP` is (invite-only sends links
    // only to accounts).
    factories::user(app.db(), "ada@example.com").await;
    let link = app.link_in(&mail_for(&app, "ada@example.com").await, "/login_links/");
    app.clear_cookies();
    app.post_form(&link, &[])
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn the_code_signs_in_only_in_the_browser_that_asked() {
    let app = open_app().await;
    let mail = mail_for(&app, "ada@example.com").await;
    let code = code_in(&mail);
    let asking_browser = app.cookie("login_browser").unwrap();
    // Another browser can't use the code.
    app.clear_cookies();
    app.post_form("/login_links/code", &[("code", &code)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    // The asking browser can, with spaces or dashes typed in.
    app.set_cookie("login_browser", &asking_browser);
    let spaced = format!("{} {}", &code[..3], &code[3..]);
    app.post_form("/login_links/code", &[("code", &spaced)])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    assert!(signed_in(&app));
    // And then the link is used up too.
    let link = app.link_in(&mail, "/login_links/");
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_code_allows_five_wrong_guesses_then_burns_the_link() {
    let app = open_app().await;
    let mail = mail_for(&app, "ada@example.com").await;
    let code = code_in(&mail);
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for left in (1..login_link::MAX_CODE_ATTEMPTS).rev() {
        app.post_form("/login_links/code", &[("code", wrong)])
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
            .assert_text(&format!("{left} tr"));
    }
    app.post_form("/login_links/code", &[("code", wrong)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    // Burned: the right code and the link are both dead now.
    app.post_form("/login_links/code", &[("code", &code)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_text("expired or was used up");
    app.post_form(&app.link_in(&mail, "/login_links/"), &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(!signed_in(&app));
}

/// Burning a link takes 5 wrong codes; a new link must not bring 5 more. Wrong codes count per
/// address in `auth_throttles`, across links, browsers and networks, with growing pauses.
#[tokio::test]
async fn code_guesses_are_throttled_per_address_across_links() {
    let app = open_app().await;
    let first = mail_for(&app, "ada@example.com").await;
    let code = code_in(&first);
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..login_link::MAX_CODE_ATTEMPTS {
        app.post_form("/login_links/code", &[("code", wrong)])
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    // A fresh link, then in another browser: even the right code waits now.
    let second = mail_for(&app, "ada@example.com").await;
    let res = app
        .post_form("/login_links/code", &[("code", &code_in(&second))])
        .await;
    res.assert_status(StatusCode::TOO_MANY_REQUESTS)
        .assert_text("Too many wrong codes for this address");
    assert!(res.headers.contains_key(header::RETRY_AFTER));
    app.clear_cookies();
    let third = mail_for(&app, "ada@example.com").await;
    app.post_form("/login_links/code", &[("code", &code_in(&third))])
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    assert!(!signed_in(&app));
    // Another address is not affected.
    let bob = mail_for(&app, "bob@example.com").await;
    app.post_form("/login_links/code", &[("code", &code_in(&bob))])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    // Once the pause is over (cleared here), the right code works and forgives the failures.
    app.clear_cookies();
    let key = login_links::code_throttle_key(&app.state, "ada@example.com");
    auth_throttle::clear(app.db(), &key).await.unwrap();
    let fourth = mail_for(&app, "ada@example.com").await;
    app.post_form("/login_links/code", &[("code", &code_in(&fourth))])
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn link_requests_are_capped_per_address_per_day() {
    let app = open_app().await;
    // A day of requests, spread over many 3-minute windows.
    for _ in 0..login_links::DAILY_LIMIT {
        app.state
            .rate_limit
            .check(
                &login_links::daily_key("ada@example.com"),
                login_links::DAILY_LIMIT,
                std::time::Duration::from_secs(24 * 60 * 60),
            )
            .await
            .unwrap();
    }
    request_link(&app, "ada@example.com")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS)
        .assert_text("Too many sign-in links requested");
    app.settle().await;
    assert!(app.last_email_to("ada@example.com").is_none());
    request_link(&app, "bob@example.com")
        .await
        .assert_redirect_to("/login_links/sent");
}

#[tokio::test]
async fn links_expire_after_fifteen_minutes() {
    let app = open_app().await;
    let mail = mail_for(&app, "ada@example.com").await;
    let link = app.link_in(&mail, "/login_links/");
    let row = login_link::Entity::find()
        .one(app.db())
        .await
        .unwrap()
        .unwrap();
    let mut a = row.clone().into_active_model();
    a.expires_at = Set(Utc::now() + Duration::minutes(1)); // issued 14 minutes ago
    a.update(app.db()).await.unwrap();
    app.get(&link).await.assert_ok();
    let mut a = row.into_active_model();
    a.expires_at = Set(Utc::now() - Duration::minutes(1)); // issued 16 minutes ago
    a.update(app.db()).await.unwrap();
    app.get(&link).await.assert_status(StatusCode::NOT_FOUND);
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.post_form("/login_links/code", &[("code", &code_in(&mail))])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_new_link_cancels_the_previous_one() {
    let app = open_app().await;
    let first = app.link_in(&mail_for(&app, "ada@example.com").await, "/login_links/");
    let second = app.link_in(&mail_for(&app, "ada@example.com").await, "/login_links/");
    assert_ne!(first, second);
    app.post_form(&first, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.post_form(&second, &[])
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn only_token_and_code_digests_are_stored() {
    let app = open_app().await;
    let mail = mail_for(&app, "ada@example.com").await;
    let link = app.link_in(&mail, "/login_links/");
    let token = link.trim_start_matches("/login_links/");
    let row = login_link::Entity::find()
        .one(app.db())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(row.token_digest, token);
    assert!(!row.code_digest.contains(&code_in(&mail)));
    assert_eq!(row.code_digest.len(), 64);
}

#[tokio::test]
async fn requests_are_rate_limited_per_network_and_per_address_across_networks() {
    let app = open_app().await;
    for i in 0..10 {
        request_link(&app, &format!("user{i}@example.com"))
            .await
            .assert_redirect_to("/login_links/sent");
    }
    request_link(&app, "another@example.com")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS)
        .assert_text("Too many sign-in links requested");

    // Per address, even when every request comes from a different network.
    let app = TestApp::with_config(Config {
        trusted_proxy: true,
        ..Config::test()
    })
    .await;
    for i in 0..10 {
        let ip = format!("198.51.100.{i}");
        app.post_form_with_headers(
            "/login_links",
            &[("email", "ada@example.com")],
            &[("x-forwarded-for", &ip)],
        )
        .await
        .assert_redirect_to("/login_links/sent");
    }
    app.post_form_with_headers(
        "/login_links",
        &[("email", "ada@example.com")],
        &[("x-forwarded-for", "203.0.113.9")],
    )
    .await
    .assert_status(StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn sign_in_by_link_honors_return_to() {
    let app = open_app().await;
    app.get("/account/email/edit")
        .await
        .assert_redirect_to("/session/new");
    let link = app.link_in(&mail_for(&app, "ada@example.com").await, "/login_links/");
    app.post_form(&link, &[])
        .await
        .assert_redirect_to("/account/email/edit");
}

#[tokio::test]
async fn the_sent_page_shows_the_code_form_and_the_dev_mailbox_hint_in_development() {
    let dev = TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await;
    dev.get("/login_links/sent")
        .await
        .assert_ok()
        .assert_contains("autocomplete=\"one-time-code\"")
        .assert_contains("href=\"/dev/mailbox\" hx-boost=\"false\"");
    let prod = TestApp::with_config(Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        ..Config::test()
    })
    .await;
    prod.get("/login_links/sent")
        .await
        .assert_ok()
        .assert_not_contains("/dev/mailbox");
}

#[tokio::test]
async fn emailed_code_reauth_confirms_identity() {
    let app = open_app().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    age_sessions(&app, 30).await;
    app.get("/account/email/edit")
        .await
        .assert_redirect_to("/account/confirm");
    app.get("/account/confirm")
        .await
        .assert_ok()
        .assert_contains("Email me a code");
    app.post_form("/account/confirm/email", &[])
        .await
        .assert_redirect_to("/account/confirm");
    app.settle().await;
    let mail = app.last_email_to("ada@example.com").unwrap();
    assert!(
        !mail.text.contains("/login_links/"),
        "a re-auth email carries a code, not a sign-in link"
    );
    let code = code_in(&mail);
    let wrong = if code == "000000" { "111111" } else { "000000" };
    app.post_form("/account/confirm/email/code", &[("code", wrong)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    app.post_form("/account/confirm/email/code", &[("code", &code)])
        .await
        .assert_redirect_to("/account/email/edit");
    app.get("/account/email/edit").await.assert_ok();
}

#[tokio::test]
async fn a_reauth_code_cant_sign_anyone_in() {
    let app = open_app().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    app.post_form("/account/confirm/email", &[]).await;
    app.settle().await;
    let code = code_in(&app.last_email_to("ada@example.com").unwrap());
    let browser = app.cookie("login_browser").unwrap();
    app.clear_cookies();
    app.set_cookie("login_browser", &browser);
    app.post_form("/login_links/code", &[("code", &code)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!signed_in(&app));
}

#[tokio::test]
async fn link_tokens_and_codes_never_reach_the_logs() {
    let logs = LogCapture::start();
    let app = open_app().await;
    let mail = mail_for(&app, "ada@example.com").await;
    let link = app.link_in(&mail, "/login_links/");
    let token = link.trim_start_matches("/login_links/").to_owned();
    let code = code_in(&mail);
    app.get(&link).await;
    app.post_form("/login_links/code", &[("code", &code)]).await;
    app.settle().await;
    logs.assert_absent(&[&token, &format!("code: {code}"), &format!("\"{code}\"")]);
}

#[tokio::test]
async fn code_guesses_are_capped_per_network() {
    let app = open_app().await;
    request_link(&app, "ada@example.com").await;
    for _ in 0..30 {
        let res = app
            .post_form("/login_links/code", &[("code", "000000")])
            .await;
        assert_ne!(res.status, StatusCode::TOO_MANY_REQUESTS);
    }
    app.post_form("/login_links/code", &[("code", "000000")])
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_reauth_code_only_confirms_its_own_account() {
    let app = open_app().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let bob = factories::user(app.db(), "bob@example.com").await;
    app.sign_in_as(&ada).await;
    app.post_form("/account/confirm/email", &[]).await;
    app.settle().await;
    let code = code_in(&app.last_email_to("ada@example.com").unwrap());
    // Same browser (same login_browser cookie), now signed in as Bob.
    app.sign_in_as(&bob).await;
    age_sessions(&app, 30).await;
    app.post_form("/account/confirm/email/code", &[("code", &code)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    app.get("/account/email/edit")
        .await
        .assert_redirect_to("/account/confirm");
}

#[tokio::test]
async fn magic_link_pages_follow_the_theme_switch() {
    let app = open_app().await;
    crate::auth_core_test::assert_pages_follow_the_theme(&app, &["/session/new"]).await;
    mail_for(&app, "ada@example.com").await;
    crate::auth_core_test::assert_pages_follow_the_theme(&app, &["/login_links/sent"]).await;
}

// ---------------------------------------------------------------------------------------------
// Invite-only (`Signup::Invite`): links only for existing accounts, never a new account
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn invite_only_magic_links_go_only_to_existing_accounts() {
    let app = app_with_signup(Signup::Invite).await;
    factories::user(app.db(), "ada@example.com").await;
    request_link(&app, "nobody@example.com")
        .await
        .assert_redirect_to("/login_links/sent");
    app.settle().await;
    assert!(app.last_email_to("nobody@example.com").is_none());
    assert!(
        login_link::Entity::find()
            .all(app.db())
            .await
            .unwrap()
            .is_empty(),
        "no link (and no code to guess) for an unknown address"
    );
    app.get("/login_links/sent")
        .await
        .assert_ok()
        .assert_text("If that address has an account");
    let mail = mail_for(&app, "ada@example.com").await;
    assert!(!mail.text.contains("creates your account"));
    let link = app.link_in(&mail, "/login_links/");
    app.post_form(&link, &[])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    assert!(signed_in(&app));
    assert_eq!(user::Entity::find().all(app.db()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn invite_only_magic_links_never_create_accounts() {
    // A link minted while sign-up was still open (or by an old process), used once it is closed.
    let app = app_with_signup(Signup::Invite).await;
    let minted = login_link::mint(
        app.db(),
        &app.state.config.auth.secret_key,
        "nobody@example.com",
        login_link::Purpose::SignIn,
        &offrails_host::kernel::auth_token::digest("this-browser"),
        Utc::now(),
    )
    .await
    .unwrap();
    app.set_cookie("login_browser", "this-browser");
    let link = format!("/login_links/{}", minted.token);
    app.get(&link).await.assert_ok();
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(!signed_in(&app));
    assert!(
        user::find_by_email(app.db(), "nobody@example.com")
            .await
            .unwrap()
            .is_none()
    );
}
