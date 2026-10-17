//! auth-magic-link with auth-password installed too (compiled only then, through the optional
//! `test mods` wire): account pre-hijacking across the two sign-in methods.
//!
//! The attack: Mallory signs up with Ada's address and a password Mallory knows. The address is
//! never verified, so Mallory can't sign in yet. Later Ada signs in by emailed link or code, which
//! proves the address and verifies the account. If Mallory's password survived that, Mallory
//! would now sign in to Ada's verified account.

use axum::http::StatusCode;
use offrails_host::models::{password_credential, user, user_token};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::auth_magic_link_test::{code_in, mail_for, signed_in};
use crate::support::TestApp;

const MALLORYS_PASSWORD: &str = "mallory knows this one";

/// Mallory signs up with Ada's address; returns Mallory's verification link.
async fn mallory_registers_adas_address(app: &TestApp) -> String {
    app.post_form(
        "/registration",
        &[
            ("email", "ada@example.com"),
            ("password", MALLORYS_PASSWORD),
        ],
    )
    .await
    .assert_redirect_to("/registration/sent");
    app.settle().await;
    let mail = app
        .last_email_to("ada@example.com")
        .expect("a verification email");
    let link = app.link_in(&mail, "/email-verification/");
    app.clear_cookies(); // Ada's browser from here on
    link
}

async fn assert_mallory_is_out(app: &TestApp, verification_link: &str) {
    let ada = user::find_by_email(app.db(), "ada@example.com")
        .await
        .unwrap()
        .unwrap();
    assert!(ada.is_verified());
    assert!(
        password_credential::changed_at(app.db(), ada.id)
            .await
            .unwrap()
            .is_none(),
        "the password set before verification is gone"
    );
    assert!(
        user_token::Entity::find()
            .filter(user_token::Column::UserId.eq(ada.id))
            .all(app.db())
            .await
            .unwrap()
            .is_empty(),
        "Mallory's verification link is gone"
    );
    app.clear_cookies(); // Mallory's browser
    let res = app
        .post_form(
            "/session/password",
            &[
                ("email", "ada@example.com"),
                ("password", MALLORYS_PASSWORD),
            ],
        )
        .await;
    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!signed_in(app), "Mallory's password no longer signs in");
    app.post_form(verification_link, &[("password", MALLORYS_PASSWORD)])
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(!signed_in(app));
}

#[tokio::test]
async fn a_password_set_before_verification_dies_when_the_owner_signs_in_by_link() {
    let app = TestApp::new().await;
    let verification = mallory_registers_adas_address(&app).await;
    let mail = mail_for(&app, "ada@example.com").await;
    app.post_form(&app.link_in(&mail, "/login_links/"), &[])
        .await
        .assert_redirect_to("/account");
    assert!(signed_in(&app), "Ada is in");
    assert_mallory_is_out(&app, &verification).await;
}

#[tokio::test]
async fn a_password_set_before_verification_dies_when_the_owner_signs_in_by_code() {
    let app = TestApp::new().await;
    let verification = mallory_registers_adas_address(&app).await;
    let mail = mail_for(&app, "ada@example.com").await;
    app.post_form("/login_links/code", &[("code", &code_in(&mail))])
        .await
        .assert_redirect_to("/account");
    assert!(signed_in(&app), "Ada is in");
    assert_mallory_is_out(&app, &verification).await;
}

#[tokio::test]
async fn a_verified_accounts_password_survives_a_magic_link_sign_in() {
    let app = TestApp::new().await;
    let ada = crate::factories::user_with_password(app.db(), "ada@example.com").await;
    let mail = mail_for(&app, "ada@example.com").await;
    app.post_form(&app.link_in(&mail, "/login_links/"), &[])
        .await
        .assert_redirect_to("/account");
    assert!(
        password_credential::changed_at(app.db(), ada.id)
            .await
            .unwrap()
            .is_some(),
        "only an unverified account is claimed"
    );
}
