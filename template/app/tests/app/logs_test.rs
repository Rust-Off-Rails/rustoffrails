//! What reaches the logs: request URIs go through `kernel::redact`, so a token in an emailed link
//! (`/login_links/<token>`, reset and verification links) or in a query never lands in a log line,
//! at any level.

use offrails_app::kernel::redact::{REDACTED, TOKEN_ROUTES, log_filter};

use crate::support::TestApp;

/// Every log line the app's filter lets through at `trace` (tower_http's request span included),
/// written to a temporary file while it lives.
struct LogCapture {
    file: tempfile::NamedTempFile,
    _guard: tracing::subscriber::DefaultGuard,
}

impl LogCapture {
    fn start() -> Self {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let sink = file.reopen().expect("reopen");
        let subscriber = tracing_subscriber::fmt()
            // The app's own filter at its loudest: RUST_LOG=trace, CSRF tracing asked for too.
            .with_env_filter(log_filter("trace,tower_http::csrf=trace"))
            .with_ansi(false)
            .with_writer(move || sink.try_clone().expect("clone log file"))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        Self {
            file,
            _guard: guard,
        }
    }

    fn text(&self) -> String {
        std::fs::read_to_string(self.file.path()).unwrap_or_default()
    }
}

#[tokio::test]
async fn tokens_in_request_uris_never_reach_the_logs() {
    // Short enough that only the route list catches it; long and random for the generic rule.
    const SHORT: &str = "s3cretTok";
    const LONG: &str = "q8Zr2mX0kLwP4vN7tY1sB9cD3fG6hJ5aE2uI8oQ4rTw";
    let logs = LogCapture::start();
    let app = TestApp::new().await;

    // Every listed route (samples add theirs), with a short token in its {token} slot.
    for route in TOKEN_ROUTES {
        let path = route
            .split('/')
            .map(|s| match s {
                "{token}" => SHORT,
                s if s.starts_with('{') => "1",
                s => s,
            })
            .collect::<Vec<_>>()
            .join("/");
        app.get(&path).await;
    }
    // Any route, listed or not: a long random segment, and secrets in the query.
    app.get(&format!("/no/such/link/{LONG}")).await;
    app.get(&format!("/?token={SHORT}&page=2")).await;

    let text = logs.text();
    assert!(
        text.contains("started processing request") && text.contains(REDACTED),
        "the capture must see the request span:\n{text}"
    );
    assert!(
        text.contains("page=2"),
        "the rest of the URI stays readable"
    );
    for secret in [SHORT, LONG] {
        assert!(!text.contains(secret), "{secret} reached the logs:\n{text}");
    }
}
