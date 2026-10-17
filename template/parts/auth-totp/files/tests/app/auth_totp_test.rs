//! auth-totp: the code step after any first factor, enrolment, replay protection, recovery codes,
//! trusted devices, re-auth with a code, and encryption at rest. On SQLite and Postgres.
//!
//! The first factor is completed through `auth::sign_in`, the function every sign-in method
//! (password, magic link, reset, verification) calls, so these tests hold with any of them.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use chrono::{Duration, Utc};
use offrails_host::{
    controllers::auth::{self, RequestMeta},
    models::{
        auth_event::{self, Kind, Meta},
        auth_throttle, pending_login, recovery_code, session, totp_credential, trusted_device,
        user, user_token,
    },
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait, IntoActiveModel};

use crate::auth_core_test::{LogCapture, age_sessions};
use crate::factories;
use crate::support::TestApp;

fn meta() -> RequestMeta {
    RequestMeta {
        meta: Meta::default(),
        device: None,
    }
}

/// A user with two-factor on; returns them and their recovery codes.
async fn enrolled(app: &TestApp, email: &str) -> (user::Model, Vec<String>) {
    let u = factories::user(app.db(), email).await;
    totp_credential::pending_for(app.db(), &app.state.config.totp, u.id, Utc::now())
        .await
        .unwrap();
    totp_credential::enable(app.db(), u.id, Utc::now())
        .await
        .unwrap();
    let codes = recovery_code::regenerate(
        app.db(),
        &app.state.config.auth.secret_key,
        u.id,
        Utc::now(),
    )
    .await
    .unwrap();
    (u, codes)
}

/// The code the user's authenticator shows `offset` steps from now.
async fn code(app: &TestApp, u: &user::Model, offset: i64) -> String {
    let cred = totp_credential::find_for(app.db(), u.id)
        .await
        .unwrap()
        .unwrap();
    let secret = totp_credential::secret_of(&app.state.config.totp, &cred).unwrap();
    let t = totp_credential::totp(&secret, &u.email).unwrap();
    t.generate((Utc::now().timestamp() + offset * 30) as u64)
        .to_string()
}

/// Completes the first factor as any sign-in method would; the browser jar gets its cookies.
async fn first_factor(app: &TestApp, u: &user::Model) -> axum::response::Response {
    let mut headers = HeaderMap::new();
    let jar: Vec<String> = ["trusted_device", "session_token"]
        .iter()
        .filter_map(|n| app.cookie(n).map(|v| format!("{n}={v}")))
        .collect();
    if !jar.is_empty() {
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&jar.join("; ")).unwrap(),
        );
    }
    let res = auth::sign_in(&app.state, &headers, &meta(), u.clone(), "password")
        .await
        .unwrap();
    for c in res.headers().get_all(header::SET_COOKIE) {
        let c = c.to_str().unwrap();
        if let Some((name, rest)) = c.split_once('=') {
            let value = rest.split(';').next().unwrap();
            if !value.is_empty() && !c.contains("Max-Age=0") {
                app.set_cookie(name, value);
            }
        }
    }
    res
}

fn signed_in(app: &TestApp) -> bool {
    app.cookie("session_token").is_some()
}

// ---------------------------------------------------------------------------------------------
// The second step
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_password_alone_doesnt_sign_in_when_two_factor_is_on() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let res = first_factor(&app, &ada).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers().get(header::LOCATION).unwrap(), "/two-factor");
    assert!(!signed_in(&app), "no session before the code");
    assert!(session::all_for(app.db(), &ada).await.unwrap().is_empty());
    app.get("/account").await.assert_redirect_to("/session/new");

    app.get("/two-factor")
        .await
        .assert_ok()
        .assert_contains("autocomplete=\"one-time-code\"")
        .assert_contains("inputmode=\"numeric\"");
    let c = code(&app, &ada, 0).await;
    app.post_form("/two-factor", &[("code", &c)])
        .await
        .assert_redirect_to("/account");
    assert!(signed_in(&app));
    assert_eq!(
        session::all_for(app.db(), &ada).await.unwrap()[0].sign_in_method,
        "password+totp"
    );
    assert!(
        app.cookie("pending_login").is_none(),
        "the pending sign-in is cleared"
    );
}

