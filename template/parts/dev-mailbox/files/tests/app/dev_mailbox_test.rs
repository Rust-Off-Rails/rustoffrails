//! The dev mailbox: every screen, the live inbox, the agents' JSON and CLI, and the security
//! controls in samples/dev-mailbox/CHECKS.md (each row names its test here).

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use offrails_host::config::{AppEnv, Config, MailConfig};
use offrails_host::kernel::mailbox;
use offrails_host::kernel::mailbox_cli::{self, MailboxCommand};
use offrails_host::kernel::mailbox_web::{self, checks};
use offrails_host::kernel::mailer::{Attachment, Email, Mailer};
use offrails_host::routes::{Access, ROUTES};
use tower::ServiceExt;

use crate::support::TestApp;

async fn dev_app() -> TestApp {
    TestApp::with_config(Config {
        env: AppEnv::Development,
        ..Config::test()
    })
    .await
}

/// Delivers through the development mailer, exactly as the app would, and returns the new id.
async fn deliver(app: &TestApp, email: Email) -> i64 {
    assert!(
        app.state.mailer.is_local(),
        "development delivers to the mailbox"
    );
    app.state.mailer.deliver(email).await.unwrap();
    mailbox::newest_id(app.db()).await.unwrap()
}

fn welcome(to: &str) -> Email {
    Email::new(
        to,
        "Welcome aboard",
        r#"<!doctype html><html><head><title>Hi</title></head><body><p>Hi! <a href="http://localhost:3000/session/abc">Sign in</a></p><img src="http://localhost:3000/logo.png" alt="Logo"></body></html>"#,
        "Hi!\n\nSign in: http://localhost:3000/session/abc\n\nYour code is 482913.\n",
    )
}

fn hostile() -> Email {
    Email::new(
        "eve@example.com",
        "<img src=x onerror=alert(1)> subject",
        r#"<html><head></head><body><script>parent.document.body.innerHTML='pwned'</script><p onclick="alert(2)">hi</p></body></html>"#,
        "<script>alert(3)</script>",
    )
}

// ---- styling: the page ships its own CSS and JS -----------------------------------------------

