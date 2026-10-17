//! `app mailbox …`: the dev mailbox from a terminal, for humans and agents without a browser.
//! It reads the same `mailbox_messages` table as `/dev/mailbox`, so mail from any process shows.
//! GUIDE: .offrails/guides/mailers.md#for-agents

use std::time::Duration;

use clap::Subcommand;
use sea_orm::DatabaseConnection;

use crate::kernel::mailbox::{self, Filter, Model};
use crate::kernel::mailer::{Email, Mailer};

#[derive(Debug, Subcommand)]
pub enum MailboxCommand {
    /// Print new mail as it arrives (Ctrl-C stops), with its links and codes.
    Tail {
        /// Only mail to this address.
        #[arg(long)]
        to: Option<String>,
    },
    /// The newest messages, one line each.
    List {
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
    /// One message as JSON (the same shape as /dev/mailbox/latest.json); the newest without an id.
    Show { id: Option<i64> },
    /// Delete every message.
    Clear,
    /// Development only: fill the mailbox with realistic sample mail (sign-in link, code,
    /// receipt with attachments, digest, password reset) to try the viewer.
    Seed {
        /// The app's address, for the links in the sample mail.
        #[arg(long, default_value = "http://localhost:3000")]
        app_url: String,
    },
}

pub async fn run(db: &DatabaseConnection, command: MailboxCommand) -> anyhow::Result<()> {
    match command {
        MailboxCommand::Tail { to } => tail(db, to).await,
        MailboxCommand::List { to, limit } => {
            let filter = Filter {
                to,
                ..Filter::default()
            };
            for m in mailbox::filter(db, &filter, limit).await?.iter().rev() {
                println!("{}", line(m));
            }
            Ok(())
        }
        MailboxCommand::Show { id } => {
            let m = match id {
                Some(id) => mailbox::find(db, id).await?,
                None => mailbox::latest(db, None).await?,
            };
            let m = m.ok_or_else(|| anyhow::anyhow!("no such message"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&mailbox::summary_json(&m))?
            );
            Ok(())
        }
        MailboxCommand::Clear => {
            println!("cleared {} message(s)", mailbox::clear(db).await?);
            Ok(())
        }
        MailboxCommand::Seed { app_url } => {
            let from: lettre::message::Mailbox =
                format!("{} <hello@example.com>", crate::config::APP_NAME).parse()?;
            let mailer = Mailer::local(db.clone(), from);
            let mail = seed_mail(&app_url);
            let n = mail.len();
            for email in mail {
                mailer.deliver(email).await?;
            }
            println!("seeded {n} messages");
            Ok(())
        }
    }
}

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="color-scheme" content="light dark"><title>{title}</title>
<style>body{{margin:0;padding:32px 12px;background:#f3f4f6;font:15px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;color:#111827}}.card{{max-width:560px;margin:0 auto;background:#fff;border:1px solid #e5e7eb;border-radius:10px;padding:32px}}h1{{font-size:22px;margin:0 0 12px}}.btn{{display:inline-block;background:#4f46e5;color:#fff!important;text-decoration:none;font-weight:600;padding:11px 20px;border-radius:7px}}.muted{{color:#6b7280;font-size:13px}}.code{{font:700 30px/1.2 ui-monospace,Menlo,monospace;letter-spacing:.25em;background:#f5f3ff;border-radius:8px;padding:14px 18px;display:inline-block}}table{{width:100%;border-collapse:collapse}}td{{padding:8px 0;border-bottom:1px solid #e5e7eb}}@media (prefers-color-scheme: dark){{body{{background:#0b0f19;color:#e5e7eb}}.card{{background:#111827;border-color:#374151}}.code{{background:#2e1065}}td{{border-color:#374151}}}}</style></head>
<body><div class="card">{body}</div><p class="muted" style="text-align:center">Acme Inc. · 1 Example Street · Springfield</p></body></html>"#
    )
}

/// What `app mailbox seed` stores: one of each kind of mail an app sends.
fn seed_mail(app_url: &str) -> Vec<Email> {
    use crate::kernel::mailer::Attachment;
    // A small PNG and a minimal PDF, so the attachment list has something real to show.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x20, 0x08, 0x02, 0x00, 0x00, 0x00, 0xfc,
        0x18, 0xed, 0xa3, 0x00, 0x00, 0x01, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0xb5, 0xcd,
        0x57, 0x76, 0x82, 0x00, 0x00, 0x04, 0xc0, 0x3d, 0x5d, 0x7a, 0xa2, 0x41, 0x45, 0xb1, 0x17,
        0xac, 0xd8, 0x8d, 0x28, 0xd6, 0x97, 0x62, 0xca, 0x41, 0x11, 0x04, 0x11, 0x41, 0xd1, 0xdf,
        0x70, 0x89, 0xbd, 0xc0, 0x0c, 0x22, 0x37, 0x7a, 0xe4, 0x56, 0x8f, 0xde, 0xe9, 0xaf, 0xf7,
        0xba, 0xf0, 0xb0, 0x15, 0x1e, 0xb7, 0xb1, 0xa7, 0x6d, 0xfc, 0xd9, 0x48, 0xbc, 0x18, 0x62,
        0xc4, 0x10, 0xa3, 0x46, 0x52, 0x30, 0x53, 0x31, 0x53, 0x8a, 0x9b, 0x52, 0x62, 0x97, 0x16,
        0x77, 0x99, 0xe4, 0x2e, 0x9b, 0xb2, 0x72, 0x92, 0x95, 0x4b, 0x5b, 0xf9, 0x8c, 0x55, 0xc8,
        0xda, 0xc5, 0xbc, 0x5d, 0x2c, 0xd8, 0xa5, 0xe2, 0xbe, 0x5c, 0xda, 0x57, 0xca, 0x7b, 0xb9,
        0xe2, 0xc8, 0xb2, 0x53, 0xad, 0x3a, 0xb5, 0x9a, 0x53, 0xaf, 0x1f, 0x40, 0xd5, 0x1b, 0xcd,
        0x03, 0xa8, 0x7a, 0xa3, 0x75, 0x00, 0x55, 0x6f, 0x2a, 0x2e, 0xa8, 0x7a, 0xab, 0x1d, 0x06,
        0x4c, 0x5d, 0xe9, 0xb8, 0xa0, 0xea, 0x4a, 0xd7, 0x05, 0x55, 0x6f, 0xf7, 0x8e, 0xa0, 0xea,
        0x9d, 0xfe, 0x11, 0x54, 0xbd, 0x3b, 0x08, 0x03, 0xa6, 0xde, 0x1b, 0x7a, 0xa0, 0xea, 0xbd,
        0x91, 0x07, 0xaa, 0xde, 0x1f, 0x7b, 0xa0, 0xea, 0x03, 0xd5, 0x07, 0x55, 0x1f, 0x4e, 0x7c,
        0x50, 0xf5, 0xe1, 0xd4, 0x07, 0x55, 0x1f, 0x69, 0x3e, 0xa8, 0xfa, 0xdb, 0xec, 0x04, 0xaa,
        0x3e, 0x9e, 0x87, 0x01, 0x53, 0x57, 0x17, 0x27, 0x50, 0x75, 0x75, 0x79, 0x06, 0x55, 0x9f,
        0xac, 0xce, 0xa0, 0xea, 0xd3, 0x75, 0x18, 0x30, 0x75, 0xed, 0x3d, 0x00, 0x55, 0xd7, 0x3e,
        0x02, 0x50, 0xf5, 0xd9, 0x67, 0x00, 0xaa, 0x3e, 0xff, 0x0a, 0x40, 0xd5, 0x17, 0x9b, 0x0b,
        0xa8, 0xfa, 0xf2, 0xfb, 0x02, 0xaa, 0xbe, 0xfc, 0xb9, 0x80, 0xaa, 0xaf, 0x7e, 0xaf, 0xa0,
        0xea, 0xeb, 0xbf, 0xeb, 0x3f, 0xb0, 0x6f, 0xef, 0xcc, 0xdb, 0xa1, 0x02, 0x82, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    const PDF: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Count 0>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";
    let link = |path: &str| format!("{app_url}{path}");
    // WHY: oldest first; the newest (a code) is what the inbox opens on.
    vec![
        Email::new(
            "team@example.com",
            "Margaret invited you to the Launch board",
            page("Invite", r#"<h1>You're invited</h1><p>Margaret invited you to collaborate on <strong>Launch</strong>.</p><p><a class="btn" href="/invitations/accept">Accept the invitation</a></p>"#),
            String::new(),
        ),
        Email::new(
            "ada@example.com",
            "Welcome to Acme, Ada",
            page("Welcome", &format!(r#"<h1>Welcome aboard, Ada</h1><p>Your workspace is ready. Invite your team and create your first board.</p><p><a class="btn" href="{}">Open your workspace</a></p><p class="muted">Questions? Just reply to this email.</p>"#, link("/boards"))),
            format!("Welcome aboard, Ada\n\nYour workspace is ready. Invite your team and create your first board.\n\nOpen your workspace: {}\n\nQuestions? Just reply to this email.\n", link("/boards")),
        )
        .reply_to("support@example.com"),
        Email::new(
            "ada@example.com",
            "Weekly digest: 3 boards changed",
            page("Digest", &format!(r#"<h1>This week at Acme</h1><img src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='496' height='120'%3E%3Crect width='496' height='120' rx='8' fill='%234f46e5'/%3E%3Ctext x='24' y='70' font-family='sans-serif' font-size='28' font-weight='700' fill='white'%3E3 boards changed%3C/text%3E%3C/svg%3E" width="496" height="120"><p><strong>Roadmap</strong>: 4 cards moved to Done.</p><p><strong>Launch</strong>: 2 new comments.</p><p><strong>Hiring</strong>: 1 card added.</p><p><a class="btn" href="{}">See what changed</a></p>"#, link("/boards"))),
            format!("This week at Acme\n\nRoadmap: 4 cards moved to Done.\nLaunch: 2 new comments.\nHiring: 1 card added.\n\nSee what changed: {}\n", link("/boards")),
        ),
        Email::new(
            "linus@example.com",
            "Receipt #1042 from Acme",
            page("Receipt", &format!(r#"<h1>Thanks for your order</h1><table><tr><td>Team plan (monthly)</td><td align="right">$48.00</td></tr><tr><td>Tax</td><td align="right">$4.32</td></tr><tr><td><strong>Total</strong></td><td align="right"><strong>$52.32</strong></td></tr></table><p>Your invoice is attached. <a href="{}">View billing</a></p>"#, link("/billing"))),
            format!("Thanks for your order\n\nTeam plan (monthly)  $48.00\nTax                   $4.32\nTotal                $52.32\n\nView billing: {}\n", link("/billing")),
        )
        .cc("billing@example.com")
        .attach(Attachment::new("invoice-1042.pdf", "application/pdf", PDF))
        .attach(Attachment::new("logo.png", "image/png", PNG)),
        Email::new(
            "linus@example.com",
            "Reset your password",
            page("Reset", &format!(r#"<h1>Reset your password</h1><p>Someone asked to reset the password for this account. If it was you:</p><p><a class="btn" href="{}">Choose a new password</a></p><p class="muted">The link expires in 1 hour. If it wasn't you, your password is unchanged.</p>"#, link("/passwords/Zp4Kq8sW2nVx6Hj1/edit"))),
            format!("Reset your password\n\nIf you asked to reset your password, open this link (it expires in 1 hour):\n{}\n\nIf it wasn't you, your password is unchanged.\n", link("/passwords/Zp4Kq8sW2nVx6Hj1/edit")),
        ),
        Email::new(
            "grace@example.com",
            "Your sign-in link",
            page("Sign in", &format!(r#"<h1>Sign in to Acme</h1><p>Click the button to sign in. The link works once and expires in 15 minutes.</p><p><a class="btn" href="{}">Sign in</a></p><p class="muted">If you didn't ask for this, you can ignore it.</p>"#, link("/session/7Qe2xK9vT1mWb4rN"))),
            format!("Sign in to Acme\n\nThis link works once and expires in 15 minutes:\n{}\n\nIf you didn't ask for this, you can ignore it.\n", link("/session/7Qe2xK9vT1mWb4rN")),
        ),
        Email::new(
            "grace@example.com",
            "Your verification code: 482 913",
            page("Verification code", r#"<h1>Your verification code</h1><p>Enter this code to finish signing in:</p><p class="code">482 913</p><p class="muted">It expires in 10 minutes.</p>"#),
            "Your verification code is 482 913.\n\nIt expires in 10 minutes.\n".to_owned(),
        ),
    ]
}

/// `#12  14:03:22  ada@example.com  Your sign-in link`
fn line(m: &Model) -> String {
    format!(
        "#{:<5} {}  {}  {}",
        m.id,
        m.created_at.format("%H:%M:%S"),
        m.to_addr,
        m.subject
    )
}

async fn tail(db: &DatabaseConnection, to: Option<String>) -> anyhow::Result<()> {
    let mut since = mailbox::newest_id(db).await?;
    eprintln!("waiting for mail (Ctrl-C stops)…");
    loop {
        let filter = Filter {
            to: to.clone(),
            since: Some(since),
            ..Filter::default()
        };
        // WHY: oldest first, so a burst prints in the order it was sent.
        for m in mailbox::filter(db, &filter, 100).await?.iter().rev() {
            println!("{}", line(m));
            for code in mailbox::codes_in(&m.text) {
                println!("        code  {code}");
            }
            for link in mailbox::links_in(&m.text, &m.html) {
                println!("        link  {link}");
            }
            since = since.max(m.id);
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::seed_mail;
    use crate::kernel::mailbox_web::checks;

    /// The seeded "Weekly digest" has a 496 px banner: the 320 px check flags it, and only it.
    #[test]
    fn the_seeded_weekly_digest_overflows_at_320_px() {
        for email in seed_mail("http://localhost:3000") {
            let flagged = checks(&email.text, &email.html, &[], &email.subject)
                .iter()
                .any(|c| c.message.contains("320 px"));
            assert_eq!(
                flagged,
                email.subject.starts_with("Weekly digest"),
                "{}",
                email.subject
            );
        }
    }
}
