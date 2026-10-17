//! auth-core: sessions, cookies, return-to, the account page, revocation, re-auth, email change,
//! "this wasn't me", throttling and the audit trail. Every test runs on SQLite and Postgres.
//! The sign-in methods (password, magic link, two-factor) have their own files.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use offrails_host::{
    config::{AppEnv, Config},
    controllers::auth::{self, CurrentSession, RequestMeta},
    kernel::{auth_token, session as auth_cookie},
    models::{
        auth_event::{self, Kind, Meta},
        auth_throttle, session, user, user_token,
    },
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait, IntoActiveModel};

use crate::factories;
use crate::support::TestApp;

fn production() -> Config {
    Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        ..Config::test()
    }
}

fn development() -> Config {
    Config {
        env: AppEnv::Development,
        ..Config::test()
    }
}

fn meta() -> RequestMeta {
    RequestMeta {
        meta: Meta {
            ip: Some("192.0.2.10".into()),
            user_agent: Some(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Firefox/140.0".into(),
            ),
            device_digest: None,
        },
        device: None,
    }
}

fn cookie_header(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    let value = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    if !value.is_empty() {
        h.insert(header::COOKIE, HeaderValue::from_str(&value).unwrap());
    }
    h
}

fn set_cookies(r: &axum::response::Response) -> Vec<String> {
    r.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect()
}

/// The raw session token a response's `Set-Cookie` carries.
fn session_token_in(cookies: &[String], secure: bool) -> Option<String> {
    let prefix = format!("{}=", auth_cookie::session_name(secure));
    cookies.iter().find_map(|c| {
        c.strip_prefix(&prefix)
            .and_then(|rest| rest.split(';').next())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    })
}

// ---------------------------------------------------------------------------------------------
// Sessions and cookies
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_account_page_needs_a_session_and_remembers_where_you_were_going() {
    let app = TestApp::new().await;
    let res = app.get("/account?tab=sessions").await;
    res.assert_redirect_to("/session/new");
    let remembered = app.cookie("return_to").expect("return_to cookie");
    assert_eq!(
        URL_SAFE_NO_PAD.decode(remembered).unwrap(),
        b"/account?tab=sessions"
    );

    let user = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&user).await;
    app.get("/account")
        .await
        .assert_ok()
        .assert_contains("ada@example.com")
        .assert_contains("Where you're signed in");
}

#[tokio::test]
async fn htmx_requests_without_a_session_get_hx_redirect() {
    let app = TestApp::new().await;
    let res = app.get_htmx("/account").await;
    assert_eq!(res.headers.get("hx-redirect").unwrap(), "/session/new");
}

#[tokio::test]
async fn only_token_digests_are_stored() {
    let app = TestApp::new().await;
    let user = factories::user(app.db(), "ada@example.com").await;
    let raw = app.sign_in_as(&user).await;
    let rows = session::Entity::find().all(app.db()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].token_digest, raw);
    assert_eq!(rows[0].token_digest, auth_token::digest(&raw));

    let token = user_token::issue(
        app.db(),
        user.id,
        user_token::Purpose::NotMe,
        &user.email,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    let stored = user_token::Entity::find().all(app.db()).await.unwrap();
    assert!(
        stored
            .iter()
            .all(|t| t.token_digest != token && t.token_digest.len() == 64)
    );
}

#[tokio::test]
async fn sign_out_kills_the_token_server_side() {
    let app = TestApp::new().await;
    let user = factories::user(app.db(), "ada@example.com").await;
    let raw = app.sign_in_as(&user).await;
    let res = app.post_form("/session/delete", &[]).await;
    res.assert_redirect_to("/");
    assert!(
        res.set_cookies()
            .iter()
            .any(|c| c.starts_with("session_token=;") && c.contains("Max-Age=0"))
    );
    // Replaying the old cookie logs nobody in.
    app.set_cookie("session_token", &raw);
    app.get("/account").await.assert_redirect_to("/session/new");
    assert!(
        auth_event::kinds_for(app.db(), user.id)
            .await
            .unwrap()
            .contains(&Kind::SignedOut)
    );
}

