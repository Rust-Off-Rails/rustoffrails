//! `app/mailers`: one file per mailer. Each action renders an HTML and a text template into an
//! `Email`, with absolute URLs only. Spec: .offrails/guides/mailers.md#writing-a-mailer

pub mod previews;
pub mod test_mailer;
// samples: mailer mods

pub use crate::kernel::mailer::{Attachment, Email};