#[tokio::test]
async fn users_without_two_factor_sign_in_directly() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let res = first_factor(&app, &ada).await;
    assert_eq!(res.headers().get(header::LOCATION).unwrap(), "/account");
    assert!(signed_in(&app));
}

#[tokio::test]
async fn a_totp_code_works_only_once() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let c = code(&app, &ada, 0).await;
    first_factor(&app, &ada).await;
    app.post_form("/two-factor", &[("code", &c)])
        .await
        .assert_redirect_to("/account");
    // Replay the same code in a second sign-in (a shoulder-surfer, a phishing relay).
    app.clear_cookies();
    first_factor(&app, &ada).await;
    app.post_form("/two-factor", &[("code", &c)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    // An older step (the previous code) is refused too once a newer one was used.
    let previous = code(&app, &ada, -1).await;
    app.post_form("/two-factor", &[("code", &previous)])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    // The next code works.
    let next = code(&app, &ada, 1).await;
    app.post_form("/two-factor", &[("code", &next)])
        .await
        .assert_redirect_to("/account");
}

#[tokio::test]
async fn two_racing_requests_with_one_code_sign_in_once() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let cred = totp_credential::find_for(app.db(), ada.id)
        .await
        .unwrap()
        .unwrap();
    let c = code(&app, &ada, 0).await;
    let config = &app.state.config.totp;
    let (a, b) = tokio::join!(
        totp_credential::verify(app.db(), config, &cred, &ada.email, &c, Utc::now()),
        totp_credential::verify(app.db(), config, &cred, &ada.email, &c, Utc::now()),
    );
    assert_eq!([a.unwrap(), b.unwrap()].iter().filter(|ok| **ok).count(), 1);
}

#[tokio::test]
async fn a_pending_sign_in_allows_five_wrong_codes_then_starts_over() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    first_factor(&app, &ada).await;
    let right = code(&app, &ada, 0).await;
    let wrong = if right == "000000" {
        "111111"
    } else {
        "000000"
    };
    for _ in 0..(pending_login::MAX_ATTEMPTS - 1) {
        app.post_form("/two-factor", &[("code", wrong)])
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
            .assert_contains("aria-invalid=\"true\"");
    }
    app.post_form("/two-factor", &[("code", wrong)])
        .await
        .assert_redirect_to("/session/new");
    // Even the right code is useless now: the password must be entered again.
    app.post_form("/two-factor", &[("code", &right)])
        .await
        .assert_redirect_to("/session/new");
    assert!(!signed_in(&app));
    assert!(
        auth_event::kinds_for(app.db(), ada.id)
            .await
            .unwrap()
            .contains(&Kind::TwoFactorFailed)
    );
}

#[tokio::test]
async fn wrong_codes_are_throttled_per_account_across_sign_ins() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let right = code(&app, &ada, 0).await;
    let wrong = if right == "000000" {
        "111111"
    } else {
        "000000"
    };
    // Five wrong codes spread over fresh pending sign-ins.
    for _ in 0..auth_throttle::FREE_FAILURES {
        app.clear_cookies();
        first_factor(&app, &ada).await;
        app.post_form("/two-factor", &[("code", wrong)]).await;
    }
    app.clear_cookies();
    first_factor(&app, &ada).await;
    app.post_form("/two-factor", &[("code", &right)])
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS)
        .assert_text("Too many wrong codes");
}

#[tokio::test]
async fn a_pending_sign_in_expires() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    first_factor(&app, &ada).await;
    for p in pending_login::Entity::find().all(app.db()).await.unwrap() {
        let mut a = p.into_active_model();
        a.expires_at = Set(Utc::now() - Duration::seconds(1));
        a.update(app.db()).await.unwrap();
    }
    let c = code(&app, &ada, 0).await;
    app.post_form("/two-factor", &[("code", &c)])
        .await
        .assert_redirect_to("/session/new");
    app.get("/two-factor")
        .await
        .assert_redirect_to("/session/new");
}

