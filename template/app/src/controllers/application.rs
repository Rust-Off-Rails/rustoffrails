//! Shared extractors (`ApplicationController`): layout context, client IP, `HX-Request`, paging.
//! Authentication, when a sample adds it, is an extractor on each handler, never middleware, so a
//! route's protection is visible in its signature.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts, Query};
use axum::http::request::Parts;

use crate::error::AppError;
use crate::kernel::css::css_warning;
use crate::kernel::flash::RequestFlash;
use crate::kernel::theme::Theme;
use crate::models::Page;
use crate::state::AppState;
use crate::views::{Ctx, DEFAULT_DESCRIPTION};

impl FromRequestParts<AppState> for Ctx {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let flash = parts
            .extensions
            .get::<RequestFlash>()
            .and_then(|f| f.0.clone());
        let development = state.config.is_development();
        Ok(Self {
            flash,
            title: String::new(),
            description: DEFAULT_DESCRIPTION.to_owned(),
            path: parts.uri.path().to_owned(),
            app_url: state.config.app_url.trim_end_matches('/').to_owned(),
            noindex: false,
            assets: state.assets.assets(),
            dev: development,
            // Missing, empty or stale static/app.css: a plain-text banner first in <main>.
            css_warning: if development { css_warning() } else { None },
            theme: Theme::from_headers(&parts.headers),
            // samples: ctx init
        })
    }
}

/// The caller's IP: the socket address, or the **rightmost** `X-Forwarded-For` hop when
/// `APP_TRUSTED_PROXY=true` (behind Fly, Kamal-proxy, Render and similar). `None` in tests,
/// which run without a socket.
pub struct ClientIp(pub Option<IpAddr>);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let forwarded = state
            .config
            .trusted_proxy
            .then(|| {
                parts
                    .headers
                    .get("x-forwarded-for")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.rsplit(',').next())
                    .and_then(|hop| hop.trim().parse::<IpAddr>().ok())
            })
            .flatten();
        let ip = forwarded.or_else(|| {
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip())
        });
        Ok(Self(ip))
    }
}

impl ClientIp {
    /// `"{route}:{ip}"`, or `"{route}:unknown"` without a socket (every test request shares it,
    /// so one `TestApp` can exhaust a limit in a loop).
    pub fn key(&self, route: &str) -> String {
        match self.0 {
            Some(ip) => format!("{route}:{ip}"),
            None => format!("{route}:unknown"),
        }
    }
}

/// `true` when the request came from htmx (`HX-Request: true`).
pub struct HxRequest(pub bool);

impl<S: Send + Sync> FromRequestParts<S> for HxRequest {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .headers
                .get("hx-request")
                .is_some_and(|v| v.as_bytes() == b"true"),
        ))
    }
}

impl FromRequestParts<AppState> for Page {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Query(page) = Query::<Page>::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Invalid(crate::models::Errors::single("page", "is invalid")))?;
        Ok(page)
    }
}

// samples: extractors