/// The bug this sample fixes: an app put the mailbox CSS in app.css while kernel pages never load
/// app.css, so the mailbox rendered unstyled.
#[tokio::test]
async fn every_mailbox_page_links_its_own_stylesheet_and_script() {
    let app = dev_app().await;
    let empty = app.get("/dev/mailbox").await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    for page in [
        empty,
        app.get("/dev/mailbox").await,
        app.get(&format!("/dev/mailbox/{id}?tab=raw")).await,
        app.get("/dev/mailbox/previews").await,
        app.get("/dev/mailbox/previews/test_mailer/test_email")
            .await,
    ] {
        page.assert_ok()
            .assert_contains(r#"<link rel="stylesheet" href="/static/kernel/admin.css?v="#)
            .assert_contains(r#"<link rel="stylesheet" href="/static/kernel/mailbox.css?v="#)
            .assert_contains(r#"<script src="/static/kernel/mailbox.js?v="#)
            .assert_not_contains("/static/app.css");
    }
    let css = app.get("/static/kernel/mailbox.css").await;
    css.assert_ok();
    assert!(
        css.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );
    app.get("/static/kernel/mailbox.js").await.assert_ok();
}

/// The tool follows the kernel header's theme switch; the email's own Dark view doesn't: it
/// stays a property of the preview, whatever the tool's theme.
#[tokio::test]
async fn the_mailbox_follows_the_theme_switch_and_the_dark_preview_stays_the_emails() {
    let app = dev_app().await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    app.set_cookie("offrails_theme", "light");
    app.get(&format!("/dev/mailbox/{id}?view=dark"))
        .await
        .assert_ok()
        .assert_contains(r#"<html lang="en" data-theme="light">"#)
        .assert_contains("mbx-stage mbx-stage-dark")
        .assert_contains("/html?scheme=dark");
    app.set_cookie("offrails_theme", "dark");
    app.get(&format!("/dev/mailbox/{id}"))
        .await
        .assert_contains(r#"<html lang="en" data-theme="dark">"#)
        .assert_contains("mbx-stage mbx-stage-desktop")
        .assert_contains("/html?scheme=light");
    let css = std::fs::read_to_string("static/kernel/mailbox.css").unwrap();
    assert!(
        !css.contains("@media (prefers-color-scheme"),
        "a color-scheme media query ignores the switch: use light-dark() tokens"
    );
    assert!(css.contains("--mbx-stage: light-dark("));
}

/// Every class a mailbox template uses has a rule in the stylesheets the page loads.
#[test]
fn every_mailbox_class_has_a_rule() {
    use offrails_host::kernel::css::{has_selector, strip_askama};
    let css = [
        std::fs::read_to_string("static/kernel/admin.css").unwrap(),
        std::fs::read_to_string("static/kernel/mailbox.css").unwrap(),
    ]
    .concat();
    // Hooks for scripts and state classes set by mailbox.js, never styles of their own.
    let hooks = ["has-message"];
    let mut missing = Vec::new();
    for name in ["index.html", "_list.html", "_message.html"] {
        let path = format!("templates/kernel/mailbox/{name}");
        let src = std::fs::read_to_string(&path).unwrap();
        for chunk in src.split("class=\"").skip(1) {
            let value = chunk.split('"').next().unwrap_or_default();
            for token in strip_askama(value).split_whitespace() {
                // `mbx-stage-{{ view }}` and the like: the variants are checked below.
                if token.ends_with('-') || hooks.contains(&token) {
                    continue;
                }
                if !has_selector(&css, token) {
                    missing.push(format!("{path}: {token}"));
                }
            }
        }
    }
    for variant in [
        "mbx-stage-600",
        "mbx-stage-320",
        "mbx-stage-dark",
        "mbx-check-error",
        "mbx-check-warn",
        "mbx-check-info",
    ] {
        if !has_selector(&css, variant) {
            missing.push(format!("variant: {variant}"));
        }
    }
    assert!(
        missing.is_empty(),
        "classes with no rule:\n{}",
        missing.join("\n")
    );
}

// ---- development only ------------------------------------------------------------------------

#[tokio::test]
async fn mailbox_routes_do_not_exist_in_production_or_test() {
    for config in [
        Config {
            env: AppEnv::Production,
            app_url: "https://app.example.com".into(),
            ..Config::test()
        },
        Config::test(),
    ] {
        let app = TestApp::with_config(config).await;
        let rows: Vec<_> = ROUTES
            .iter()
            .filter(|(_, p, _, _)| p.starts_with("/dev/mailbox") || *p == "/dev/mailers")
            .collect();
        assert!(rows.len() >= 15, "every mailbox route is in ROUTES");
        for (method, path, _, access) in rows {
            assert_eq!(*access, Access::Dev, "{path}");
            let path = crate::security_test::concrete(path);
            let status = match *method {
                "GET" => app.request_head(&path, &[]).await.0,
                _ => app.post_form(&path, &[]).await.status,
            };
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        }
    }
}

#[tokio::test]
async fn mail_in_production_never_reaches_the_mailbox_table() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Production,
        app_url: "https://app.example.com".into(),
        ..Config::test()
    })
    .await;
    assert!(!app.state.mailer.is_local());
    app.state
        .mailer
        .deliver(welcome("ada@example.com"))
        .await
        .unwrap();
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (0, 0));
}

// ---- mail can't run code or reach the page ---------------------------------------------------

#[tokio::test]
async fn the_html_body_is_served_alone_in_a_sandbox() {
    let app = dev_app().await;
    let id = deliver(&app, hostile()).await;
    let body = app.get(&format!("/dev/mailbox/{id}/html")).await;
    body.assert_ok();
    let csp = body.headers["content-security-policy"].to_str().unwrap();
    assert!(
        csp.starts_with("sandbox allow-popups allow-popups-to-escape-sandbox;"),
        "{csp}"
    );
    assert!(
        !csp.contains("allow-scripts") && !csp.contains("allow-same-origin"),
        "{csp}"
    );
    assert!(
        csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'self'"),
        "{csp}"
    );
    assert!(
        !csp.contains("script-src"),
        "no script source is ever allowed: {csp}"
    );
    assert_eq!(body.headers["x-frame-options"], "SAMEORIGIN");
    assert_eq!(body.headers["referrer-policy"], "no-referrer");
    assert_eq!(body.headers["cache-control"], "no-store");

    let page = app.get(&format!("/dev/mailbox/{id}")).await;
    let iframe = page
        .body
        .split("<iframe")
        .nth(1)
        .and_then(|s| s.split('>').next())
        .expect("the HTML tab shows an iframe")
        .to_owned();
    assert!(
        iframe.contains(r#"sandbox="allow-popups allow-popups-to-escape-sandbox""#),
        "{iframe}"
    );
    assert!(!iframe.contains("allow-scripts") && !iframe.contains("allow-same-origin"));
    assert!(iframe.contains(&format!(r#"src="/dev/mailbox/{id}/html?scheme=light""#)));
}

#[tokio::test]
async fn markup_in_mail_is_escaped_on_every_mailbox_page() {
    let app = dev_app().await;
    let id = deliver(&app, hostile()).await;
    for path in [
        "/dev/mailbox".to_owned(),
        format!("/dev/mailbox/{id}?tab=text"),
        format!("/dev/mailbox/{id}?tab=raw"),
        format!("/dev/mailbox/{id}?tab=headers"),
        "/dev/mailbox/list".to_owned(),
    ] {
        let page = app.get(&path).await;
        page.assert_ok()
            .assert_not_contains("<img src=x")
            .assert_not_contains("<script>alert(3)")
            .assert_not_contains("<script>parent.");
    }
    app.get(&format!("/dev/mailbox/{id}"))
        .await
        .assert_contains("&#60;img src=x onerror=alert(1)&#62; subject");
}

#[tokio::test]
async fn attachments_are_downloads_never_documents_on_this_origin() {
    let app = dev_app().await;
    let email = welcome("ada@example.com")
        .attach(Attachment::new(
            "page.html",
            "text/html",
            b"<script>alert(1)</script>",
        ))
        .attach(Attachment::new(
            "logo.svg",
            "image/svg+xml",
            b"<svg onload=alert(1)/>",
        ))
        .attach(Attachment::new("photo.png", "image/png", b"\x89PNG fake"))
        .attach(Attachment::new(
            "report \"Q3\".pdf",
            "application/pdf",
            b"%PDF-1.4",
        ));
    let id = deliver(&app, email).await;

    let html = app.get(&format!("/dev/mailbox/{id}/attachments/0")).await;
    html.assert_ok();
    assert_eq!(
        html.headers["content-disposition"],
        r#"attachment; filename="page.html""#
    );
    assert!(
        html.headers["content-security-policy"]
            .to_str()
            .unwrap()
            .starts_with("sandbox")
    );
    assert_eq!(html.headers["x-content-type-options"], "nosniff");
    let pdf = app.get(&format!("/dev/mailbox/{id}/attachments/3")).await;
    assert_eq!(
        pdf.headers["content-disposition"],
        r#"attachment; filename="report _Q3_.pdf""#
    );
    assert_eq!(pdf.headers["content-type"], "application/pdf");
    app.get(&format!("/dev/mailbox/{id}/attachments/9"))
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // Raster images get a thumbnail; SVG (which can carry script) never does.
    let page = app.get(&format!("/dev/mailbox/{id}")).await;
    page.assert_contains(&format!(
        r#"<img class="mbx-thumb" src="/dev/mailbox/{id}/attachments/2""#
    ))
    .assert_not_contains(&format!(r#"src="/dev/mailbox/{id}/attachments/1""#))
    .assert_contains("photo.png")
    .assert_contains("report &#34;Q3&#34;.pdf");
}

// ---- the viewer --------------------------------------------------------------------------------

#[tokio::test]
async fn opening_a_message_marks_it_read_and_shows_its_parts() {
    let app = dev_app().await;
    let first = deliver(&app, welcome("grace@example.com")).await;
    let id = deliver(
        &app,
        welcome("ada@example.com")
            .cc("grace@example.com")
            .bcc("audit@example.com")
            .reply_to("support@example.com"),
    )
    .await;
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (2, 2));

    // The inbox opens the newest and marks only it read.
    let inbox = app.get("/dev/mailbox").await;
    inbox
        .assert_ok()
        .assert_contains(&format!(r#"href="/dev/mailbox/{id}" aria-current="true""#))
        .assert_contains("Welcome aboard");
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (2, 1));

    let page = app.get(&format!("/dev/mailbox/{first}")).await;
    page.assert_ok().assert_contains("grace@example.com");
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (2, 0));

    let page = app.get(&format!("/dev/mailbox/{id}")).await;
    page.assert_contains("<dt>Cc</dt><dd>grace@example.com</dd>")
        .assert_contains("<dt>Bcc</dt><dd>audit@example.com</dd>")
        .assert_contains("<dt>Reply-To</dt><dd>support@example.com</dd>");
    app.get(&format!("/dev/mailbox/{id}?tab=text"))
        .await
        .assert_contains("Sign in: http://localhost:3000/session/abc");
    app.get(&format!("/dev/mailbox/{id}?tab=raw"))
        .await
        .assert_contains("Subject: Welcome aboard")
        .assert_contains("multipart/alternative");
    app.get(&format!("/dev/mailbox/{id}?tab=headers"))
        .await
        .assert_contains(r#"<th scope="row" class="mono">Message-ID</th>"#);
    for (view, class) in [
        ("600", "mbx-stage-600"),
        ("320", "mbx-stage-320"),
        ("dark", "mbx-stage-dark"),
        ("nonsense", "mbx-stage-desktop"),
    ] {
        app.get(&format!("/dev/mailbox/{id}?view={view}"))
            .await
            .assert_contains(&format!(r#"class="mbx-stage {class}""#));
    }
}

#[tokio::test]
async fn codes_and_links_are_extracted_with_copy_buttons() {
    let app = dev_app().await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    app.get(&format!("/dev/mailbox/{id}"))
        .await
        .assert_contains(r#"<output class="mbx-code mono">482913</output>"#)
        .assert_contains(r#"data-copy="482913""#)
        .assert_contains(r#"data-copy="http://localhost:3000/session/abc""#)
        .assert_contains(r#"rel="noopener noreferrer""#);
}

#[tokio::test]
async fn dates_and_years_in_a_message_are_not_shown_as_codes() {
    // A "post published" email once showed "CODE 2026" (a year mistaken for a sign-in code).
    let app = dev_app().await;
    let id = deliver(
        &app,
        Email::new(
            "ada@example.com",
            "Your post is live",
            "<p>Your post <em>Writing code in 2026</em> is live.</p>",
            "Your post \"Writing code in 2026\" is live.\n\nPublished on 18 October 2026 at 10:30 (2026-10-18).\n\n2026\n\nRead it: http://localhost:3000/posts/42\n",
        ),
    )
    .await;
    let page = app.get(&format!("/dev/mailbox/{id}")).await;
    page.assert_ok()
        .assert_not_contains("mbx-codes")
        .assert_not_contains(r#"data-copy="2026""#)
        .assert_contains(r#"data-copy="http://localhost:3000/posts/42""#);
    let m = mailbox::find(app.db(), id).await.unwrap().unwrap();
    assert!(mailbox::codes_in(&m.text).is_empty());
}

#[tokio::test]
async fn checks_flag_what_breaks_in_real_inboxes() {
    let flagged = checks(
        "",
        r#"<a href="/relative">x</a><img src="https://example.com/a.png"><img alt="ok" src="b"><script></script>"#,
        &[],
        "",
    );
    let levels: Vec<_> = flagged
        .iter()
        .map(|c| (c.level, c.message.as_str()))
        .collect();
    assert!(
        levels
            .iter()
            .any(|(l, m)| *l == "error" && m.contains("relative URL")),
        "{levels:?}"
    );
    assert!(
        levels
            .iter()
            .any(|(l, m)| *l == "warn" && m.contains("plain-text"))
    );
    assert!(
        levels
            .iter()
            .any(|(l, m)| *l == "warn" && m.contains("1 of 2 image"))
    );
    assert!(
        levels
            .iter()
            .any(|(l, m)| *l == "warn" && m.contains("<script>"))
    );
    assert!(
        levels
            .iter()
            .any(|(l, m)| *l == "warn" && m.contains("No subject"))
    );
    let big = "x".repeat(103 * 1024);
    assert!(
        checks("t", &big, &[], "s")
            .iter()
            .any(|c| c.message.contains("Gmail clips"))
    );
    let headers = vec![(
        "List-Unsubscribe".to_owned(),
        "<https://example.com/u>".to_owned(),
    )];
    let clean = checks(
        "Hi",
        r#"<p>Hi <a href="https://example.com">x</a></p>"#,
        &headers,
        "Hi",
    );
    assert!(
        clean.is_empty(),
        "{:?}",
        clean.iter().map(|c| &c.message).collect::<Vec<_>>()
    );

    let app = dev_app().await;
    let id = deliver(
        &app,
        Email::new("ada@example.com", "Hi", r#"<a href="/x">x</a>"#, ""),
    )
    .await;
    app.get(&format!("/dev/mailbox/{id}"))
        .await
        .assert_contains(r#"<details class="mbx-checks" open>"#)
        .assert_contains("relative URL");
}

#[tokio::test]
async fn search_narrows_the_list_and_keeps_the_query_in_links() {
    let app = dev_app().await;
    deliver(&app, welcome("ada@example.com")).await;
    let other = deliver(
        &app,
        Email::new("grace@example.com", "Your receipt", "<p>r</p>", "r"),
    )
    .await;
    let page = app.get("/dev/mailbox?q=receipt").await;
    page.assert_ok()
        .assert_contains("Your receipt")
        .assert_not_contains("Welcome aboard")
        .assert_contains(&format!(r#"href="/dev/mailbox/{other}?q=receipt""#))
        .assert_contains(r#"value="receipt""#);
    app.get("/dev/mailbox?q=nothing-like-this")
        .await
        .assert_contains("No mail matches");
}

#[tokio::test]
async fn delete_clear_and_gone_messages_redirect_with_a_flash() {
    let app = dev_app().await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    deliver(&app, welcome("grace@example.com")).await;

    app.post_form(&format!("/dev/mailbox/{id}/delete"), &[])
        .await
        .assert_redirect_to("/dev/mailbox");
    app.get("/dev/mailbox")
        .await
        .assert_contains("Message deleted.");
    // Back to a deleted message: the inbox with a flash, never a bare 404.
    app.get(&format!("/dev/mailbox/{id}"))
        .await
        .assert_redirect_to("/dev/mailbox");
    app.get("/dev/mailbox")
        .await
        .assert_contains("That message was deleted.");
    app.get(&format!("/dev/mailbox/{id}/html"))
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.post_form("/dev/mailbox/clear", &[])
        .await
        .assert_redirect_to("/dev/mailbox");
    app.get("/dev/mailbox")
        .await
        .assert_contains("Cleared 1 message.")
        .assert_contains("No mail yet");
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (0, 0));
}

#[tokio::test]
async fn the_eml_download_is_the_exact_message() {
    let app = dev_app().await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    let eml = app.get(&format!("/dev/mailbox/{id}/message.eml")).await;
    eml.assert_ok().assert_contains("Subject: Welcome aboard");
    assert_eq!(eml.headers["content-type"], "message/rfc822");
    assert_eq!(
        eml.headers["content-disposition"],
        format!(r#"attachment; filename="message-{id}.eml""#)
    );
    let stored = mailbox::find(app.db(), id).await.unwrap().unwrap();
    assert_eq!(eml.body, stored.eml);
}

/// The test button stores straight into the mailbox, never over SMTP, whatever MAILER says.
#[tokio::test]
async fn the_test_email_button_never_sends_over_smtp() {
    let app = TestApp::with_config(Config {
        env: AppEnv::Development,
        mail: MailConfig {
            mailer: Some("smtp".into()),
            // Nothing listens here: a real send would fail.
            smtp_url: Some("smtp://127.0.0.1:9".into()),
            ..MailConfig::test()
        },
        ..Config::test()
    })
    .await;
    let res = app.post_form("/dev/mailbox/test", &[]).await;
    let id = mailbox::newest_id(app.db()).await.unwrap();
    res.assert_redirect_to(&format!("/dev/mailbox/{id}"));
    let m = mailbox::find(app.db(), id).await.unwrap().unwrap();
    assert_eq!(m.subject, "Test email: the dev mailbox works");
    assert_eq!(mailbox::codes_in(&m.text).len(), 1);
    app.get(&format!("/dev/mailbox/{id}"))
        .await
        .assert_contains("Sent a test email to your mailbox.")
        .assert_contains("hello.txt");
}

// ---- previews --------------------------------------------------------------------------------

#[tokio::test]
async fn previews_render_in_the_same_viewer_and_are_never_stored() {
    let app = dev_app().await;
    app.get("/dev/mailbox/previews")
        .await
        .assert_ok()
        .assert_contains("test_mailer/test_email")
        .assert_contains("rendered now from sample data, not sent");
    let page = app
        .get("/dev/mailbox/previews/test_mailer/test_email?tab=raw")
        .await;
    page.assert_ok().assert_contains("Subject: Test email from");
    let body = app
        .get("/dev/mailbox/previews/test_mailer/test_email/html")
        .await;
    body.assert_ok().assert_contains("Email works.");
    assert!(
        body.headers["content-security-policy"]
            .to_str()
            .unwrap()
            .starts_with("sandbox")
    );
    app.get("/dev/mailbox/previews/nope/nothing")
        .await
        .assert_redirect_to("/dev/mailbox/previews");
    app.get("/dev/mailbox/previews/nope/nothing/html")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (0, 0));
    let old = app.request_head("/dev/mailers", &[]).await;
    assert_eq!(old.0, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(old.1["location"], "/dev/mailbox/previews");
}

// ---- for agents and tests ----------------------------------------------------------------------

#[tokio::test]
async fn the_json_endpoints_filter_and_carry_links_and_codes() {
    let app = dev_app().await;
    let missing = app.get("/dev/mailbox/latest.json").await;
    missing.assert_status(StatusCode::NOT_FOUND);
    insta::assert_snapshot!(missing.body, @r#"{"error":"no messages"}"#);

    let first = deliver(&app, welcome("ada@example.com")).await;
    deliver(
        &app,
        Email::new("grace@example.com", "Your receipt", "<p>r</p>", "r"),
    )
    .await;

    let latest: serde_json::Value = app
        .get("/dev/mailbox/latest.json?to=ADA@example.com")
        .await
        .json();
    assert_eq!(latest["id"], first);
    assert_eq!(latest["links"][0], "http://localhost:3000/session/abc");
    assert_eq!(latest["codes"][0], "482913");

    let all: Vec<serde_json::Value> = app.get("/dev/mailbox.json").await.json();
    assert_eq!(all.len(), 2);
    let to: Vec<serde_json::Value> = app
        .get("/dev/mailbox.json?to=grace@example.com")
        .await
        .json();
    assert_eq!(to.len(), 1);
    let subject: Vec<serde_json::Value> = app.get("/dev/mailbox.json?subject=RECEIPT").await.json();
    assert_eq!(subject[0]["subject"], "Your receipt");
    let since: Vec<serde_json::Value> = app
        .get(&format!("/dev/mailbox.json?since={first}"))
        .await
        .json();
    assert_eq!(since.len(), 1);
}

#[tokio::test]
async fn the_cli_lists_shows_and_clears() {
    let app = dev_app().await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    mailbox_cli::run(app.db(), MailboxCommand::List { to: None, limit: 5 })
        .await
        .unwrap();
    mailbox_cli::run(app.db(), MailboxCommand::Show { id: Some(id) })
        .await
        .unwrap();
    assert!(
        mailbox_cli::run(app.db(), MailboxCommand::Show { id: Some(id + 1) })
            .await
            .is_err()
    );
    mailbox_cli::run(app.db(), MailboxCommand::Clear)
        .await
        .unwrap();
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (0, 0));
}

// ---- live ------------------------------------------------------------------------------------

/// Reads SSE frames until one contains `needle`, or fails after `wait`.
async fn next_event(body: &mut Body, needle: &str, wait: Duration) -> String {
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = tokio::time::timeout(left, body.frame())
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} event; saw: {seen}"));
        let Some(Ok(frame)) = frame else {
            panic!("the stream ended; saw: {seen}")
        };
        if let Some(data) = frame.data_ref() {
            seen.push_str(&String::from_utf8_lossy(data));
            if let Some(at) = seen.find(needle) {
                return seen[at..].to_owned();
            }
        }
    }
}

#[tokio::test]
async fn the_inbox_is_live_over_server_sent_events() {
    let app = dev_app().await;
    let router = offrails_host::routes::router(app.state.clone()).unwrap();
    let res = router
        .oneshot(
            Request::get("/dev/mailbox/events?since=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    assert_eq!(res.headers()["cache-control"], "no-store");
    let mut body = res.into_body();
    // Connected: a comment, so the browser knows the stream is up.
    next_event(&mut body, ": live", Duration::from_secs(5)).await;

    let id = deliver(&app, welcome("ada@example.com")).await;
    let event = next_event(&mut body, "event: mailbox", Duration::from_secs(5)).await;
    assert!(event.contains(&format!("id: {id}")), "{event}");
    assert!(event.contains(&format!(r#""newest":{id}"#)), "{event}");
    assert!(event.contains(r#""unread":1"#), "{event}");
}

#[tokio::test]
async fn a_reconnect_resumes_from_the_last_event_id() {
    let app = dev_app().await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    let router = offrails_host::routes::router(app.state.clone()).unwrap();
    // A tab that saw nothing (since=0) hears about the message at once…
    let res = router
        .clone()
        .oneshot(
            Request::get("/dev/mailbox/events?since=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = res.into_body();
    next_event(
        &mut body,
        &format!(r#""newest":{id}"#),
        Duration::from_secs(5),
    )
    .await;
    // …and one resuming from that id hears nothing new.
    let res = router
        .oneshot(
            Request::get("/dev/mailbox/events?since=0")
                .header("last-event-id", id.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = res.into_body();
    next_event(&mut body, ": live", Duration::from_secs(5)).await;
}

#[tokio::test]
async fn the_list_fragment_reports_the_counts_for_the_live_inbox() {
    let app = dev_app().await;
    let id = deliver(&app, welcome("ada@example.com")).await;
    let res = app.get(&format!("/dev/mailbox/list?selected={id}")).await;
    res.assert_ok()
        .assert_contains(&format!(r#"data-id="{id}""#))
        .assert_not_contains("<html");
    assert_eq!(res.headers["x-mailbox-total"], "1");
    assert_eq!(res.headers["x-mailbox-unread"], "1");
}

// ---- the toolbox around it ---------------------------------------------------------------------

#[tokio::test]
async fn dev_index_and_introspection_show_the_mailbox() {
    let app = dev_app().await;
    deliver(&app, welcome("ada@example.com")).await;
    app.get("/dev")
        .await
        .assert_contains("1 unread of 1")
        .assert_contains(r#"action="/dev/mailbox/test""#)
        .assert_contains(r#"href="/dev/mailbox""#);
    let v: serde_json::Value = app.get("/dev/introspect.json").await.json();
    assert_eq!(v["mailbox"]["total"], 1);
    app.get("/")
        .await
        .assert_contains(r#"<a href="/dev/mailbox" hx-boost="false" class="link">Mailbox</a>"#);
}

#[tokio::test]
async fn every_mailbox_response_is_no_store() {
    let app = dev_app().await;
    let id = deliver(
        &app,
        welcome("ada@example.com").attach(Attachment::new("a.txt", "text/plain", b"a")),
    )
    .await;
    for path in [
        format!("/dev/mailbox/{id}"),
        format!("/dev/mailbox/{id}/html"),
        format!("/dev/mailbox/{id}/message.eml"),
        format!("/dev/mailbox/{id}/attachments/0"),
        "/dev/mailbox/list".to_owned(),
        "/dev/mailbox/latest.json".to_owned(),
        "/dev/mailbox/previews/test_mailer/test_email/html".to_owned(),
    ] {
        let (_, headers) = app.request_head(&path, &[]).await;
        assert_eq!(headers["cache-control"], "no-store", "{path}");
    }
    let res = app.post_form("/dev/mailbox/clear", &[]).await;
    assert_eq!(res.headers["cache-control"], "no-store", "a redirect");
}

#[test]
fn the_sample_email_validates() {
    let email = mailbox_web::sample_email("http://localhost:3000", "000123");
    offrails_host::kernel::mailer::validate(&email).unwrap();
    let _ = Mailer::memory();
}