// ---------------------------------------------------------------------------------------------
// Recovery codes
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_recovery_code_works_only_once() {
    let app = TestApp::new().await;
    let (ada, codes) = enrolled(&app, "ada@example.com").await;
    assert_eq!(codes.len(), 10);
    first_factor(&app, &ada).await;
    app.post_form(
        "/two-factor/recovery",
        &[("code", &codes[0].to_lowercase())],
    )
    .await
    .assert_redirect_to("/account");
    assert_eq!(
        session::all_for(app.db(), &ada).await.unwrap()[0].sign_in_method,
        "recovery_code"
    );
    app.settle().await;
    let mail = app.last_email_to("ada@example.com").unwrap();
    assert!(mail.text.contains("9 recovery codes left"));
    app.get("/account").await.assert_text("Used 1 of 10");

    app.clear_cookies();
    first_factor(&app, &ada).await;
    app.post_form("/two-factor/recovery", &[("code", &codes[0])])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!signed_in(&app));
}

/// Two requests racing with the same recovery code: exactly one signs in.
#[tokio::test]
async fn a_recovery_code_used_twice_at_the_same_time_works_once() {
    let app = TestApp::new().await;
    let (ada, codes) = enrolled(&app, "ada@example.com").await;
    first_factor(&app, &ada).await;
    let form = [("code", codes[0].as_str())];
    let (a, b) = tokio::join!(
        app.post_form("/two-factor/recovery", &form),
        app.post_form("/two-factor/recovery", &form)
    );
    let wins = [&a, &b]
        .iter()
        .filter(|r| r.status == StatusCode::SEE_OTHER && r.location() == Some("/account"))
        .count();
    assert_eq!(wins, 1, "statuses: {} and {}", a.status, b.status);
    assert_eq!(session::all_for(app.db(), &ada).await.unwrap().len(), 1);
    assert_eq!(recovery_code::remaining(app.db(), ada.id).await.unwrap(), 9);
}

#[tokio::test]
async fn recovery_codes_are_stored_as_keyed_digests() {
    let app = TestApp::new().await;
    let (_, codes) = enrolled(&app, "ada@example.com").await;
    let rows = recovery_code::Entity::find().all(app.db()).await.unwrap();
    assert_eq!(rows.len(), 10);
    for r in rows {
        assert_eq!(r.code_digest.len(), 64);
        assert!(
            codes
                .iter()
                .all(|c| !r.code_digest.contains(&c.replace('-', "")))
        );
    }
}

