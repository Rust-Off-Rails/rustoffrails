//! The live room (the realtime sample's demo): `/live/<room>` with presence, typing and messages
//! over the room's channel, and its header, refreshed on every open page when someone edits it.
//!
//! GUIDE: .offrails/guides/channels.md#the-app-side

use axum::extract::{Path, State};
use axum::http::HeaderValue;
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use chrono::Utc;

use crate::channels::identity::{self, CurrentPerson, RequirePerson, Signed};
use crate::controllers::application::{ClientIp, HxRequest};
use crate::error::AppError;
use crate::kernel::cookie;
use crate::kernel::flash::Flash;
use crate::models::guest::{self, GuestParams};
use crate::models::room::{self, HeadlineParams};
use crate::models::{Errors, guest_session, room_message};
use crate::routes::paths;
use crate::state::AppState;
use crate::views::live::{LiveHeader, LiveShow, LiveSignIn, MessageView, RoomLink};
use crate::views::{Ctx, render};

/// Messages the page renders before the socket connects.
const FIRST_PAINT: u64 = 50; // APP: how much history a page shows

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/live", get(index))
        .route("/live/{name}", get(show))
        .route("/live/{name}/header", get(header))
        .route("/live/{name}/headline", post(update_headline))
}

/// Development only: guests sign in with a display name (`identity.rs`). Under `/dev`, like every
/// development-only route; production uses the app's own accounts.
pub fn dev_router() -> Router<AppState> {
    Router::new()
        .route("/dev/guests/sign-in", get(sign_in_form).post(sign_in))
        .route("/dev/guests/sign-out", post(sign_out))
}

/// Not signed in: development has guests (unless accounts replaced them); elsewhere it's the
/// app's sign-in (401 → redirect).
fn sign_in_first(s: &AppState) -> Result<Response, AppError> {
    if s.config.is_development() && identity::DEV_GUESTS {
        Ok(Redirect::to("/dev/guests/sign-in").into_response())
    } else {
        Err(AppError::Unauthorized)
    }
}

/// `GET /live`: the lobby.
pub async fn index(
    State(s): State<AppState>,
    CurrentPerson(signed): CurrentPerson,
) -> Result<Response, AppError> {
    if signed.is_none() {
        return sign_in_first(&s);
    }
    Ok(Redirect::to("/live/lobby").into_response())
}

/// `GET /live/{name}`: the room.
pub async fn show(
    State(s): State<AppState>,
    ctx: Ctx,
    CurrentPerson(signed): CurrentPerson,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    let Some(signed) = signed else {
        return sign_in_first(&s);
    };
    // SECURITY: a members-only room is "not found" to everyone else, like a missing one (test: a_members_only_room_is_not_found_for_others).
    let room = room::find_for(&s.db, signed.person.id, &name)
        .await?
        .ok_or(AppError::NotFound)?;
    let rooms = room::all_for(&s.db, signed.person.id)
        .await?
        .into_iter()
        .map(|r| RoomLink {
            current: r.id == room.id,
            slug: r.slug,
            name: r.name,
            members_only: r.members_only,
        })
        .collect();
    let messages: Vec<MessageView> = room_message::latest(&s.db, room.id, FIRST_PAINT)
        .await?
        .iter()
        .map(MessageView::from)
        .collect();
    // INVARIANT: the page's cursor is the newest message it shows, so a message posted between this render and the socket's join is replayed (test: a_reconnect_replays_exactly_what_was_missed)
    let since = messages.last().map_or(0, |m| m.id);
    let page = LiveShow {
        ctx: ctx.titled(&room.name).noindex(),
        topic: room::topic(&room.slug),
        user_topic: format!("user:{}", signed.person.id),
        me: signed.person,
        rooms,
        messages,
        since,
        channels_js: s.assets.path("channels.js"),
        live_room_js: s.assets.path("live_room.js"),
        template_message: MessageView::blank(),
        palette: (0..8).collect(),
        dev_guest: s.config.is_development() && identity::DEV_GUESTS,
        errors: Errors::default(),
        room,
    };
    Ok(render(page)?.into_response())
}

/// `GET /live/{name}/header`: the header region alone (htmx re-fetches it on `refresh`).
pub async fn header(
    State(s): State<AppState>,
    RequirePerson(signed): RequirePerson,
    HxRequest(htmx): HxRequest,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    let room = room::find_for(&s.db, signed.person.id, &name)
        .await?
        .ok_or(AppError::NotFound)?;
    if !htmx {
        return Ok(Redirect::to(&format!("/live/{}", room.slug)).into_response());
    }
    Ok(render(LiveHeader {
        room,
        errors: Errors::default(),
    })?
    .into_response())
}

