//! auth-password: sign-up and verification, sign-in, throttling, reset, change, re-auth and the
//! password policy. Every flow and every attack, on SQLite and Postgres.

use axum::http::{StatusCode, header};
use chrono::{Duration, Utc};
use offrails_host::{
    config::Signup,
    models::{
        auth_event::{self, Kind},
        auth_throttle, password_credential, session, user, user_token,
    },
    routes::paths,
    tasks::auth_invite,
};
use sea_orm::EntityTrait;

use crate::auth_core_test::{LogCapture, age_sessions, app_with_signup};
use crate::factories::{self, PASSWORD};
use crate::support::{TestApp, TestResponse};

const NEW_PASSWORD: &str = "copper kettle lighthouse";

/// Sign-up tests run with public sign-up on, whatever the app's `DEFAULT_SIGNUP`; the invite-only
/// tests below run with it off.
async fn open_app() -> TestApp {
    app_with_signup(Signup::Open).await
}

async fn sign_in(app: &TestApp, email: &str, password: &str) -> TestResponse {
    app.post_form(
        "/session/password",
        &[("email", email), ("password", password)],
    )
    .await
}

fn signed_in(app: &TestApp) -> bool {
    app.cookie("session_token").is_some()
}

/// Signs up and returns the verification link path.
async fn sign_up(app: &TestApp, email: &str, password: &str) -> String {
    app.post_form("/registration", &[("email", email), ("password", password)])
        .await
        .assert_redirect_to("/registration/sent");
    app.settle().await;
    let mail = app.last_email_to(email).expect("verification email");
    app.link_in(&mail, "/email-verification/")
}

