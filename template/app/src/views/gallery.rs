//! `/dev/components` and `/dev/layouts/*` (development only): every component of
//! templates/components/ui.html in every state, and each layout with sample content. The page an
//! agent opens (or screenshots with `cargo offrails prove`) to see the brand and the kit at once.
//! GUIDE: .offrails/guides/views.md#components

use askama::Template;
use serde::Deserialize;

use super::Ctx;
use crate::kernel::flash::{FlashKind, FlashMessage};
use crate::models::Errors;
use crate::routes::paths;

/// The demo form: one field of every kind. Re-rendered with what was typed on a 422.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DemoForm {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub contact: String,
    #[serde(default)]
    pub terms: Option<String>,
    /// Set by the confirm-dialog demo's button.
    #[serde(default)]
    pub confirm_demo: Option<String>,
}

impl DemoForm {
    pub fn validate(&self) -> Errors {
        let mut e = Errors::default();
        if self.title.trim().is_empty() {
            e.add("title", "can't be blank");
        }
        if !self.email.contains('@') {
            e.add("email", "must be an email address");
        }
        if !PLANS.iter().any(|(v, _)| *v == self.plan) {
            e.add("plan", "must be one of the plans");
        }
        if self.terms.is_none() {
            e.add("terms", "must be accepted");
        }
        e
    }
}

pub const PLANS: &[(&str, &str)] = &[("free", "Free"), ("team", "Team"), ("business", "Business")];
pub const CONTACTS: &[(&str, &str)] = &[
    ("email", "Email"),
    ("phone", "Phone"),
    ("none", "Don't contact me"),
];

pub struct DemoRow {
    pub title: &'static str,
    pub author: &'static str,
    pub status: &'static str,
    pub year: i32,
}

pub const ROWS: [DemoRow; 4] = [
    DemoRow {
        title: "The Left Hand of Darkness",
        author: "Ursula K. Le Guin",
        status: "reading",
        year: 1969,
    },
    DemoRow {
        title: "Kindred",
        author: "Octavia E. Butler",
        status: "done",
        year: 1979,
    },
    DemoRow {
        title: "Piranesi",
        author: "Susanna Clarke",
        status: "next",
        year: 2020,
    },
    DemoRow {
        title: "Dune",
        author: "Frank Herbert",
        status: "done",
        year: 1965,
    },
];

#[derive(Template)]
#[template(path = "dev/components.html")]
pub struct Gallery {
    pub ctx: Ctx,
    /// The live demo form and its errors (empty until a submit fails).
    pub form: DemoForm,
    pub errors: Errors,
    /// The fields' error state, shown beside the live form.
    pub bad: Errors,
    pub rows: Vec<&'static DemoRow>,
    pub sort: String,
    pub dir: String,
    pub page: u64,
    pub notice: Option<FlashMessage>,
    pub alert: Option<FlashMessage>,
}

impl Gallery {
    pub fn new(ctx: Ctx, sort: &str, dir: &str, page: u64) -> Self {
        let mut rows: Vec<&'static DemoRow> = ROWS.iter().collect();
        match sort {
            "author" => rows.sort_by_key(|r| r.author),
            "year" => rows.sort_by_key(|r| r.year),
            _ => rows.sort_by_key(|r| r.title),
        }
        if dir == "desc" {
            rows.reverse();
        }
        let mut bad = Errors::default();
        bad.add("bad_title", "can't be blank");
        bad.add("bad_plan", "must be one of the plans");
        bad.add("bad_terms", "must be accepted");
        Self {
            ctx,
            form: DemoForm::default(),
            errors: Errors::default(),
            bad,
            rows,
            sort: if ["title", "author", "year"].contains(&sort) {
                sort.to_owned()
            } else {
                "title".to_owned()
            },
            dir: if dir == "desc" {
                "desc".to_owned()
            } else {
                "asc".to_owned()
            },
            page,
            notice: Some(FlashMessage {
                kind: FlashKind::Notice,
                message: "Saved. (A notice flash.)".into(),
            }),
            alert: Some(FlashMessage {
                kind: FlashKind::Alert,
                message: "That didn't work. (An alert flash.)".into(),
            }),
        }
    }
}

/// The htmx-loaded modal (`ui::modal_link` → this fragment, which opens itself).
#[derive(Template)]
#[template(path = "dev/_modal.html")]
pub struct GalleryModal;

#[derive(Template)]
#[template(path = "dev/layouts/sidebar.html")]
pub struct SidebarDemo {
    pub ctx: Ctx,
    pub rows: &'static [DemoRow],
}

#[derive(Template)]
#[template(path = "dev/layouts/marketing.html")]
pub struct MarketingDemo {
    pub ctx: Ctx,
}

#[derive(Template)]
#[template(path = "dev/layouts/auth.html")]
pub struct AuthDemo {
    pub ctx: Ctx,
    pub email: String,
    pub errors: Errors,
}

#[derive(Template)]
#[template(path = "dev/layouts/docs.html")]
pub struct DocsDemo {
    pub ctx: Ctx,
}
