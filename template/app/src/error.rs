//! `AppError` and the one place error responses are shaped (`rescue_from` + the exceptions app).
//!
//! 1. `impl IntoResponse for AppError` logs 5xx once and returns a bare status response carrying
//!    an `ErrorReport` in its extensions.
//! 2. `shape_errors` (middleware) classifies the request, runs it, and rewrites any response
//!    carrying a report into the page for the request flavor.

use axum::{
    extract::{Request, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sea_orm::{ConnAcquireErr, DbErr};

use crate::{
    config::AppEnv,
    kernel::theme::Theme,
    models::Errors,
    routes::paths,
    state::AppState,
    views::{self, render},
};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    /// Not logged in.
    #[error("unauthorized")]
    Unauthorized,
    /// Logged in, not allowed.
    #[error("forbidden")]
    Forbidden,
    /// Validation failed.
    #[error("invalid")]
    Invalid(Errors),
    #[error("too many requests")]
    TooManyRequests,
    /// Overloaded: no database connection within the pool's acquire timeout. 503 + Retry-After.
    #[error("unavailable")]
    Unavailable,
    #[error(transparent)]
    Db(DbErr),
    #[error(transparent)]
    Template(#[from] askama::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<DbErr> for AppError {
    fn from(e: DbErr) -> Self {
        match e {
            DbErr::RecordNotFound(_) => AppError::NotFound,
            // The pool had no free connection within acquire_timeout (5 s): overloaded, not broken.
            DbErr::ConnectionAcquire(ConnAcquireErr::Timeout) => AppError::Unavailable,
            other => AppError::Db(other),
        }
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(e.into())
    }
}

/// How the request arrived. Set once per request by `shape_errors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFlavor {
    Html,
    Htmx,
    /// `/api/…` and event streams: errors are JSON (`{"error": "not_found"}`).
    Api,
}

/// Carried from `IntoResponse` to `shape_errors` as a response extension.
#[derive(Debug, Clone)]
pub struct ErrorReport {
    pub status: StatusCode,
    pub errors: Option<Errors>,
    /// The error chain. Only ever displayed in development.
    pub detail: Option<String>,
}

pub const ROUTE_FALLBACK: HeaderName = HeaderName::from_static("x-route-fallback");

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let mut report = ErrorReport {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            errors: None,
            detail: None,
        };
        match &self {
            AppError::NotFound => report.status = StatusCode::NOT_FOUND,
            AppError::Unauthorized => report.status = StatusCode::UNAUTHORIZED,
            AppError::Forbidden => report.status = StatusCode::FORBIDDEN,
            AppError::Invalid(errors) => {
                report.status = StatusCode::UNPROCESSABLE_ENTITY;
                report.errors = Some(errors.clone());
            }
            AppError::TooManyRequests => report.status = StatusCode::TOO_MANY_REQUESTS,
            AppError::Unavailable => report.status = StatusCode::SERVICE_UNAVAILABLE,
            AppError::Db(_) | AppError::Template(_) | AppError::Internal(_) => {
                // The one place a 500 is logged, with the full chain.
                let chain = format!("{self:#}");
                tracing::error!(error = %chain, "request failed");
                report.detail = Some(chain);
            }
        }
        let mut response = (
            report.status,
            report.status.canonical_reason().unwrap_or("error"),
        )
            .into_response();
        response.extensions_mut().insert(report);
        response
    }
}

/// What `CatchPanicLayer` calls when a handler panics: logs the panic message (with the request
/// span's `request_id`) and answers like any other 500. The layer sits inside `shape_errors`, so
/// the standalone `errors/500.html` is rendered there, for htmx and full-page requests alike.
pub fn panic_response(payload: Box<dyn std::any::Any + Send>) -> Response {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    AppError::Internal(anyhow::anyhow!("handler panicked: {message}")).into_response()
}

/// Middleware: classifies the request, then rewrites any report-carrying response into the
/// page for the request flavor. Error pages never touch the database.
pub async fn shape_errors(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let flavor = classify(&req);
    req.extensions_mut().insert(flavor);
    let theme = Theme::from_headers(req.headers());

    let mut response = next.run(req).await;
    // A handler's own 422 re-render (a form with errors) ends here too.
    keep_url_on_invalid(flavor, &mut response);
    let Some(report) = response.extensions_mut().remove::<ErrorReport>() else {
        return response;
    };
    let from_fallback = response.headers().contains_key(&ROUTE_FALLBACK);
    let development = state.config.env == AppEnv::Development;

    if flavor == ResponseFlavor::Api {
        let mut out = api_error(&report);
        if from_fallback {
            out.headers_mut()
                .insert(ROUTE_FALLBACK, HeaderValue::from_static("1"));
        }
        return retry_after(out).await;
    }
    let rendered = match report.status {
        StatusCode::UNAUTHORIZED => return unauthorized(flavor),
        StatusCode::NOT_FOUND => {
            render(views::errors::NotFound { theme }).map(|h| (StatusCode::NOT_FOUND, h))
        }
        StatusCode::FORBIDDEN => {
            render(views::errors::Forbidden { theme }).map(|h| (StatusCode::FORBIDDEN, h))
        }
        StatusCode::TOO_MANY_REQUESTS => render(views::errors::TooManyRequests { theme })
            .map(|h| (StatusCode::TOO_MANY_REQUESTS, h)),
        StatusCode::UNPROCESSABLE_ENTITY => render(views::errors::Invalid {
            messages: report
                .errors
                .as_ref()
                .map(Errors::full_messages)
                .unwrap_or_default(),
        })
        .map(|h| (StatusCode::UNPROCESSABLE_ENTITY, h)),
        StatusCode::SERVICE_UNAVAILABLE => render(views::errors::Unavailable { theme })
            .map(|h| (StatusCode::SERVICE_UNAVAILABLE, h)),
        _ => render(views::errors::ServerError {
            theme,
            detail: report.detail.filter(|_| development),
        })
        .map(|h| (StatusCode::INTERNAL_SERVER_ERROR, h)),
    };
    let mut out = match rendered {
        Ok((status, html)) => {
            let mut out = (status, html).into_response();
            if status == StatusCode::TOO_MANY_REQUESTS {
                out.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("180"));
            }
            out
        }
        // The error page itself failed to render: fall back to plain text.
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong").into_response(),
    };
    // Lets the routability test tell "no such route" from a handler's own "no such record".
    if from_fallback {
        out.headers_mut()
            .insert(ROUTE_FALLBACK, HeaderValue::from_static("1"));
    }
    keep_url_on_invalid(flavor, &mut out);
    retry_after(out).await
}

