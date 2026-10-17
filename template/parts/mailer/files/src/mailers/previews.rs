//! Mailer previews (`/rails/mailers`): every mailer action, rendered from fake data. The dev
//! mailbox lists them under Previews and shows them in the same viewer as real mail. Never sent,
//! never stored, and they never touch the database.

pub use crate::kernel::mailer::Preview;

use crate::mailers::test_mailer;

// APP: one entry per mailer action, built from fake data (never the database).
pub const PREVIEWS: &[Preview] = &[
    Preview {
        name: "test_mailer/test_email",
        build: |s| Box::pin(async { test_mailer::test_email(&s.config, "ada@example.com") }),
    },
    // samples: previews
];
