//! The component gallery and the layout demos (development only: mounted by `dev::router`, so
//! the same guard as `/dev`). Spec: .offrails/guides/views.md#components

use axum::Form;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::error::AppError;
use crate::kernel::flash::Flash;
use crate::kernel::theme::Theme;
use crate::models::Errors;
use crate::routes::paths;
use crate::views::gallery::{
    AuthDemo, DemoForm, DocsDemo, Gallery, GalleryModal, MarketingDemo, ROWS, SidebarDemo,
};
use crate::views::{self, Ctx};

#[derive(Debug, Default, Deserialize)]
pub struct GalleryQuery {
    sort: Option<String>,
    dir: Option<String>,
    page: Option<u64>,
    /// `light` or `dark`: this view in that theme, whatever the switch says.
    theme: Option<String>,
}

fn themed(mut ctx: Ctx, theme: Option<&str>) -> Ctx {
    if let Some(t) = theme.and_then(Theme::parse) {
        ctx.theme = t;
    }
    ctx.titled("Components").noindex()
}

#[tracing::instrument(skip_all)]
pub async fn index(ctx: Ctx, Query(q): Query<GalleryQuery>) -> Result<Response, AppError> {
    let ctx = themed(ctx, q.theme.as_deref());
    let page = Gallery::new(
        ctx,
        q.sort.as_deref().unwrap_or("title"),
        q.dir.as_deref().unwrap_or("asc"),
        q.page.unwrap_or(1),
    );
    Ok(views::render(page)?.into_response())
}

/// The demo form: 422 with the values kept and every error shown, or back with a notice.
#[tracing::instrument(skip_all)]
pub async fn submit(ctx: Ctx, Form(form): Form<DemoForm>) -> Result<Response, AppError> {
    if form.confirm_demo.is_some() {
        return Ok((
            Flash::notice("Confirmed in the dialog."),
            Redirect::to(&paths::dev_components()),
        )
            .into_response());
    }
    let errors = form.validate();
    if errors.is_empty() {
        return Ok((
            Flash::notice(format!("Saved \u{201c}{}\u{201d}.", form.title.trim())),
            Redirect::to(&paths::dev_components()),
        )
            .into_response());
    }
    let mut page = Gallery::new(themed(ctx, None), "title", "asc", 1);
    page.form = form;
    page.errors = errors;
    Ok((StatusCode::UNPROCESSABLE_ENTITY, views::render(page)?).into_response())
}

#[tracing::instrument(skip_all)]
pub async fn modal() -> Result<Response, AppError> {
    Ok(views::render(GalleryModal)?.into_response())
}

#[tracing::instrument(skip_all)]
pub async fn sidebar(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(SidebarDemo {
        ctx: ctx.titled("Sidebar layout").noindex(),
        rows: &ROWS,
    })?
    .into_response())
}

#[tracing::instrument(skip_all)]
pub async fn marketing(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(MarketingDemo {
        ctx: ctx.titled("Marketing layout").noindex(),
    })?
    .into_response())
}

#[tracing::instrument(skip_all)]
pub async fn auth(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(AuthDemo {
        ctx: ctx.titled("Auth layout").noindex(),
        email: String::new(),
        errors: Errors::default(),
    })?
    .into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct AuthDemoForm {
    #[serde(default)]
    email: String,
}

/// Nobody signs in here: the auth card's error state, with the email kept.
#[tracing::instrument(skip_all)]
pub async fn auth_submit(ctx: Ctx, Form(form): Form<AuthDemoForm>) -> Result<Response, AppError> {
    let errors = Errors::single("password", "is wrong (this demo signs nobody in)");
    let page = AuthDemo {
        ctx: ctx.titled("Auth layout").noindex(),
        email: form.email,
        errors,
    };
    Ok((StatusCode::UNPROCESSABLE_ENTITY, views::render(page)?).into_response())
}

#[tracing::instrument(skip_all)]
pub async fn docs(ctx: Ctx) -> Result<Response, AppError> {
    Ok(views::render(DocsDemo {
        ctx: ctx.titled("Docs layout").noindex(),
    })?
    .into_response())
}