#[tokio::test]
async fn sign_in_rotates_the_session_and_kills_the_old_token() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let old = app.sign_in_as(&ada).await;
    let headers = cookie_header(&[("session_token", &old)]);
    let res = auth::finish_sign_in(&app.state, &headers, &meta(), ada.clone(), "test")
        .await
        .unwrap();
    let new = session_token_in(&set_cookies(&res), false).expect("a new session cookie");
    assert_ne!(new, old, "sign-in must issue a new token");
    assert!(
        session::find_by_token(app.db(), &old, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        session::find_by_token(app.db(), &new, Utc::now())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_session_planted_before_sign_in_is_worthless_after_it() {
    // Fixation: an attacker plants their own valid cookie in the victim's browser; the victim
    // signs in. The planted token must not become (or stay) the victim's session.
    let app = TestApp::new().await;
    let mallory = factories::user(app.db(), "mallory@example.com").await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let planted = app.sign_in_as(&mallory).await;
    let headers = cookie_header(&[("session_token", &planted)]);
    let res = auth::finish_sign_in(&app.state, &headers, &meta(), ada, "test")
        .await
        .unwrap();
    let new = session_token_in(&set_cookies(&res), false).unwrap();
    assert_ne!(new, planted);
    assert!(
        session::find_by_token(app.db(), &planted, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn production_auth_cookies_are_host_prefixed_and_secure() {
    let app = TestApp::with_config(production()).await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let res = auth::finish_sign_in(&app.state, &HeaderMap::new(), &meta(), ada, "test")
        .await
        .unwrap();
    let cookies = set_cookies(&res);
    let auth_cookies: Vec<&String> = cookies
        .iter()
        .filter(|c| !c.starts_with("_flash="))
        .collect();
    assert!(
        auth_cookies.len() >= 3,
        "session, return_to and device cookies: {cookies:?}"
    );
    for c in auth_cookies {
        assert!(c.starts_with("__Host-"), "{c}");
        assert!(
            c.contains("; Path=/")
                && c.contains("HttpOnly")
                && c.contains("SameSite=Lax")
                && c.contains("Secure"),
            "{c}"
        );
        assert!(!c.contains("Domain"), "{c}");
    }
    // A signed-out GET of a protected page remembers it in a __Host- cookie too.
    let res = app.get("/account").await;
    assert!(
        res.set_cookies()
            .iter()
            .any(|c| c.starts_with("__Host-return_to=") && c.contains("Secure"))
    );
}

#[tokio::test]
async fn production_ignores_a_planted_unprefixed_session_cookie() {
    let app = TestApp::with_config(production()).await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let (_, raw) = session::start(app.db(), ada.id, "test", None, None, Utc::now())
        .await
        .unwrap();
    app.set_cookie("session_token", &raw);
    app.get("/account").await.assert_redirect_to("/session/new");
    app.clear_cookies();
    app.set_cookie("__Host-session_token", &raw);
    app.get("/account").await.assert_ok();
}

#[tokio::test]
async fn expired_sessions_log_nobody_in() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let raw = app.sign_in_as(&ada).await;
    let row = session::Entity::find()
        .one(app.db())
        .await
        .unwrap()
        .unwrap();

    // Idle for more than 30 days.
    let mut idle = row.clone().into_active_model();
    idle.last_seen_at = Set(Utc::now() - Duration::days(31));
    idle.update(app.db()).await.unwrap();
    app.get("/account").await.assert_redirect_to("/session/new");

    // Active, but signed in more than 90 days ago (absolute lifetime).
    let mut old = row.into_active_model();
    old.last_seen_at = Set(Utc::now());
    old.created_at = Set(Utc::now() - Duration::days(91));
    old.update(app.db()).await.unwrap();
    app.set_cookie("session_token", &raw);
    app.get("/account").await.assert_redirect_to("/session/new");
}

#[tokio::test]
async fn return_to_rejects_open_redirects() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    for evil in [
        "//evil.example/x",
        "/\\evil.example",
        "https://evil.example",
        "/ok\r\nLocation: //evil.example",
    ] {
        let value = URL_SAFE_NO_PAD.encode(evil);
        let headers = cookie_header(&[("return_to", &value)]);
        let res = auth::finish_sign_in(&app.state, &headers, &meta(), ada.clone(), "test")
            .await
            .unwrap();
        assert_eq!(
            res.headers().get(header::LOCATION).unwrap(),
            "/account",
            "{evil:?}"
        );
    }
    let value = URL_SAFE_NO_PAD.encode("/account?tab=sessions");
    let headers = cookie_header(&[("return_to", &value)]);
    let res = auth::finish_sign_in(&app.state, &headers, &meta(), ada, "test")
        .await
        .unwrap();
    assert_eq!(
        res.headers().get(header::LOCATION).unwrap(),
        "/account?tab=sessions"
    );
    assert!(
        set_cookies(&res)
            .iter()
            .any(|c| c.starts_with("return_to=;") && c.contains("Max-Age=0")),
        "used once"
    );
}

// ---------------------------------------------------------------------------------------------
// The sessions list
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_sessions_list_shows_devices_and_revokes_one_or_all_others() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let phone_ua =
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) Version/18.0 Mobile Safari/604.1";
    let (phone, phone_raw) = session::start(
        app.db(),
        ada.id,
        "password",
        Some("198.51.100.7".into()),
        Some(phone_ua.into()),
        Utc::now(),
    )
    .await
    .unwrap();
    let (_, laptop_raw) = session::start(
        app.db(),
        ada.id,
        "magic_link",
        None,
        Some("Mozilla/5.0 (Windows NT 10.0) Chrome/141.0 Safari/537.36".into()),
        Utc::now(),
    )
    .await
    .unwrap();
    app.sign_in_as(&ada).await;

    let page = app.get("/account").await;
    page.assert_ok()
        .assert_contains("Safari on iPhone")
        .assert_contains("Chrome on Windows")
        .assert_contains("198.51.100.7")
        .assert_contains("This device");

    app.post_form(&format!("/account/sessions/{}/delete", phone.id), &[])
        .await
        .assert_redirect_to("/account");
    assert!(
        session::find_by_token(app.db(), &phone_raw, Utc::now())
            .await
            .unwrap()
            .is_none()
    );

    app.post_form("/account/sessions/delete-others", &[])
        .await
        .assert_redirect_to("/account");
    assert!(
        session::find_by_token(app.db(), &laptop_raw, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    // This device is still signed in.
    app.get("/account").await.assert_ok();
    let kinds = auth_event::kinds_for(app.db(), ada.id).await.unwrap();
    assert!(kinds.contains(&Kind::SessionRevoked) && kinds.contains(&Kind::OtherSessionsRevoked));
}

#[tokio::test]
async fn revoking_another_users_session_is_404() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let bob = factories::user(app.db(), "bob@example.com").await;
    let (bobs, bobs_raw) = session::start(app.db(), bob.id, "test", None, None, Utc::now())
        .await
        .unwrap();
    app.sign_in_as(&ada).await;
    app.post_form(&format!("/account/sessions/{}/delete", bobs.id), &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(
        session::find_by_token(app.db(), &bobs_raw, Utc::now())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_account_page_query_count_is_stable() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    let db = app.db().clone();
    let id = ada.id;
    app.assert_query_count_stable("/account", async |n| {
        for _ in 0..n {
            session::start(&db, id, "test", None, None, Utc::now())
                .await
                .unwrap();
            auth_event::record(&db, Some(id), Kind::SignedIn, &Meta::default(), Utc::now())
                .await
                .unwrap();
        }
    })
    .await;
}

// ---------------------------------------------------------------------------------------------
// Re-authentication ("sudo mode")
// ---------------------------------------------------------------------------------------------

pub async fn age_sessions(app: &TestApp, minutes: i64) {
    for s in session::Entity::find().all(app.db()).await.unwrap() {
        let mut a = s.into_active_model();
        a.authenticated_at = Set(Utc::now() - Duration::minutes(minutes));
        a.update(app.db()).await.unwrap();
    }
}

#[tokio::test]
async fn sensitive_actions_require_recent_auth() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    app.get("/account/email/edit").await.assert_ok();

    age_sessions(&app, 11).await;
    app.get("/account/email/edit")
        .await
        .assert_redirect_to("/account/confirm");
    let back = app.cookie("return_to").unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD.decode(back).unwrap(),
        b"/account/email/edit"
    );
    app.post_form("/account/email", &[("email", "new@example.com")])
        .await
        .assert_redirect_to("/account/confirm");
    app.settle().await;
    assert!(
        app.last_email_to("new@example.com").is_none(),
        "nothing sent without re-auth"
    );
    app.get("/account/confirm")
        .await
        .assert_ok()
        .assert_contains("Confirm it&#39;s you");
}

#[tokio::test]
async fn step_up_rotates_the_session_token() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let old = app.sign_in_as(&ada).await;
    age_sessions(&app, 30).await;
    let (s, u) = session::find_by_token(app.db(), &old, Utc::now())
        .await
        .unwrap()
        .unwrap();
    let current = CurrentSession {
        session: s,
        user: u,
    };
    let res = auth::grant_step_up(&app.state, &HeaderMap::new(), &meta(), &current)
        .await
        .unwrap();
    let new = session_token_in(&set_cookies(&res), false).unwrap();
    assert_ne!(new, old);
    assert!(
        session::find_by_token(app.db(), &old, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    let (s, _) = session::find_by_token(app.db(), &new, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert!(Utc::now() - s.authenticated_at < Duration::minutes(1));
    assert!(
        auth_event::kinds_for(app.db(), ada.id)
            .await
            .unwrap()
            .contains(&Kind::Reauthenticated)
    );
}

// ---------------------------------------------------------------------------------------------
// Email change
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn changing_the_email_needs_the_new_address_to_confirm_and_warns_the_old_one() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    app.post_form("/account/email", &[("email", "Ada.New@Example.com")])
        .await
        .assert_redirect_to("/account");
    app.settle().await;
    let mail = app
        .last_email_to("ada.new@example.com")
        .expect("confirmation to the new address");
    let link = app.link_in(&mail, "/account/email/confirm/");
    // Nothing changed yet.
    assert_eq!(
        user::find_self(app.db(), ada.id).await.unwrap().email,
        "ada@example.com"
    );
    // GET is safe (scanners); POST applies.
    app.get(&link)
        .await
        .assert_ok()
        .assert_contains("ada.new@example.com");
    app.post_form(&link, &[])
        .await
        .assert_redirect_to("/account");
    assert_eq!(
        user::find_self(app.db(), ada.id).await.unwrap().email,
        "ada.new@example.com"
    );
    app.settle().await;
    let warning = app
        .last_email_to("ada@example.com")
        .expect("the old address is told");
    assert!(warning.text.contains("ada.new@example.com"));
    assert!(warning.text.contains("/security/not-me/"));
    // The link worked once.
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_email_change_to_a_taken_address_looks_the_same_and_sends_nothing() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    factories::user(app.db(), "bob@example.com").await;
    app.sign_in_as(&ada).await;
    let taken = app
        .post_form("/account/email", &[("email", "bob@example.com")])
        .await;
    let free = app
        .post_form("/account/email", &[("email", "free@example.com")])
        .await;
    assert_eq!(taken.status, free.status);
    assert_eq!(taken.location(), free.location());
    app.settle().await;
    assert!(app.last_email_to("bob@example.com").is_none());
    assert!(app.last_email_to("free@example.com").is_some());
}

// ---------------------------------------------------------------------------------------------
// "This wasn't me"
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn not_me_signs_out_everywhere() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let (_, other) = session::start(app.db(), ada.id, "test", None, None, Utc::now())
        .await
        .unwrap();
    let mine = app.sign_in_as(&ada).await;
    // An email change someone started (a link on its way to their address) must die too.
    let pending_change = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::EmailChange,
        "mallory@example.com",
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    auth::notify(
        &app.state,
        ada.id,
        offrails_host::mailers::auth_mailer::Notice::PasswordChanged,
    );
    app.settle().await;
    let mail = app.last_email_to("ada@example.com").unwrap();
    let link = app.link_in(&mail, "/security/not-me/");

    app.clear_cookies(); // the person may be locked out: no session needed
    app.get(&link)
        .await
        .assert_ok()
        .assert_contains("Sign out everywhere");
    app.post_form(&link, &[])
        .await
        .assert_redirect_to("/session/new");
    for raw in [mine, other] {
        assert!(
            session::find_by_token(app.db(), &raw, Utc::now())
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        auth_event::kinds_for(app.db(), ada.id)
            .await
            .unwrap()
            .contains(&Kind::NotMe)
    );
    assert!(
        user_token::find_valid(
            app.db(),
            user_token::Purpose::EmailChange,
            &pending_change,
            Utc::now()
        )
        .await
        .unwrap()
        .is_none(),
        "the pending email change is revoked"
    );
    app.get(&format!("/account/email/confirm/{pending_change}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Single use.
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn token_pages_are_not_cached_and_send_no_referrer() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let token = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::NotMe,
        &ada.email,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    for path in [
        format!("/security/not-me/{token}"),
        "/security/not-me/not-a-real-token".to_owned(),
    ] {
        let res = app.get(&path).await;
        assert_eq!(
            res.headers.get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "{path}"
        );
        assert_eq!(
            res.headers.get(header::REFERRER_POLICY).unwrap(),
            "no-referrer",
            "{path}"
        );
        // The token is never echoed into canonical or social-card URLs.
        assert!(
            !res.body
                .contains("canonical\" href=\"http://localhost:3000/security"),
            "{path}"
        );
        assert!(
            !res.body
                .contains("og:url\" content=\"http://localhost:3000/security"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn expired_and_wrong_purpose_tokens_are_refused() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let reset = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::PasswordReset,
        &ada.email,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    // A reset token is not a "not me" token.
    app.post_form(&format!("/security/not-me/{reset}"), &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let old = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::NotMe,
        &ada.email,
        None,
        Utc::now() - Duration::days(8),
    )
    .await
    .unwrap();
    app.get(&format!("/security/not-me/{old}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------------------------
// Throttling
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn parallel_guesses_are_counted_up_front() {
    let app = TestApp::new().await;
    let db = app.db().clone();
    let now = Utc::now();
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move {
                auth_throttle::begin(&db, "parallel", now)
                    .await
                    .unwrap()
                    .is_ok()
            })
        })
        .collect();
    let mut allowed = 0;
    for t in tasks {
        if t.await.unwrap() {
            allowed += 1;
        }
    }
    assert_eq!(
        allowed,
        auth_throttle::FREE_FAILURES,
        "only the free attempts get through"
    );
}

#[tokio::test]
async fn refused_attempts_do_not_extend_the_pause() {
    let app = TestApp::new().await;
    let now = Utc::now();
    for _ in 0..auth_throttle::FREE_FAILURES {
        assert!(
            auth_throttle::begin(app.db(), "k", now)
                .await
                .unwrap()
                .is_ok()
        );
    }
    let first = auth_throttle::begin(app.db(), "k", now)
        .await
        .unwrap()
        .unwrap_err();
    for _ in 0..50 {
        let again = auth_throttle::begin(app.db(), "k", now + Duration::seconds(10))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            again.until, first.until,
            "hammering never moves the end of the pause"
        );
    }
    // Right after the pause ends, an attempt goes through.
    assert!(
        auth_throttle::begin(app.db(), "k", first.until + Duration::seconds(1))
            .await
            .unwrap()
            .is_ok()
    );
}

#[tokio::test]
async fn a_success_forgives_and_a_quiet_day_forgets() {
    let app = TestApp::new().await;
    let now = Utc::now();
    for _ in 0..3 {
        auth_throttle::begin(app.db(), "k", now)
            .await
            .unwrap()
            .unwrap();
    }
    let attempt = auth_throttle::begin(app.db(), "k", now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.number, 4);
    auth_throttle::succeeded(app.db(), attempt).await.unwrap();
    assert_eq!(
        auth_throttle::begin(app.db(), "k", now)
            .await
            .unwrap()
            .unwrap()
            .number,
        1
    );
    let later = now + Duration::hours(25);
    assert_eq!(
        auth_throttle::begin(app.db(), "k", later)
            .await
            .unwrap()
            .unwrap()
            .number,
        1
    );
}

// ---------------------------------------------------------------------------------------------
// Models, screens and headers
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn find_or_create_twice_in_one_transaction_keeps_it_usable() {
    let app = TestApp::new().await;
    let txn = offrails_host::kernel::db::begin(app.db()).await.unwrap();
    let a = user::find_or_create(&txn, "Race@Example.com", Utc::now())
        .await
        .unwrap();
    let b = user::find_or_create(&txn, "race@example.com", Utc::now())
        .await
        .unwrap();
    assert_eq!(a.id, b.id);
    // On Postgres a caught unique violation would have aborted the transaction here.
    session::start(&txn, a.id, "test", None, None, Utc::now())
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(user::Entity::find().all(app.db()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn auth_screens_link_to_the_dev_mailbox_in_development_only() {
    let dev = TestApp::with_config(development()).await;
    dev.get("/session/new")
        .await
        .assert_ok()
        .assert_contains("href=\"/dev/mailbox\" hx-boost=\"false\"");
    for app in [
        TestApp::new().await,
        TestApp::with_config(production()).await,
    ] {
        app.get("/session/new")
            .await
            .assert_ok()
            .assert_not_contains("/dev/mailbox");
    }
}

#[tokio::test]
async fn signed_in_pages_are_no_store_and_carry_the_header_matrix() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    for path in ["/account", "/account/confirm", "/account/email/edit"] {
        let res = app.get(path).await;
        res.assert_ok();
        assert_eq!(
            res.headers.get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "{path}"
        );
        for h in [
            "content-security-policy",
            "x-frame-options",
            "x-content-type-options",
            "referrer-policy",
        ] {
            assert!(res.headers.contains_key(h), "{path} lacks {h}");
        }
    }
}

#[tokio::test]
async fn cross_site_posts_to_auth_routes_are_refused() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let raw = app.sign_in_as(&ada).await;
    for path in [
        "/session/delete",
        "/account/sessions/delete-others",
        "/account/email",
    ] {
        app.post_form_with_headers(
            path,
            &[("email", "x@example.com")],
            &[("sec-fetch-site", "cross-site")],
        )
        .await
        .assert_status(StatusCode::FORBIDDEN);
        app.post_form_with_headers(
            path,
            &[],
            &[("sec-fetch-site", ""), ("origin", "https://evil.example")],
        )
        .await
        .assert_status(StatusCode::FORBIDDEN);
    }
    assert!(
        session::find_by_token(app.db(), &raw, Utc::now())
            .await
            .unwrap()
            .is_some(),
        "still signed in"
    );
}

#[tokio::test]
async fn security_notices_go_out_with_an_absolute_not_me_link() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    auth::notify(
        &app.state,
        ada.id,
        offrails_host::mailers::auth_mailer::Notice::NewSignIn {
            device: "Firefox on Linux".into(),
            ip: Some("192.0.2.1".into()),
        },
    );
    app.settle().await;
    let mail = app.last_email_to("ada@example.com").unwrap();
    assert!(mail.subject.contains("New sign-in"));
    assert!(mail.text.contains("Firefox on Linux"));
    assert!(mail.text.contains("http://localhost:3000/security/not-me/"));
    assert!(mail.html.contains("http://localhost:3000/security/not-me/"));
}

#[tokio::test]
async fn the_demo_task_refuses_outside_development() {
    let app = TestApp::with_config(production()).await;
    assert!(
        offrails_host::tasks::auth_demo::run(&app.state, vec![])
            .await
            .is_err()
    );
    let dev = TestApp::with_config(development()).await;
    offrails_host::tasks::auth_demo::run(&dev.state, vec![])
        .await
        .unwrap();
    assert!(
        user::find_by_email(dev.db(), "demo@example.com")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_secret_key_must_be_long_enough_and_set_in_production() {
    use offrails_host::config::AuthConfig;
    let (mut missing, mut invalid) = (Vec::new(), Vec::new());
    AuthConfig::from_value(AppEnv::Production, None, &mut missing, &mut invalid);
    assert_eq!(missing, vec!["APP_SECRET_KEY".to_owned()]);
    let (mut missing, mut invalid) = (Vec::new(), Vec::new());
    AuthConfig::from_value(
        AppEnv::Production,
        Some("too-short".into()),
        &mut missing,
        &mut invalid,
    );
    assert_eq!(invalid.len(), 1);
    let (mut missing, mut invalid) = (Vec::new(), Vec::new());
    let ok = AuthConfig::from_value(
        AppEnv::Production,
        Some("k".repeat(48)),
        &mut missing,
        &mut invalid,
    );
    assert!(missing.is_empty() && invalid.is_empty());
    assert_eq!(ok.secret_key, "k".repeat(48).into_bytes());
    // Development works without one; two test configs never share a key.
    let (mut missing, mut invalid) = (Vec::new(), Vec::new());
    AuthConfig::from_value(AppEnv::Development, None, &mut missing, &mut invalid);
    assert!(missing.is_empty() && invalid.is_empty());
    assert_ne!(
        Config::test().auth.secret_key,
        Config::test().auth.secret_key
    );
}

#[tokio::test]
async fn a_reset_link_sent_to_an_old_address_dies_with_the_change() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let reset = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::PasswordReset,
        &ada.email,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(
        user_token::find_valid(
            app.db(),
            user_token::Purpose::PasswordReset,
            &reset,
            Utc::now()
        )
        .await
        .unwrap()
        .is_some()
    );
    user::change_email(app.db(), ada.id, "ada.new@example.com", Utc::now())
        .await
        .unwrap();
    assert!(
        user_token::find_valid(
            app.db(),
            user_token::Purpose::PasswordReset,
            &reset,
            Utc::now()
        )
        .await
        .unwrap()
        .is_none()
    );
}

// ---------------------------------------------------------------------------------------------
// Shared with the method samples' tests
// ---------------------------------------------------------------------------------------------

/// Captures every log line at trace level (tower_http's request lines aside: they carry URLs,
/// see CHECKS.md) until the guard drops. Tests run flows, then assert no secret was logged.
pub struct LogCapture {
    file: tempfile::NamedTempFile,
    _guard: tracing::subscriber::DefaultGuard,
}

impl LogCapture {
    pub fn start() -> Self {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let sink = file.reopen().expect("reopen");
        let subscriber = tracing_subscriber::fmt()
            // The app's own filter (main.rs), at trace: what a RUST_LOG=trace run would write.
            .with_env_filter(offrails_host::kernel::redact::log_filter(
                "trace,hyper=info,h2=info",
            ))
            .with_ansi(false)
            .with_writer(move || sink.try_clone().expect("clone log file"))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        Self {
            file,
            _guard: guard,
        }
    }

    pub fn text(&self) -> String {
        std::fs::read_to_string(self.file.path()).unwrap_or_default()
    }

    /// Fails when any `secret` appears in the captured logs.
    pub fn assert_absent(&self, secrets: &[&str]) {
        let logs = self.text();
        assert!(
            logs.len() > 200,
            "the capture saw almost nothing; is logging wired?"
        );
        for s in secrets {
            assert!(!s.is_empty());
            assert!(!logs.contains(s), "a secret reached the logs: {s:?}");
        }
    }
}

#[tokio::test]
async fn session_tokens_never_reach_the_logs() {
    let logs = LogCapture::start();
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let raw = app.sign_in_as(&ada).await;
    app.get("/account").await.assert_ok();
    auth::notify(
        &app.state,
        ada.id,
        offrails_host::mailers::auth_mailer::Notice::PasswordChanged,
    );
    app.settle().await;
    let link = app.link_in(
        &app.last_email_to("ada@example.com").unwrap(),
        "/security/not-me/",
    );
    let not_me = link.trim_start_matches("/security/not-me/").to_owned();
    app.post_form(&link, &[]).await;
    logs.assert_absent(&[&raw, &not_me]);
}

/// Every page in `paths` renders `<html data-theme>` from the `offrails_theme` cookie: auth pages
/// use the app layout, which follows the kernel header's theme switch (samples/FORMAT.md#theming).
pub async fn assert_pages_follow_the_theme(app: &TestApp, paths: &[&str]) {
    for theme in ["system", "light", "dark"] {
        app.set_cookie("offrails_theme", theme);
        for path in paths {
            app.get(path)
                .await
                .assert_ok()
                .assert_contains(&format!(r#"<html lang="en" data-theme="{theme}">"#));
        }
    }
}

#[tokio::test]
async fn auth_pages_follow_the_theme_switch() {
    let app = TestApp::new().await;
    assert_pages_follow_the_theme(&app, &["/session/new"]).await;
    let user = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&user).await;
    assert_pages_follow_the_theme(&app, &["/account", "/account/email/edit"]).await;
}

/// Every handler that deletes session rows tells `auth::sessions_ended` after its commit, so
/// realtime (when installed) closes that person's sockets at once. Checked on the source: one
/// function at a time, across every controller in the app (other auth samples' too).
#[test]
fn every_session_ending_path_calls_sessions_ended() {
    const ENDS: &[&str] = &[
        "session::revoke_for(",
        "session::revoke_others(",
        "session::revoke_all(",
        "auth::revoke_everything(",
        "auth::revoke_other_access(",
        "auth::claim_unverified_account(",
        // Sign-out, and sign-in over an old session (`finish_sign_in`).
        "session::destroy_by_token(",
    ];
    // The helpers themselves run inside a caller's transaction; the caller reports.
    const HELPERS: &[&str] = &[
        "revoke_everything(",
        "revoke_other_access(",
        "claim_unverified_account(",
    ];
    const REPORTS: &[&str] = &[
        "sessions_ended(&s,",
        "sessions_ended(s,",
        "sessions_ended(state,",
    ];
    let mut checked = 0;
    for entry in std::fs::read_dir("src/controllers").unwrap() {
        let path = entry.unwrap().path();
        let src = std::fs::read_to_string(&path).unwrap();
        for f in src
            .split("\nasync fn ")
            .flat_map(|c| c.split("\npub async fn "))
        {
            if !ENDS.iter().any(|e| f.contains(e)) || HELPERS.iter().any(|h| f.starts_with(h)) {
                continue;
            }
            checked += 1;
            let name = f.split('(').next().unwrap_or_default();
            assert!(
                REPORTS.iter().any(|r| f.contains(r)),
                "{}: {name} ends sessions without auth::sessions_ended",
                path.display()
            );
        }
    }
    assert!(checked >= 4, "found only {checked} session-ending handlers");
}
