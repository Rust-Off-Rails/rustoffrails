//! `flash[:notice]`: a one-shot message carried in the `_flash` cookie.
//! The value is URL-encoded JSON and is only ever rendered escaped, so it needs no signature.

use std::convert::Infallible;

use axum::{
    extract::Request,
    http::header::SET_COOKIE,
    middleware::Next,
    response::{IntoResponseParts, Response, ResponseParts},
};
use serde::{Deserialize, Serialize};

use super::cookie;

const COOKIE_NAME: &str = "_flash";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashKind {
    Notice,
    Alert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashMessage {
    pub kind: FlashKind,
    pub message: String,
}

impl FlashMessage {
    pub fn is_alert(&self) -> bool {
        self.kind == FlashKind::Alert
    }
}

/// Request extension holding the flash that arrived with this request, if any.
#[derive(Debug, Clone, Default)]
pub struct RequestFlash(pub Option<FlashMessage>);

/// Return it in a response tuple to set the flash for the next request.
pub struct Flash(FlashMessage);

impl Flash {
    pub fn notice(message: impl Into<String>) -> Self {
        Self(FlashMessage {
            kind: FlashKind::Notice,
            message: message.into(),
        })
    }

    pub fn alert(message: impl Into<String>) -> Self {
        Self(FlashMessage {
            kind: FlashKind::Alert,
            message: message.into(),
        })
    }
}

impl IntoResponseParts for Flash {
    type Error = Infallible;

    fn into_response_parts(self, mut res: ResponseParts) -> Result<ResponseParts, Self::Error> {
        if let Ok(json) = serde_json::to_string(&self.0) {
            let value = cookie::set(COOKIE_NAME, &cookie::encode(&json), None, false);
            res.headers_mut().append(SET_COOKIE, value);
        }
        Ok(res)
    }
}

/// Read-once semantics: parse the cookie into request extensions, and expire it on the way out
/// unless the handler set a new one.
pub async fn middleware(mut req: Request, next: Next) -> Response {
    let incoming = cookie::get(req.headers(), COOKIE_NAME);
    let message = incoming
        .as_deref()
        .and_then(cookie::decode)
        .and_then(|json| serde_json::from_str::<FlashMessage>(&json).ok());
    req.extensions_mut().insert(RequestFlash(message));

    let mut response = next.run(req).await;
    if incoming.is_some() {
        let sets_new_flash = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .any(|v| v.to_str().is_ok_and(|s| s.starts_with("_flash=")));
        if !sets_new_flash {
            response
                .headers_mut()
                .append(SET_COOKIE, cookie::clear(COOKIE_NAME, false));
        }
    }
    response
}
