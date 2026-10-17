# Generator: `mailer` (`rails generate mailer`)

> **TL;DR**
> - `cargo offrails g mailer review_mailer new_review` writes `src/mailers/review_mailer.rs` (a pure fn per action returning an `Email`: HTML + text on the shared mail layout, absolute links), `templates/mailers/review_mailer/new_review.{html,txt}`, a preview per action in `PREVIEWS` (the dev mailbox's Previews tab), and `tests/app/review_mailer_test.rs`; it needs the `mailer` part.
> - Then give each action its data and words, and send it **from a job**: `state.mailer.deliver(email)` in `perform` (`cargo offrails g job NotifyReviewAuthor review_id:bigint`), enqueued from the model fn in the same transaction.
> - Mail carrying a token gets its own job that mints the token; never render secrets into job args.
>
> Read the rest when adding an email.

```
cargo offrails g mailer review_mailer new_review
cargo offrails g job notify_review_author review_id:bigint --queue mailers
```

1. `src/mailers/review_mailer.rs`: make the generated fn take what the email is about: `pub fn new_review(cfg: &Config, to: &str, review: &Review, book: &Book) -> Result<Email, AppError>`. Pure: data in, rendered `Email` out, no DB access. Callers load the data.
2. Templates: `templates/mailers/review_mailer/new_review.html` and `.txt` share field names (two Askama structs, `NewReviewHtml` / `NewReviewText`). Links are absolute (`cfg.app_url` + `paths::…`). Keep the HTML simple: table layout, inline styles, no external CSS. The HTML template `{% extends "mailers/layout.html" %}`.
3. Send it from a job: in the job's `perform`, load the records by id (a gone record is `Outcome::Cancel`), render, then `state.mailer.deliver(email).await.context("delivering …")?`. Enqueue the job from the owning model fn, inside its transaction (`jobs::enqueue(&txn, NotifyReviewAuthorJob { review_id })`), so it never exists if the change rolls back. If the email carries a token (password reset, magic link, invitation), the job mints the token and stores its digest; its args hold only ids ([auth](../guides/auth.md), [mailers](../guides/mailers.md#delivering)).
4. **Preview:** the generator added `Preview { name: "review_mailer/new_review", build: … }` to `PREVIEWS` in `src/mailers/previews.rs`; build it from fake data (no DB) once the fn takes records.
5. Test: the generated test renders each action (recipient, subject, absolute links in both parts) and delivers it; add the flow: trigger the model fn, `drain_jobs()`, then assert `last_email_to(…)` has the subject and a key link (follow it with `app.get`).
6. Exercise it for real: run the app, trigger the flow, then `curl -s localhost:3000/dev/mailbox/latest.json?to=<addr>` and follow a link ([dev-tools](../guides/dev-tools.md)). Tell the human they can see it at http://localhost:3000/dev/mailbox.
7. Development always delivers to the dev mailbox (`Mailer::Local`), whatever `SMTP_URL` says. If `EMAIL` in `AGENTS.md` has no production provider yet, tell the human that production mail is only logged until they choose one (set `SMTP_URL`).