fn classify(req: &Request) -> ResponseFlavor {
    let path = req.uri().path();
    let wants_stream = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    if path == "/api" || path.starts_with("/api/") || wants_stream {
        ResponseFlavor::Api
    } else if req.headers().contains_key("hx-request") {
        ResponseFlavor::Htmx
    } else {
        ResponseFlavor::Html
    }
}

/// `{"error": "<code>"}`, or `{"errors": {field: [messages]}}` for a 422.
fn api_error(report: &ErrorReport) -> Response {
    let body = match &report.errors {
        Some(errors) => serde_json::json!({ "errors": errors.0 }),
        None => serde_json::json!({ "error": match report.status {
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::UNPROCESSABLE_ENTITY => "invalid",
            StatusCode::TOO_MANY_REQUESTS => "too_many_requests",
            StatusCode::SERVICE_UNAVAILABLE => "unavailable",
            _ => "internal",
        }}),
    };
    (report.status, axum::Json(body)).into_response()
}

/// `map_response` just outside the timeout layer, and the end of `shape_errors`: every 503
/// (the timeout layer's, an overloaded pool's) says when to come back.
pub async fn retry_after(mut response: Response) -> Response {
    if response.status() == StatusCode::SERVICE_UNAVAILABLE
        && !response.headers().contains_key(header::RETRY_AFTER)
    {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

/// Logs every 5xx with its request id, method and path, so no status a client sees goes
/// unexplained (an extractor's rejection or a layer's own response has no other log line).
pub async fn log_server_errors(req: Request, next: Next) -> Response {
    // SECURITY: the path as logs may show it, tokens redacted (test: tokens_in_request_uris_never_reach_the_logs).
    let (method, path) = (
        req.method().clone(),
        crate::kernel::redact::path(req.uri().path()),
    );
    let id = req.headers().get("x-request-id").cloned();
    let response = next.run(req).await;
    if response.status().is_server_error() {
        tracing::error!(
            status = response.status().as_u16(),
            request_id = id.as_ref().and_then(|v| v.to_str().ok()).unwrap_or("-"),
            %method,
            path,
            "server error response"
        );
    }
    response
}

/// htmx swaps a boosted form's 422 in and then pushes the **POST** URL into the address bar, so
/// reloading shows another page. `HX-Push-Url: false` (honoured by htmx 2) keeps the form's URL.
fn keep_url_on_invalid(flavor: ResponseFlavor, response: &mut Response) {
    if flavor == ResponseFlavor::Htmx && response.status() == StatusCode::UNPROCESSABLE_ENTITY {
        response
            .headers_mut()
            .insert("hx-push-url", HeaderValue::from_static("false"));
    }
}

/// Not logged in: to the sign-in page (`paths::new_session`, routed by an authentication sample).
fn unauthorized(flavor: ResponseFlavor) -> Response {
    let login = paths::new_session();
    match flavor {
        ResponseFlavor::Htmx => {
            let mut r = StatusCode::OK.into_response();
            if let Ok(v) = HeaderValue::from_str(&login) {
                r.headers_mut().insert("hx-redirect", v);
            }
            r
        }
        _ => axum::response::Redirect::to(&login).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(response: &Response) -> &ErrorReport {
        response
            .extensions()
            .get::<ErrorReport>()
            .expect("a report")
    }

    #[test]
    fn a_panic_becomes_a_500_that_names_the_message() {
        for payload in [
            Box::new("boom") as Box<dyn std::any::Any + Send>,
            Box::new(String::from("boom")),
        ] {
            let response = panic_response(payload);
            assert_eq!(report(&response).status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(
                report(&response).detail.as_deref(),
                Some("handler panicked: boom")
            );
        }
        let odd = panic_response(Box::new(42_u8));
        assert!(
            report(&odd)
                .detail
                .as_deref()
                .unwrap()
                .contains("non-string")
        );
    }
}