/// `POST /live/{name}/headline`: anyone in the room may change it; every open page refreshes its
/// header through `channels::refresh`.
pub async fn update_headline(
    State(s): State<AppState>,
    RequirePerson(signed): RequirePerson,
    HxRequest(htmx): HxRequest,
    ip: ClientIp,
    Path(name): Path<String>,
    Form(params): Form<HeadlineParams>,
) -> Result<Response, AppError> {
    let room = room::find_for(&s.db, signed.person.id, &name)
        .await?
        .ok_or(AppError::NotFound)?;
    s.rate_limit
        .check(
            &ip.key(&format!("live#headline:{}", signed.person.id)),
            10,
            std::time::Duration::from_secs(60),
        )
        .await?;
    let headline = match params.validate() {
        Ok(h) => h,
        Err(errors) if htmx => {
            let html = render(LiveHeader { room, errors })?;
            return Ok((axum::http::StatusCode::UNPROCESSABLE_ENTITY, html).into_response());
        }
        Err(errors) => return Err(AppError::Invalid(errors)),
    };
    let room = room::update_headline(&s.db, room, headline, Utc::now()).await?;
    // After the commit: every page in the room re-fetches its header.
    crate::channels::refresh(&s, &room::topic(&room.slug), "room-header");
    if htmx {
        return Ok(render(LiveHeader {
            room,
            errors: Errors::default(),
        })?
        .into_response());
    }
    Ok((
        Flash::notice("Headline updated."),
        Redirect::to(&format!("/live/{}", room.slug)),
    )
        .into_response())
}

// ---- development guests ---------------------------------------------------------------------

/// With accounts (identity.rs), guests don't exist: the guest pages send people to sign in.
fn no_guests() -> Response {
    Redirect::to(&paths::new_session()).into_response()
}

/// `GET /dev/guests/sign-in` (development).
pub async fn sign_in_form(
    ctx: Ctx,
    CurrentPerson(signed): CurrentPerson,
) -> Result<Response, AppError> {
    if !identity::DEV_GUESTS {
        return Ok(no_guests());
    }
    if signed.is_some() {
        return Ok(Redirect::to("/live/lobby").into_response());
    }
    Ok(render(LiveSignIn {
        ctx: ctx.titled("Join the live room").noindex(),
        name: String::new(),
        errors: Errors::default(),
    })?
    .into_response())
}

/// `POST /dev/guests/sign-in` (development): a new guest with this display name, and a session. The
/// server picks the id: the form only names the guest.
pub async fn sign_in(
    State(s): State<AppState>,
    ctx: Ctx,
    Form(params): Form<GuestParams>,
) -> Result<Response, AppError> {
    if !identity::DEV_GUESTS {
        return Ok(no_guests());
    }
    let new = match params.validate() {
        Ok(new) => new,
        Err(errors) => {
            let page = LiveSignIn {
                ctx: ctx.titled("Join the live room").noindex(),
                name: params.name,
                errors,
            };
            return Ok(
                (axum::http::StatusCode::UNPROCESSABLE_ENTITY, render(page)?).into_response(),
            );
        }
    };
    let now = Utc::now();
    let guest = guest::create(&s.db, new, now).await?;
    let (_, token) = guest_session::start(&s.db, guest.id, now).await?;
    let secure = s.config.is_production();
    let cookie = cookie::set(
        identity::cookie_name(secure),
        &token,
        Some(guest_session::IDLE.num_seconds()),
        secure,
    );
    Ok((
        AppendHeaders([(axum::http::header::SET_COOKIE, cookie)]),
        Redirect::to("/live/lobby"),
    )
        .into_response())
}

/// `POST /dev/guests/sign-out` (development): ends this browser's session; its sockets and streams
/// close with `bye revoked` on every node.
pub async fn sign_out(
    State(s): State<AppState>,
    RequirePerson(signed): RequirePerson,
) -> Result<Response, AppError> {
    // SECURITY: with accounts the session id is auth-core's; never delete a guest row by it
    // (test: with_accounts_the_guest_pages_send_people_to_sign_in).
    if !identity::DEV_GUESTS {
        return Ok(no_guests());
    }
    end_session(&s, &signed).await?;
    let secure = s.config.is_production();
    let clear: HeaderValue = cookie::clear(identity::cookie_name(secure), secure);
    Ok((
        AppendHeaders([(axum::http::header::SET_COOKIE, clear)]),
        Flash::notice("Signed out."),
        Redirect::to("/dev/guests/sign-in"),
    )
        .into_response())
}

/// Deletes the session, then wakes the person's connections (each re-checks its own session).
pub async fn end_session(s: &AppState, signed: &Signed) -> Result<(), AppError> {
    guest_session::destroy(&s.db, signed.session_id).await?;
    crate::channels::session_ended(s, signed.person.id);
    Ok(())
}