#[tokio::test]
async fn regenerating_recovery_codes_voids_the_old_ones() {
    let app = TestApp::new().await;
    let (ada, old) = enrolled(&app, "ada@example.com").await;
    app.sign_in_as(&ada).await;
    let page = app
        .post_form("/account/two-factor/recovery-codes", &[])
        .await;
    page.assert_ok()
        .assert_text("Your new recovery codes")
        .assert_contains("download=\"recovery-codes.txt\"");
    assert_eq!(page.headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert!(
        !recovery_code::redeem(
            app.db(),
            &app.state.config.auth.secret_key,
            ada.id,
            &old[0],
            Utc::now()
        )
        .await
        .unwrap()
    );
    app.settle().await;
    assert!(
        app.last_email_to("ada@example.com")
            .unwrap()
            .subject
            .contains("recovery codes")
    );
}

// ---------------------------------------------------------------------------------------------
// Enrolment
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn enrolment_needs_a_valid_code_before_two_factor_turns_on() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    let (_, other) = session::start(app.db(), ada.id, "test", None, None, Utc::now())
        .await
        .unwrap();
    let mine = app.sign_in_as(&ada).await;
    let page = app.get("/account/two-factor/new").await;
    page.assert_ok()
        .assert_contains("src=\"data:image/svg+xml;base64,")
        .assert_contains("autocomplete=\"one-time-code\"")
        .assert_contains("id=\"manual_key\"");
    // Reloading keeps the same secret (a scanned QR code stays valid).
    let again = app.get("/account/two-factor/new").await;
    let qr = |b: &str| {
        b.split("data:image/svg+xml;base64,")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned()
    };
    assert_eq!(qr(&page.body), qr(&again.body));
    assert!(
        !totp_credential::enabled_for(app.db(), ada.id)
            .await
            .unwrap()
    );

    app.post_form("/account/two-factor", &[("code", "123456")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        !totp_credential::enabled_for(app.db(), ada.id)
            .await
            .unwrap(),
        "still off"
    );

    let c = code(&app, &ada, 0).await;
    let codes_page = app.post_form("/account/two-factor", &[("code", &c)]).await;
    codes_page
        .assert_ok()
        .assert_text("Save your recovery codes")
        .assert_contains("only time they're shown");
    assert_eq!(
        codes_page.headers.get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(
        totp_credential::enabled_for(app.db(), ada.id)
            .await
            .unwrap()
    );
    assert_eq!(
        recovery_code::remaining(app.db(), ada.id).await.unwrap(),
        10
    );
    // A privilege change: this session got a new token, other sessions ended.
    assert!(
        session::find_by_token(app.db(), &mine, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        session::find_by_token(app.db(), &other, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    app.get("/account")
        .await
        .assert_ok()
        .assert_contains("Turn off");
    app.settle().await;
    assert!(
        app.last_email_to("ada@example.com")
            .unwrap()
            .subject
            .contains("Two-factor authentication is on")
    );
}

#[tokio::test]
async fn the_enrolment_page_is_not_cached() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    let res = app.get("/account/two-factor/new").await;
    assert_eq!(res.headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
}

#[tokio::test]
async fn enrolment_needs_a_recent_reauth() {
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    age_sessions(&app, 30).await;
    app.get("/account/two-factor/new")
        .await
        .assert_redirect_to("/account/confirm");
}

#[tokio::test]
async fn totp_secrets_are_encrypted_at_rest_and_bound_to_their_owner() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let bob = factories::user(app.db(), "bob@example.com").await;
    let cred = totp_credential::find_for(app.db(), ada.id)
        .await
        .unwrap()
        .unwrap();
    let secret = totp_credential::secret_of(&app.state.config.totp, &cred).unwrap();
    assert!(cred.secret_ciphertext.starts_with("v1:"));
    let b32 = totp_credential::manual_key(&secret).replace(' ', "");
    assert!(!cred.secret_ciphertext.contains(&b32));
    // Copying Ada's row onto Bob doesn't give Bob a known secret: it doesn't decrypt.
    let copied = totp_credential::Model {
        user_id: bob.id,
        ..cred
    };
    assert!(totp_credential::secret_of(&app.state.config.totp, &copied).is_err());
}

// ---------------------------------------------------------------------------------------------
// Re-auth and turning it off
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn two_factor_users_must_step_up_with_a_code() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    app.sign_in_as(&ada).await;
    age_sessions(&app, 30).await;
    assert!(
        auth::step_up_needs_second_factor(&app.state, &ada)
            .await
            .unwrap()
    );
    app.get("/account/confirm")
        .await
        .assert_ok()
        .assert_contains("/account/confirm/two-factor")
        .assert_not_contains("/account/confirm/password\"")
        .assert_not_contains("Email me a code");
}

#[tokio::test]
async fn turning_two_factor_off_needs_a_fresh_code() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    app.sign_in_as(&ada).await;
    age_sessions(&app, 30).await;
    app.post_form("/account/two-factor/delete", &[])
        .await
        .assert_redirect_to("/account/confirm");
    assert!(
        totp_credential::enabled_for(app.db(), ada.id)
            .await
            .unwrap(),
        "still on"
    );
    app.post_form("/account/confirm/two-factor", &[("code", "000000")])
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    let c = code(&app, &ada, 0).await;
    app.post_form("/account/confirm/two-factor", &[("code", &c)])
        .await
        .assert_redirect_to("/account");
    app.post_form("/account/two-factor/delete", &[])
        .await
        .assert_redirect_to("/account");
    assert!(
        !totp_credential::enabled_for(app.db(), ada.id)
            .await
            .unwrap()
    );
    assert_eq!(recovery_code::remaining(app.db(), ada.id).await.unwrap(), 0);
    app.settle().await;
    assert!(
        app.last_email_to("ada@example.com")
            .unwrap()
            .subject
            .contains("turned off")
    );
    assert!(
        auth_event::kinds_for(app.db(), ada.id)
            .await
            .unwrap()
            .contains(&Kind::TwoFactorDisabled)
    );
}

#[tokio::test]
async fn a_recovery_code_also_confirms_identity() {
    let app = TestApp::new().await;
    let (ada, codes) = enrolled(&app, "ada@example.com").await;
    app.sign_in_as(&ada).await;
    age_sessions(&app, 30).await;
    app.post_form("/account/confirm/two-factor", &[("code", &codes[3])])
        .await
        .assert_redirect_to("/account");
    assert_eq!(recovery_code::remaining(app.db(), ada.id).await.unwrap(), 9);
}

// ---------------------------------------------------------------------------------------------
// Trusted devices
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_trusted_device_skips_the_code_for_thirty_days() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    first_factor(&app, &ada).await;
    let c = code(&app, &ada, 0).await;
    app.post_form("/two-factor", &[("code", &c), ("trust_device", "1")])
        .await
        .assert_redirect_to("/account");
    assert!(app.cookie("trusted_device").is_some());
    app.get("/account")
        .await
        .assert_ok()
        .assert_contains("Remembered devices");

    // Sign out, sign in again: no code needed on this browser.
    app.post_form("/session/delete", &[]).await;
    let res = first_factor(&app, &ada).await;
    assert_eq!(res.headers().get(header::LOCATION).unwrap(), "/account");
    assert!(signed_in(&app));

    // Another browser still needs the code.
    app.clear_cookies();
    let res = first_factor(&app, &ada).await;
    assert_eq!(res.headers().get(header::LOCATION).unwrap(), "/two-factor");

    // After 30 days the device needs a code again.
    for d in trusted_device::Entity::find().all(app.db()).await.unwrap() {
        let mut a = d.into_active_model();
        a.expires_at = Set(Utc::now() - Duration::seconds(1));
        a.update(app.db()).await.unwrap();
    }
    assert!(
        trusted_device::all_for(app.db(), &ada, Utc::now())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_trusted_device_cookie_only_works_for_its_own_account() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let (bob, _) = enrolled(&app, "bob@example.com").await;
    let raw = trusted_device::trust(app.db(), ada.id, None, None, Utc::now())
        .await
        .unwrap();
    app.set_cookie("trusted_device", &raw);
    let res = first_factor(&app, &bob).await;
    assert_eq!(res.headers().get(header::LOCATION).unwrap(), "/two-factor");
    assert!(!signed_in(&app));
}

#[tokio::test]
async fn forgetting_another_users_trusted_device_is_404() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let (bob, _) = enrolled(&app, "bob@example.com").await;
    trusted_device::trust(app.db(), bob.id, None, None, Utc::now())
        .await
        .unwrap();
    let bobs = trusted_device::all_for(app.db(), &bob, Utc::now())
        .await
        .unwrap();
    app.sign_in_as(&ada).await;
    app.post_form(
        &format!("/account/trusted-devices/{}/delete", bobs[0].id),
        &[],
    )
    .await
    .assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        trusted_device::all_for(app.db(), &bob, Utc::now())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn credential_changes_and_not_me_forget_trusted_devices() {
    let app = TestApp::new().await;
    let (ada, _) = enrolled(&app, "ada@example.com").await;
    let none_trusted = async || {
        trusted_device::all_for(app.db(), &ada, Utc::now())
            .await
            .unwrap()
            .is_empty()
    };

    // "This wasn't me", through its link.
    trusted_device::trust(app.db(), ada.id, None, None, Utc::now())
        .await
        .unwrap();
    let not_me = user_token::issue(
        app.db(),
        ada.id,
        user_token::Purpose::NotMe,
        &ada.email,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    app.post_form(&format!("/security/not-me/{not_me}"), &[])
        .await
        .assert_redirect_to("/session/new");
    assert!(none_trusted().await, "not-me forgets trusted devices");

    // A password change or reset (the helpers they call).
    trusted_device::trust(app.db(), ada.id, None, None, Utc::now())
        .await
        .unwrap();
    let txn = offrails_host::kernel::db::begin(app.db()).await.unwrap();
    auth::revoke_everything(&txn, ada.id).await.unwrap();
    txn.commit().await.unwrap();
    assert!(none_trusted().await);
    trusted_device::trust(app.db(), ada.id, None, None, Utc::now())
        .await
        .unwrap();
    let txn = offrails_host::kernel::db::begin(app.db()).await.unwrap();
    auth::revoke_other_access(&txn, &ada, 0).await.unwrap();
    txn.commit().await.unwrap();
    assert!(none_trusted().await);

    // Turning two-factor off, through its route.
    trusted_device::trust(app.db(), ada.id, None, None, Utc::now())
        .await
        .unwrap();
    app.sign_in_as(&ada).await;
    app.post_form("/account/two-factor/delete", &[])
        .await
        .assert_redirect_to("/account");
    assert!(none_trusted().await, "turning two-factor off forgets them");
    assert!(
        !totp_credential::enabled_for(app.db(), ada.id)
            .await
            .unwrap()
    );
    assert_eq!(recovery_code::remaining(app.db(), ada.id).await.unwrap(), 0);
}

/// An unverified account claimed by its address's owner (auth-magic-link) loses any two-factor
/// set up on it before: it may be the attacker's.
#[tokio::test]
async fn claiming_an_unverified_account_drops_its_two_factor() {
    let app = TestApp::new().await;
    let u = user::find_or_create(app.db(), "ada@example.com", Utc::now())
        .await
        .unwrap();
    assert!(!u.is_verified());
    totp_credential::pending_for(app.db(), &app.state.config.totp, u.id, Utc::now())
        .await
        .unwrap();
    totp_credential::enable(app.db(), u.id, Utc::now())
        .await
        .unwrap();
    recovery_code::regenerate(
        app.db(),
        &app.state.config.auth.secret_key,
        u.id,
        Utc::now(),
    )
    .await
    .unwrap();
    trusted_device::trust(app.db(), u.id, None, None, Utc::now())
        .await
        .unwrap();
    let txn = offrails_host::kernel::db::begin(app.db()).await.unwrap();
    assert!(
        auth::claim_unverified_account(&txn, u.id, Utc::now())
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();
    assert!(
        totp_credential::find_for(app.db(), u.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(recovery_code::remaining(app.db(), u.id).await.unwrap(), 0);
    assert!(
        trusted_device::all_for(app.db(), &u, Utc::now())
            .await
            .unwrap()
            .is_empty()
    );
    // A verified account is never claimed: its two-factor stays.
    let (ada, _) = enrolled(&app, "verified@example.com").await;
    let txn = offrails_host::kernel::db::begin(app.db()).await.unwrap();
    assert!(
        !auth::claim_unverified_account(&txn, ada.id, Utc::now())
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();
    assert!(
        totp_credential::enabled_for(app.db(), ada.id)
            .await
            .unwrap()
    );
}

// ---------------------------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn totp_secrets_and_codes_never_reach_the_logs() {
    let logs = LogCapture::start();
    let app = TestApp::new().await;
    let ada = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&ada).await;
    app.get("/account/two-factor/new").await;
    let c = code(&app, &ada, 0).await;
    let page = app.post_form("/account/two-factor", &[("code", &c)]).await;
    let cred = totp_credential::find_for(app.db(), ada.id)
        .await
        .unwrap()
        .unwrap();
    let secret = totp_credential::secret_of(&app.state.config.totp, &cred).unwrap();
    let b32 = totp_credential::manual_key(&secret).replace(' ', "");
    let recovery: Vec<String> = page
        .body
        .split("<li>")
        .skip(1)
        .filter_map(|s| s.split("</li>").next())
        .filter(|s| s.len() == 11 && s.contains('-'))
        .map(str::to_owned)
        .collect();
    assert_eq!(recovery.len(), 10);
    app.settle().await;
    let mut secrets: Vec<&str> = vec![&b32];
    secrets.extend(recovery.iter().map(String::as_str));
    logs.assert_absent(&secrets);
}

#[tokio::test]
async fn two_factor_pages_follow_the_theme_switch() {
    let app = TestApp::new().await;
    let user = factories::user(app.db(), "ada@example.com").await;
    app.sign_in_as(&user).await;
    crate::auth_core_test::assert_pages_follow_the_theme(&app, &["/account/two-factor/new"]).await;
}
