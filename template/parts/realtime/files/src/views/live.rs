//! The live room's pages: the room (two panes: who's here, the conversation), its header (a
//! live region refreshed over the channel), and the development sign-in.

use askama::Template;

use super::Ctx;
use crate::channels::identity::Person;
use crate::models::Errors;
use crate::models::room;
use crate::models::room_message;
use crate::routes::paths;

pub struct RoomLink {
    pub slug: String,
    pub name: String,
    pub current: bool,
    pub members_only: bool,
}

pub struct MessageView {
    pub id: i64,
    pub author: String,
    pub initials: String,
    pub color: u8,
    pub body: String,
    /// RFC 3339; the page shows it in the reader's time zone.
    pub at: String,
    /// HH:MM UTC, until the script rewrites it to local time.
    pub time: String,
}

impl From<&room_message::Model> for MessageView {
    fn from(m: &room_message::Model) -> Self {
        Self {
            id: m.id,
            author: m.author_name.clone(),
            initials: crate::models::guest::initials(&m.author_name),
            color: room_message::color_of(m.author_id),
            body: m.body.clone(),
            at: m.created_at.to_rfc3339(),
            time: m.created_at.format("%H:%M").to_string(),
        }
    }
}

impl MessageView {
    /// The empty row the script clones for new messages.
    pub fn blank() -> Self {
        Self {
            id: 0,
            author: String::new(),
            initials: String::new(),
            color: 0,
            body: String::new(),
            at: String::new(),
            time: String::new(),
        }
    }
}

#[derive(Template)]
#[template(path = "live/show.html")]
pub struct LiveShow {
    pub ctx: Ctx,
    pub me: Person,
    pub room: room::Model,
    pub rooms: Vec<RoomLink>,
    pub messages: Vec<MessageView>,
    /// The newest message id the page shows: the socket resumes after it.
    pub since: i64,
    pub topic: String,
    pub user_topic: String,
    pub template_message: MessageView,
    /// The eight avatar colors, for the script.
    pub palette: Vec<u8>,
    /// Development guests can sign out here (accounts have their own sign-out).
    pub dev_guest: bool,
    pub errors: Errors,
}

#[derive(Template)]
#[template(path = "live/_header.html")]
pub struct LiveHeader {
    pub room: room::Model,
    pub errors: Errors,
}

#[derive(Template)]
#[template(path = "live/sign_in.html")]
pub struct LiveSignIn {
    pub ctx: Ctx,
    pub name: String,
    pub errors: Errors,
}