// ---------------------------------------------------------------------------------------------
// Sign-up and verification
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn signup_verifies_the_address_before_signing_in() {
    let app = open_app().await;
    let link = sign_up(&app, "ada@example.com", NEW_PASSWORD).await;
    assert!(!signed_in(&app), "no session before verification");
    let u = user::find_by_email(app.db(), "ada@example.com")
        .await
        .unwrap()
        .unwrap();
    assert!(!u.is_verified());
    // Same browser: one click (GET is safe, POST verifies and signs in).
    app.get(&link)
        .await
        .assert_ok()
        .assert_not_contains("name=\"password\"");
    app.post_form(&link, &[])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    assert!(signed_in(&app));
    assert!(user::find_self(app.db(), u.id).await.unwrap().is_verified());
    // The link worked once.
    app.clear_cookies();
    app.post_form(&link, &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unverified_accounts_cant_sign_in_and_get_a_fresh_link() {
    let app = open_app().await;
    sign_up(&app, "ada@example.com", NEW_PASSWORD).await;
    sign_in(&app, "ada@example.com", NEW_PASSWORD)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_text("Confirm your email address first");
    assert!(!signed_in(&app));
    app.settle().await;
    let link = app.link_in(
        &app.last_email_to("ada@example.com").unwrap(),
        "/email-verification/",
    );
    // The resent link asks for the password (it isn't tied to a browser).
    app.get(&link)
        .await
        .assert_ok()
        .assert_contains("autocomplete=\"current-password\"");
    app.post_form(&link, &[("password", NEW_PASSWORD)])
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn verification_in_another_browser_needs_the_password() {
    let app = open_app().await;
    let link = sign_up(&app, "ada@example.com", NEW_PASSWORD).await;
    app.clear_cookies(); // another browser (a phone's mail app)
    app.get(&link)
        .await
        .assert_ok()
        .assert_contains("autocomplete=\"current-password\"");
    app.post_form(&link, &[("password", "not the right one at all")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!signed_in(&app));
    app.post_form(&link, &[("password", NEW_PASSWORD)])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    assert!(signed_in(&app));
}

#[tokio::test]
async fn signup_with_an_existing_email_looks_identical() {
    let app = open_app().await;
    factories::user_with_password(app.db(), "taken@example.com").await;
    let fresh = app
        .post_form(
            "/registration",
            &[("email", "fresh@example.com"), ("password", NEW_PASSWORD)],
        )
        .await;
    let taken = app
        .post_form(
            "/registration",
            &[("email", "taken@example.com"), ("password", NEW_PASSWORD)],
        )
        .await;
    assert_eq!(fresh.status, taken.status);
    assert_eq!(fresh.location(), taken.location());
    assert_eq!(fresh.body, taken.body);
    let names = |r: &TestResponse| {
        r.set_cookies()
            .iter()
            .map(|c| c.split('=').next().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&fresh), names(&taken));
    app.settle().await;
    // Only the mailbox owner learns the difference.
    assert!(
        app.last_email_to("fresh@example.com")
            .unwrap()
            .subject
            .contains("Confirm")
    );
    assert!(
        app.last_email_to("taken@example.com")
            .unwrap()
            .subject
            .contains("already have")
    );
    // The existing account's password is untouched.
    app.clear_cookies();
    sign_in(&app, "taken@example.com", PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn the_latest_signup_wins_for_an_unverified_address() {
    // Pre-hijacking: an attacker signs up with the victim's address first.
    let app = open_app().await;
    let attackers_link = sign_up(&app, "victim@example.com", "attacker chosen secret").await;
    app.clear_cookies();
    let victims_link = sign_up(&app, "victim@example.com", NEW_PASSWORD).await;
    app.clear_cookies();
    app.post_form(&attackers_link, &[("password", "attacker chosen secret")])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.post_form(&victims_link, &[("password", NEW_PASSWORD)])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    app.clear_cookies();
    sign_in(&app, "victim@example.com", "attacker chosen secret")
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn weak_passwords_are_refused_at_signup_with_a_reason() {
    let app = open_app().await;
    for (pw, reason) in [
        ("short", "too short"),
        ("password1234", "too common"),
        ("P@ssw0rd2024!", "too common"),
        ("abcdefghijklmn", "too predictable"),
        ("ada.lovelace 2024!", "contain your email"),
    ] {
        let res = app
            .post_form(
                "/registration",
                &[("email", "ada.lovelace@example.com"), ("password", pw)],
            )
            .await;
        res.assert_status(StatusCode::UNPROCESSABLE_ENTITY)
            .assert_contains(reason)
            .assert_contains("aria-invalid=\"true\"")
            .assert_contains("aria-describedby=\"password_strength password_error\"");
        // The password is never echoed back into the form.
        assert!(!res.body.contains(&format!("value=\"{pw}\"")));
    }
    assert!(
        user::find_by_email(app.db(), "ada.lovelace@example.com")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn signup_is_rate_limited_per_address() {
    let app = open_app().await;
    for _ in 0..3 {
        app.post_form(
            "/registration",
            &[("email", "ada@example.com"), ("password", NEW_PASSWORD)],
        )
        .await
        .assert_redirect_to("/registration/sent");
    }
    app.post_form(
        "/registration",
        &[("email", "ada@example.com"), ("password", NEW_PASSWORD)],
    )
    .await
    .assert_status(StatusCode::TOO_MANY_REQUESTS)
    .assert_text("Too many sign-up attempts");
}

// ---------------------------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn password_sign_in_starts_a_session_and_honors_return_to() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    app.get("/account/email/edit")
        .await
        .assert_redirect_to("/session/new");
    sign_in(&app, " ADA@example.com ", PASSWORD)
        .await
        .assert_redirect_to("/account/email/edit");
    app.get("/account").await.assert_ok();
    let sessions = session::all_for(app.db(), &ada).await.unwrap();
    assert_eq!(sessions[0].sign_in_method, "password");
}

#[tokio::test]
async fn sign_in_form_fields_carry_autocomplete_hints() {
    let app = open_app().await;
    app.get("/session/new")
        .await
        .assert_contains("autocomplete=\"username\"")
        .assert_contains("autocomplete=\"current-password\"")
        .assert_contains("data-reveal=\"signin_password\"");
    app.get("/registration/new")
        .await
        .assert_contains("autocomplete=\"new-password\"")
        .assert_contains("autocomplete=\"email\"");
}

#[tokio::test]
async fn unknown_and_known_emails_take_the_same_path() {
    let app = TestApp::new().await;
    factories::user_with_password(app.db(), "ada@example.com").await;
    let before = app.query_count();
    let verified = password_credential::verifies_so_far();
    let wrong = sign_in(&app, "ada@example.com", "definitely not it").await;
    let known_queries = app.query_count() - before;
    let known_verifies = password_credential::verifies_so_far() - verified;
    let before = app.query_count();
    let verified = password_credential::verifies_so_far();
    let unknown = sign_in(&app, "nobody@example.com", "definitely not it").await;
    let unknown_queries = app.query_count() - before;
    let unknown_verifies = password_credential::verifies_so_far() - verified;
    // The timing defense itself: an unknown address runs exactly one argon2 verify (against the
    // dummy hash), like a known one, so the response time doesn't enumerate accounts.
    assert_eq!(known_verifies, 1, "a known address costs one argon2 verify");
    assert_eq!(
        unknown_verifies, 1,
        "an unknown address costs one argon2 verify too"
    );
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(wrong.status, unknown.status);
    assert_eq!(
        wrong.body.replace("ada@example.com", "X"),
        unknown.body.replace("nobody@example.com", "X"),
        "identical pages apart from the echoed address"
    );
    assert_eq!(
        known_queries, unknown_queries,
        "the same database work on both paths"
    );
    wrong.assert_text("Try another email address or password.");
}

#[tokio::test]
async fn sign_in_rehashes_passwords_made_with_old_parameters() {
    use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let weak = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(8 * 1024, 1, 1, None).unwrap(),
    )
    .hash_password(PASSWORD.as_bytes())
    .unwrap()
    .to_string();
    password_credential::set(app.db(), ada.id, weak.clone(), Utc::now())
        .await
        .unwrap();
    sign_in(&app, "ada@example.com", PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
    let stored = password_credential::Entity::find_by_id(ada.id)
        .one(app.db())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(stored.digest, weak);
    assert!(password_credential::current(&stored.digest));
}

#[tokio::test]
async fn wrong_passwords_are_throttled_progressively_even_for_unknown_emails() {
    for email in ["ada@example.com", "nobody@example.com"] {
        // A fresh app per address, so the per-network limit (10 per 3 minutes) isn't what trips.
        let app = TestApp::new().await;
        factories::user_with_password(app.db(), "ada@example.com").await;
        for _ in 0..auth_throttle::FREE_FAILURES {
            sign_in(&app, email, "wrong wrong wrong")
                .await
                .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
        }
        let paused = sign_in(&app, email, PASSWORD).await;
        paused
            .assert_status(StatusCode::TOO_MANY_REQUESTS)
            .assert_text("Too many failed attempts for this account. Try again in ");
        assert!(paused.headers.get(header::RETRY_AFTER).is_some());
    }
}

#[tokio::test]
async fn the_owner_is_emailed_when_sign_in_is_paused() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    for _ in 0..auth_throttle::FREE_FAILURES {
        sign_in(&app, "ada@example.com", "wrong wrong wrong").await;
    }
    app.settle().await;
    let mail = app
        .last_email_to("ada@example.com")
        .expect("a paused-sign-in notice");
    assert!(mail.subject.contains("paused"));
    let kinds = auth_event::kinds_for(app.db(), ada.id).await.unwrap();
    assert!(kinds.contains(&Kind::Throttled));
    assert_eq!(
        kinds.iter().filter(|k| **k == Kind::SignInFailed).count(),
        5
    );
}

#[tokio::test]
async fn a_reset_unlocks_sign_in() {
    let app = TestApp::new().await;
    factories::user_with_password(app.db(), "ada@example.com").await;
    for _ in 0..auth_throttle::FREE_FAILURES {
        sign_in(&app, "ada@example.com", "wrong wrong wrong").await;
    }
    sign_in(&app, "ada@example.com", PASSWORD)
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    let edit = reset_link(&app, "ada@example.com").await;
    app.post_form(
        edit.trim_end_matches("/edit"),
        &[("password", NEW_PASSWORD)],
    )
    .await
    .assert_redirect_to(&paths::after_sign_in());
    app.clear_cookies();
    sign_in(&app, "ada@example.com", NEW_PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn sign_in_is_rate_limited_per_network() {
    let app = TestApp::new().await;
    for i in 0..10 {
        sign_in(&app, &format!("user{i}@example.com"), "wrong wrong wrong")
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    sign_in(&app, "another@example.com", "wrong wrong wrong")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS)
        .assert_text("Too many sign-in attempts from your network");
}

#[tokio::test]
async fn a_new_device_gets_a_sign_in_notice_and_a_known_one_doesnt() {
    let app = TestApp::new().await;
    factories::user_with_password(app.db(), "ada@example.com").await;
    sign_in(&app, "ada@example.com", PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
    app.settle().await;
    assert!(
        app.last_email_to("ada@example.com").is_none(),
        "no notice for the first sign-in"
    );
    // Same browser (it kept its device cookie): no notice.
    app.post_form("/session/delete", &[]).await;
    sign_in(&app, "ada@example.com", PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
    app.settle().await;
    assert!(app.last_email_to("ada@example.com").is_none());
    // A new browser: a notice with a "this wasn't me" link.
    app.clear_cookies();
    sign_in(&app, "ada@example.com", PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
    app.settle().await;
    let mail = app
        .last_email_to("ada@example.com")
        .expect("new sign-in notice");
    assert!(mail.subject.contains("New sign-in"));
    assert!(mail.text.contains("/security/not-me/"));
}

// ---------------------------------------------------------------------------------------------
// Password reset
// ---------------------------------------------------------------------------------------------

async fn reset_link(app: &TestApp, email: &str) -> String {
    app.post_form("/passwords", &[("email", email)])
        .await
        .assert_redirect_to("/passwords/sent");
    app.settle().await;
    let mail = app
        .emails_to(email)
        .into_iter()
        .rev()
        .find(|m| m.subject.starts_with("Reset"))
        .expect("reset email");
    app.link_in(&mail, "/passwords/")
}

#[tokio::test]
async fn reset_requests_look_identical_for_unknown_emails() {
    let app = TestApp::new().await;
    factories::user_with_password(app.db(), "ada@example.com").await;
    let known = app
        .post_form("/passwords", &[("email", "ada@example.com")])
        .await;
    let unknown = app
        .post_form("/passwords", &[("email", "nobody@example.com")])
        .await;
    assert_eq!(known.status, unknown.status);
    assert_eq!(known.location(), unknown.location());
    assert_eq!(known.body, unknown.body);
    assert_eq!(known.set_cookies(), unknown.set_cookies());
    // Both paths write the audit row (the unknown one without a user): the same request work.
    // The email (and its token) is minted after the response, in the background.
    let requested = auth_event::Entity::find()
        .all(app.db())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == Kind::PasswordResetRequested)
        .count();
    assert_eq!(requested, 2);
    app.settle().await;
    assert!(app.last_email_to("ada@example.com").is_some());
    assert!(app.last_email_to("nobody@example.com").is_none());
}

#[tokio::test]
async fn a_reset_link_works_once() {
    let app = TestApp::new().await;
    factories::user_with_password(app.db(), "ada@example.com").await;
    let edit = reset_link(&app, "ada@example.com").await;
    let update = edit.trim_end_matches("/edit").to_owned();
    // A weak password re-renders the form and the link keeps working.
    app.post_form(&update, &[("password", "password1234")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    app.get(&edit).await.assert_ok();
    app.post_form(&update, &[("password", NEW_PASSWORD)])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    app.clear_cookies();
    app.get(&edit).await.assert_status(StatusCode::NOT_FOUND);
    app.post_form(&update, &[("password", "another fine passphrase")])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    sign_in(&app, "ada@example.com", NEW_PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn a_reset_ends_every_other_session() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    let (_, stolen) = session::start(app.db(), ada.id, "password", None, None, Utc::now())
        .await
        .unwrap();
    let edit = reset_link(&app, "ada@example.com").await;
    app.post_form(
        edit.trim_end_matches("/edit"),
        &[("password", NEW_PASSWORD)],
    )
    .await
    .assert_redirect_to(&paths::after_sign_in());
    assert!(
        session::find_by_token(app.db(), &stolen, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    app.settle().await;
    assert!(
        app.last_email_to("ada@example.com")
            .unwrap()
            .subject
            .contains("password was changed")
    );
    assert!(
        auth_event::kinds_for(app.db(), ada.id)
            .await
            .unwrap()
            .contains(&Kind::PasswordReset)
    );
}

/// An email change someone started (its link on the way to their address) dies when the owner
/// resets the password, or it would move the account after the owner secured it.
async fn pending_email_change(app: &TestApp, user_id: i64) -> String {
    user_token::issue(
        app.db(),
        user_id,
        user_token::Purpose::EmailChange,
        "mallory@example.com",
        None,
        Utc::now(),
    )
    .await
    .unwrap()
}

async fn assert_email_change_dead(app: &TestApp, token: &str) {
    assert!(
        user_token::find_valid(
            app.db(),
            user_token::Purpose::EmailChange,
            token,
            Utc::now()
        )
        .await
        .unwrap()
        .is_none(),
        "the pending email change is revoked"
    );
    app.post_form(&format!("/account/email/confirm/{token}"), &[])
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_password_reset_cancels_a_pending_email_change() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    let change = pending_email_change(&app, ada.id).await;
    let edit = reset_link(&app, "ada@example.com").await;
    app.post_form(
        edit.trim_end_matches("/edit"),
        &[("password", NEW_PASSWORD)],
    )
    .await
    .assert_redirect_to(&paths::after_sign_in());
    assert_email_change_dead(&app, &change).await;
    assert_eq!(
        user::find_self(app.db(), ada.id).await.unwrap().email,
        "ada@example.com"
    );
}

#[tokio::test]
async fn a_password_change_cancels_a_pending_email_change() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    let change = pending_email_change(&app, ada.id).await;
    app.post_form(
        "/account/password",
        &[("current_password", PASSWORD), ("password", NEW_PASSWORD)],
    )
    .await
    .assert_redirect_to(&paths::account());
    assert_email_change_dead(&app, &change).await;
}

#[tokio::test]
async fn reset_links_expire_and_die_when_the_password_changes() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    let old = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::PasswordReset,
        &ada.email,
        None,
        Utc::now() - Duration::minutes(31),
    )
    .await
    .unwrap();
    app.get(&format!("/passwords/{old}/edit"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // A live link dies when the password is changed from the account page.
    let edit = reset_link(&app, "ada@example.com").await;
    app.sign_in_as(&ada).await;
    app.post_form(
        "/account/password",
        &[("current_password", PASSWORD), ("password", NEW_PASSWORD)],
    )
    .await
    .assert_redirect_to(&paths::account());
    app.clear_cookies();
    app.get(&edit).await.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reset_pages_are_not_cached_and_send_no_referrer() {
    let app = TestApp::new().await;
    factories::user_with_password(app.db(), "ada@example.com").await;
    let edit = reset_link(&app, "ada@example.com").await;
    let res = app.get(&edit).await;
    res.assert_ok();
    assert_eq!(res.headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert_eq!(
        res.headers.get(header::REFERRER_POLICY).unwrap(),
        "no-referrer"
    );
    let token = edit
        .trim_start_matches("/passwords/")
        .trim_end_matches("/edit");
    assert!(!res.body.contains(&format!(
        "canonical\" href=\"http://localhost:3000/passwords/{token}"
    )));
}

// ---------------------------------------------------------------------------------------------
// Changing the password and re-auth
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn changing_the_password_needs_the_current_one() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    app.post_form(
        "/account/password",
        &[
            ("current_password", "wrong wrong wrong"),
            ("password", NEW_PASSWORD),
        ],
    )
    .await
    .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
    .assert_text("Current password is incorrect");
    app.post_form(
        "/account/password",
        &[("current_password", PASSWORD), ("password", PASSWORD)],
    )
    .await
    .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
    .assert_text("must be different");
    assert!(
        password_credential::check(app.db(), Some(ada.id), PASSWORD)
            .await
            .unwrap()
            .ok
    );
}

#[tokio::test]
async fn changing_the_password_signs_out_other_devices_and_keeps_this_one() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    let (_, other) = session::start(app.db(), ada.id, "password", None, None, Utc::now())
        .await
        .unwrap();
    let mine = app.sign_in_as(&ada).await;
    app.post_form(
        "/account/password",
        &[("current_password", PASSWORD), ("password", NEW_PASSWORD)],
    )
    .await
    .assert_redirect_to(&paths::account());
    assert!(
        session::find_by_token(app.db(), &other, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        session::find_by_token(app.db(), &mine, Utc::now())
            .await
            .unwrap()
            .is_none(),
        "this device got a new token"
    );
    app.get("/account").await.assert_ok();
    app.settle().await;
    assert!(
        app.last_email_to("ada@example.com")
            .unwrap()
            .subject
            .contains("password was changed")
    );
}

#[tokio::test]
async fn a_passwordless_account_can_set_a_password() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    app.get("/account").await.assert_contains("Set a password");
    app.get("/account/password/edit")
        .await
        .assert_ok()
        .assert_not_contains("current_password");
    app.post_form("/account/password", &[("password", NEW_PASSWORD)])
        .await
        .assert_redirect_to(&paths::account());
    app.clear_cookies();
    sign_in(&app, "ada@example.com", NEW_PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
}

#[tokio::test]
async fn password_reauth_confirms_and_returns() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    age_sessions(&app, 30).await;
    app.get("/account/password/edit")
        .await
        .assert_redirect_to("/account/confirm");
    app.get("/account/confirm")
        .await
        .assert_ok()
        .assert_contains("/account/confirm/password");
    app.post_form(
        "/account/confirm/password",
        &[("password", "wrong wrong wrong")],
    )
    .await
    .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
    .assert_text("Password is incorrect");
    app.post_form("/account/confirm/password", &[("password", PASSWORD)])
        .await
        .assert_redirect_to("/account/password/edit");
    app.get("/account/password/edit").await.assert_ok();
}

#[tokio::test]
async fn password_reauth_is_throttled() {
    let app = TestApp::new().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    for _ in 0..auth_throttle::FREE_FAILURES {
        app.post_form(
            "/account/confirm/password",
            &[("password", "wrong wrong wrong")],
        )
        .await;
    }
    app.post_form("/account/confirm/password", &[("password", PASSWORD)])
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------------------------
// The strength meter, logs
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_strength_meter_works_by_post_only() {
    let app = open_app().await;
    app.post_form("/password-strength", &[("password", "password1234")])
        .await
        .assert_ok()
        .assert_contains("Too weak");
    app.post_form(
        "/password-strength",
        &[("password", "the kettle sings at dawn")],
    )
    .await
    .assert_ok()
    .assert_contains("Strong");
    // GET (a password in a URL, in access logs) isn't routed.
    let res = app.get("/password-strength?password=x").await;
    assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED);
    // Sign-up re-renders the meter server-side, so it works without JavaScript.
    app.post_form(
        "/registration",
        &[("email", "bad"), ("password", "violet tractor sandwich")],
    )
    .await
    .assert_contains("w-full bg-green-600");
}

#[tokio::test]
async fn passwords_and_reset_tokens_never_reach_the_logs() {
    let logs = LogCapture::start();
    let app = open_app().await;
    factories::user_with_password(app.db(), "ada@example.com").await;
    sign_in(&app, "ada@example.com", "a wrong guess here").await;
    sign_in(&app, "ada@example.com", PASSWORD).await;
    app.post_form("/password-strength", &[("password", "a meter only secret")])
        .await;
    let edit = reset_link(&app, "ada@example.com").await;
    let token = edit
        .trim_start_matches("/passwords/")
        .trim_end_matches("/edit")
        .to_owned();
    app.post_form(
        edit.trim_end_matches("/edit"),
        &[("password", NEW_PASSWORD)],
    )
    .await;
    app.post_form(
        "/registration",
        &[
            ("email", "bob@example.com"),
            ("password", "bobs own passphrase"),
        ],
    )
    .await;
    app.settle().await;
    logs.assert_absent(&[
        PASSWORD,
        "a wrong guess here",
        "a meter only secret",
        NEW_PASSWORD,
        "bobs own passphrase",
        &token,
    ]);
}

#[tokio::test]
async fn sign_in_params_debug_hides_the_password() {
    let params = offrails_host::controllers::password_sessions::SignInParams {
        email: "ada@example.com".into(),
        password: "hunter2 hunter2".into(),
    };
    assert!(!format!("{params:?}").contains("hunter2"));
}

#[tokio::test]
async fn a_signup_never_overwrites_a_verified_accounts_password() {
    let app = open_app().await;
    let ada = factories::user_with_password(app.db(), "ada@example.com").await;
    app.post_form(
        "/registration",
        &[("email", "ada@example.com"), ("password", NEW_PASSWORD)],
    )
    .await;
    assert!(
        password_credential::check(app.db(), Some(ada.id), PASSWORD)
            .await
            .unwrap()
            .ok
    );
    assert!(
        !password_credential::check(app.db(), Some(ada.id), NEW_PASSWORD)
            .await
            .unwrap()
            .ok
    );
}

#[tokio::test]
async fn password_pages_follow_the_theme_switch() {
    let app = open_app().await;
    crate::auth_core_test::assert_pages_follow_the_theme(
        &app,
        &["/registration/new", "/passwords/new"],
    )
    .await;
    let user = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&user).await;
    crate::auth_core_test::assert_pages_follow_the_theme(&app, &["/account/password/edit"]).await;
}

// ---------------------------------------------------------------------------------------------
// Invite-only (`Signup::Invite`): no public sign-up; invitations set the first password
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn invite_only_has_no_public_sign_up() {
    let app = app_with_signup(Signup::Invite).await;
    app.get("/registration/new")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.post_form(
        "/registration",
        &[("email", "ada@example.com"), ("password", NEW_PASSWORD)],
    )
    .await
    .assert_status(StatusCode::NOT_FOUND);
    app.get("/registration/sent")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.settle().await;
    assert!(
        user::find_by_email(app.db(), "ada@example.com")
            .await
            .unwrap()
            .is_none()
    );
    assert!(app.last_email_to("ada@example.com").is_none());
    // No "Sign up" in the nav, no "Create an account" under the form.
    app.get("/session/new")
        .await
        .assert_ok()
        .assert_not_contains("/registration/new")
        .assert_text("Accounts are by invitation.");
    // The same app with sign-up open shows both.
    let open = open_app().await;
    open.get("/registration/new").await.assert_ok();
    open.get("/session/new")
        .await
        .assert_contains("href=\"/registration/new\"")
        .assert_text("Create an account");
}

#[tokio::test]
async fn an_invitation_sets_a_password_and_verifies_the_address() {
    let app = app_with_signup(Signup::Invite).await;
    auth_invite::invite(&app.state, "ada@example.com")
        .await
        .unwrap();
    let mail = app.last_email_to("ada@example.com").expect("an invitation");
    assert!(mail.text.contains("Choose your password"));
    let edit = app.link_in(&mail, "/passwords/");
    let update = edit.trim_end_matches("/edit").to_owned();
    // No password yet: sign-in fails like any wrong password.
    sign_in(&app, "ada@example.com", NEW_PASSWORD)
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    app.get(&edit)
        .await
        .assert_ok()
        .assert_text("Choose your password")
        .assert_contains("autocomplete=\"new-password\"");
    app.post_form(&update, &[("password", "password1234")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    app.post_form(&update, &[("password", NEW_PASSWORD)])
        .await
        .assert_redirect_to(&paths::after_sign_in());
    assert!(signed_in(&app));
    let ada = user::find_by_email(app.db(), "ada@example.com")
        .await
        .unwrap()
        .unwrap();
    assert!(ada.is_verified(), "the invitation link proved the address");
    let kinds = auth_event::kinds_for(app.db(), ada.id).await.unwrap();
    assert!(kinds.contains(&Kind::Invited) && kinds.contains(&Kind::EmailVerified));
    // Single use.
    app.clear_cookies();
    app.get(&edit).await.assert_status(StatusCode::NOT_FOUND);
    // The password works from now on; a first password sends no "password changed" alarm.
    sign_in(&app, "ada@example.com", NEW_PASSWORD)
        .await
        .assert_redirect_to(&paths::after_sign_in());
    app.settle().await;
    assert!(
        app.emails_to("ada@example.com")
            .iter()
            .all(|m| !m.subject.contains("password was changed"))
    );
}

#[tokio::test]
async fn invitation_links_expire_and_a_new_invitation_replaces_the_old() {
    let app = app_with_signup(Signup::Invite).await;
    let ada = auth_invite::invite(&app.state, "ada@example.com")
        .await
        .unwrap();
    let first = app.link_in(
        &app.last_email_to("ada@example.com").unwrap(),
        "/passwords/",
    );
    auth_invite::invite(&app.state, "ada@example.com")
        .await
        .unwrap();
    let second = app.link_in(
        &app.last_email_to("ada@example.com").unwrap(),
        "/passwords/",
    );
    assert_ne!(first, second);
    app.get(&first).await.assert_status(StatusCode::NOT_FOUND);
    app.get(&second).await.assert_ok();
    let old = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::Invitation,
        &ada.email,
        None,
        Utc::now() - Duration::days(8),
    )
    .await
    .unwrap();
    app.get(&format!("/passwords/{old}/edit"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // An invitation link isn't a reset link for someone else's purpose: a reset still works.
    let edit = reset_link(&app, "ada@example.com").await;
    app.get(&edit)
        .await
        .assert_ok()
        .assert_text("Choose a new password");
}

#[tokio::test]
async fn sign_in_reset_and_verification_work_in_both_signup_modes() {
    for mode in [Signup::Open, Signup::Invite] {
        let app = app_with_signup(mode).await;
        factories::user_with_password(app.db(), "ada@example.com").await;
        sign_in(&app, "ada@example.com", PASSWORD)
            .await
            .assert_redirect_to(&paths::after_sign_in());
        app.clear_cookies();
        let edit = reset_link(&app, "ada@example.com").await;
        app.post_form(
            edit.trim_end_matches("/edit"),
            &[("password", NEW_PASSWORD)],
        )
        .await
        .assert_redirect_to(&paths::after_sign_in());
        // An account that signed up before the switch and never verified still can.
        app.clear_cookies();
        let bob = user::find_or_create(app.db(), "bob@example.com", Utc::now())
            .await
            .unwrap();
        let digest = password_credential::hash(NEW_PASSWORD.to_owned())
            .await
            .unwrap();
        password_credential::set(app.db(), bob.id, digest, Utc::now())
            .await
            .unwrap();
        sign_in(&app, "bob@example.com", NEW_PASSWORD)
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
            .assert_text("Confirm your email address first");
        app.settle().await;
        let link = app.link_in(
            &app.last_email_to("bob@example.com").unwrap(),
            "/email-verification/",
        );
        app.post_form(&link, &[("password", NEW_PASSWORD)])
            .await
            .assert_redirect_to(&paths::after_sign_in());
    }
}
