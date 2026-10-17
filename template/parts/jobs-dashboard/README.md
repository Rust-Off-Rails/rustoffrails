# jobs-dashboard

A live operations dashboard for the [jobs](../jobs/) queue at **`/admin/jobs`**, in the spirit of Oban Web and Rails' Mission Control: what's running, what's failing and why, and the buttons to fix it, all server-rendered (Askama + htmx), identical in every `FRONTEND_MODE`, with its own stylesheet and script.

- **Overview.** Seven state counters (linked to the filtered list), a **throughput** chart (completed and failed per minute, last hour) and a **queue wait** chart (p50 and p95 from scheduled to started), a **now running** lane with elapsed time and each job's live **progress**, queues with status pills (running, paused, no nodes), limits, global rate limits and **Pause/Resume**, **per-worker success rates** with run-time p50/p95, and the nodes with their heartbeats. Everything refreshes live (1-10 s, per panel); a **Live/Paused** switch freezes the page.
- **Jobs.** State tabs with counts, a filter bar (queue, worker, tag, error text, and search by id, worker, queue or a value in the args as shown, redacted) whose URL is the filter, removable filter chips, keyset paging, and bulk **Retry, Run now, Cancel, Discard, Delete** on the selection or on **every job matching** the filter (up to 10,000), each behind a confirmation with the exact count.
- **Job.** State, worker, queue and the time that matters; actions valid for its state; a copyable `app jobs retry <id>`; a **timeline** (inserted → scheduled → each failed attempt → done); details; batch progress; args and meta pretty-printed (sensitive keys redacted); and **every attempt's error**, newest first, with its node and duration and the full report (cause chain and backtrace) one click away.
- **Errors.** Retryable and discarded jobs **grouped** by worker and **root cause** (the innermost error of the chain plus the host it names, numbers ignored, HTTP statuses kept), so one outage is one group; the outer context becomes the group's variants ("3 variants: order.paid, order.refunded, customer.updated"). With counts, first and last seen, a link to the jobs, the latest one, and **Retry discarded** for the group.
- **Cron.** Every `SCHEDULE` entry with its expression in words, the next run (relative and UTC), the last run (linked to the job it enqueued), and **Run now**.
- **Pause all** for deploys and migrations, with a banner while everything is paused.
- **Keyboard**: `g o`/`g j`/`g e`/`g c` sections, `/` search, `j`/`k` move through jobs, `x` select, `Enter` open, `l` live on/off, `?` the list.
- **Looks right everywhere**: light and dark, following the kernel header's System / Light / Dark switch ([theming](../FORMAT.md#theming)), WCAG 2.2 AA (axe-clean in both on every screen, desktop and mobile), usable at 320 px (the job list turns into cards), motion off under `prefers-reduced-motion`.
- **Safe**: admin-only (open only in development), every change a CSRF-checked POST, confirmations for anything bulk or destructive, nothing changes on GET, redirects only within the dashboard, `no-store` on every response, everything escaped, secret-looking args redacted.

Extracted from Rust off Rails apps and rebuilt from the best of two of them. Kept: the state cards, the stacked chart with its legend, running/paused/no-nodes pills with Pause buttons, keyset paging and the confirmation page for bulk actions. Fixed: a chart built as an HTML string and rendered with `|safe` (now an SVG template of numbers, no `|safe` anywhere), dashboard CSS living in a shared stylesheet that pages forgot to link (now its own file, linked by the page, with a test that every template class has a rule), destructive single-job actions without confirmation, and a cron page that found its last job by string-matching JSON. Added: queue-wait percentiles, the now-running lane with progress, worker success rates, error groups, discard and pause-all, the timeline, per-attempt reports, filter chips, keyboard navigation, copyable CLI commands, the mobile card layout and the password gate's rate limit.

Guide: [`.offrails/guides/jobs-dashboard.md`](https://rustoffrails.org/docs/jobs-dashboard) · [`.offrails/guides/jobs.md`](https://rustoffrails.org/docs/jobs). Depends on: [jobs](../jobs/).

## Screenshots

![The overview, light, desktop](../../docs/screenshots/jobs-dashboard/overview-light-desktop.png)

| Screen | Light | Dark |
|--------|-------|------|
| Overview | [desktop](../../docs/screenshots/jobs-dashboard/overview-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/overview-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/overview-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/overview-dark-mobile.png) |
| Jobs | [desktop](../../docs/screenshots/jobs-dashboard/list-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/list-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/list-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/list-dark-mobile.png) |
| Jobs, filtered | [desktop](../../docs/screenshots/jobs-dashboard/list-filtered-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/list-filtered-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/list-filtered-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/list-filtered-dark-mobile.png) |
| A failed job | [desktop](../../docs/screenshots/jobs-dashboard/job-failed-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/job-failed-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/job-failed-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/job-failed-dark-mobile.png) |
| A running job | [desktop](../../docs/screenshots/jobs-dashboard/job-running-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/job-running-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/job-running-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/job-running-dark-mobile.png) |
| A retrying job | [desktop](../../docs/screenshots/jobs-dashboard/job-retrying-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/job-retrying-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/job-retrying-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/job-retrying-dark-mobile.png) |
| Errors | [desktop](../../docs/screenshots/jobs-dashboard/errors-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/errors-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/errors-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/errors-dark-mobile.png) |
| Cron | [desktop](../../docs/screenshots/jobs-dashboard/cron-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/cron-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/cron-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/cron-dark-mobile.png) |
| Bulk confirmation | [desktop](../../docs/screenshots/jobs-dashboard/confirm-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/confirm-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/confirm-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/confirm-dark-mobile.png) |
| Keyboard shortcuts | [desktop](../../docs/screenshots/jobs-dashboard/shortcuts-light-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/shortcuts-light-mobile.png) | [desktop](../../docs/screenshots/jobs-dashboard/shortcuts-dark-desktop.png) · [mobile](../../docs/screenshots/jobs-dashboard/shortcuts-dark-mobile.png) |

## Install

Through the samples MCP server: `get_sample jobs-dashboard`. Or by hand, in the app root, after the [jobs](../jobs/) sample:

1. No crates to add.
2. Copy `files/` into the app, keeping the paths. In the test file, replace `offrails_host` with your crate name.
3. Wiring (each at its `samples: <anchor>` comment; the exact text is in `sample.toml`):
   - `src/kernel/mod.rs`: `pub mod jobs_web;` · `src/controllers/mod.rs`: `pub mod admin_access;`.
   - `src/config.rs`: a `jobs_dashboard: JobsDashboardConfig` field (field, `Debug` line, `let jobs_dashboard = JobsDashboardConfig::from_env()?;`, `jobs_dashboard,`, `jobs_dashboard: JobsDashboardConfig::default()` in `Config::test()`), and the `JobsDashboardConfig` struct at `config sections`.
   - `src/routes.rs`: at `routes`, merge `kernel::jobs_web::router(jobs::recurring::SCHEDULE)` with `.route_layer(from_extractor_with_state::<AdminAccess, _>(state.clone()))` when `admin_access::dashboard_enabled(&config)`, else log a warning; a `paths::admin_jobs()` helper; the eleven `ROUTES` rows, all `Access::Admin`.
   - `src/kernel/ui.rs`: a `Tab { key: "jobs", label: "Jobs", href: "/admin/jobs", dev_only: false }`.
   - `src/controllers/dev.rs`: the Jobs tile. `templates/layouts/application.html`: a development footer link `<a href="/admin/jobs" hx-boost="false" class="link">Jobs</a>`.
   - `src/tasks/mod.rs`: `pub mod jobs_demo;` and `"jobs_demo" => jobs_demo::run(state, args).await,`.
   - `.env.example` and `compose.yaml`: `APP_ADMIN_PASSWORD`.
   - `tests/app/main.rs`: `mod jobs_dashboard_test;` · `tests/app/security_test.rs`: `"src/kernel/jobs_web/mod.rs"` in the route sources · `tests/app/kernel_web_test.rs`: the four page paths in the no-store list · `tests/app/theme_switch_test.rs`: the same four in `KERNEL_PAGES`.
4. `cargo fmt --all && cargo run -- routes`, then the Definition of Done.

`tools/install.sh jobs-dashboard <app>` installs jobs first if needed, then does steps 1 to 3.

## After installing

- **Decide the gate** (below) before deploying, and say it in `docs/decisions.md`. Without one, production doesn't mount the dashboard at all and logs a warning.
- Every link from an app page into `/admin/jobs` carries `hx-boost="false"` (`app_links_into_kernel_pages_are_not_boosted`).
- Try it: `cargo run -q -- task jobs_demo` (development only: an hour of synthetic history, live reports with progress, failing webhooks, a batch, cron runs, a paused queue), then `bin/dev` and `/admin/jobs`; press `?`.

## Adapting it

- **Put it behind your own admin accounts** (with an auth sample installed). In `src/controllers/admin_access.rs` (`// APP:`), make `admin_from_session` ask the app's current-user extractor and return `Some(user.is_admin())` (or `None` when nobody is signed in), and set `HAS_ACCOUNTS = true`. Signed-in non-admins get a 404, anonymous visitors the login page. Add a test like `the_admin_password_gates_every_dashboard_route` that signs in a member and an admin. Keep `APP_ADMIN_PASSWORD` unset, or keep it as a break-glass login.
- **A password instead (no accounts).** Set `APP_ADMIN_PASSWORD` (16+ characters) in production: HTTP Basic, compared in constant time, 10 failed tries per 3 minutes per client IP. Behind a proxy, set `APP_TRUSTED_PROXY=true` so the limit counts real client IPs.
- **Brand it.** The state and chart tones are tokens at the top of `static/kernel/jobs.css` (`/* APP: */`), each `light-dark(<light>, <dark>)`; the shared chrome is `static/kernel/admin.css`. Keep AA contrast in both and never add a `prefers-color-scheme` block (it ignores the theme switch). JS that reads a color (a canvas chart) re-reads it on `document`'s `themechange` event; today's charts are SVG styled by CSS, so they need nothing.
- **Change refresh rates or windows.** The `hx-trigger="every …"` attributes in `templates/kernel/jobs/_*.html`; `PAGE_SIZE`, `CHART_MINUTES` and `ERROR_SCAN` in `src/kernel/jobs_web/mod.rs`.
- **Show more of a job.** Add a field to `JobView` in `views.rs` and a row to `show.html`. Args and meta must keep going through `redact`.

## Do not change

- The `AdminAccess` route layer on the whole router, and no mount in production without a gate (`production_without_an_admin_gate_is_404`, `the_admin_password_gates_every_dashboard_route`).
- The failed-password limit that also refuses the right password once exhausted (`failed_admin_passwords_are_rate_limited`).
- POST-only actions through the app's CSRF layer (`cross_site_dashboard_posts_are_rejected`, `get_never_changes_anything`).
- Confirmation before bulk and destructive actions, and the refusal of an empty bulk filter (`bulk_actions_confirm_with_the_exact_count_and_refuse_an_empty_filter`, `single_job_actions_post_redirect_and_confirm_when_destructive`).
- `safe_return_to` (`return_to_only_accepts_dashboard_paths`, `actions_never_redirect_off_the_dashboard`).
- Escaping everywhere, redaction of args and meta, no `|safe` (`job_data_is_escaped_and_redacted_everywhere`).
- `no_store()` on the router (`every_dashboard_response_is_no_store`).
- The page's own `jobs.css`/`jobs.js` on its `Chrome`, and a rule for every template class (`the_dashboard_loads_its_own_css_and_js`, `every_dashboard_class_has_a_css_rule`).

## How it works

`src/kernel/jobs_web/mod.rs` is an axum `Router<AppState>` of handlers that call `kernel::jobs::admin` (the same functions `app jobs` uses) and render Askama templates from plain view structs (`views.rs`), so templates hold no logic. Live parts are fragments: the same URL with `?part=stats` (and `HX-Request`) returns just that panel, which htmx swaps in on a timer; `?live=0` renders no `hx-*` attributes at all, so the page stands still. Charts are SVG built in templates from precomputed coordinates; colors come from CSS classes, never `style=` (the CSP forbids inline styles), and progress bars are native `<progress>`/`<meter>`.

Actions are forms that POST to the dashboard and redirect (303) back, with a flash and an `X-Jobs-Changed: <n>` header. Bulk and destructive ones first answer with a confirmation page that repeats the form with `confirmed=1`. `controllers/admin_access.rs` is the gate.

| Route | Shows or does |
|-------|---------------|
| `GET /admin/jobs` | Overview; `?part=stats\|charts\|running\|queues\|workers\|nodes` |
| `GET /admin/jobs/list` | Jobs: `state queue worker tag q error before live`; `?part=rows` |
| `GET /admin/jobs/{id}` | A job; `?part=status` |
| `GET /admin/jobs/errors` | Error groups; `?worker=` |
| `GET /admin/jobs/cron` | The schedule |
| `POST /admin/jobs/{id}/{action}` | `retry`, `run`, `cancel`, `discard`, `delete` (the last three confirm first) |
| `POST /admin/jobs/bulk` | `action` + `ids`, or `all_matching=1` + filter fields; always confirms |
| `POST /admin/jobs/queues/{name}/{action}` | `pause`, `resume` |
| `POST /admin/jobs/queues/pause-all` · `resume-all` | Every queue (pause-all confirms first) |
| `POST /admin/jobs/cron/{name}/run` | Enqueue a cron entry now |

## Files

- `src/kernel/jobs_web/mod.rs`: the router, handlers, confirmation flow, `safe_return_to`.
- `src/kernel/jobs_web/views.rs`: view structs and formatting (counters, charts, rows, the job page, error groups).
- `src/controllers/admin_access.rs`: `AdminAccess`, `dashboard_enabled`, the hook for your accounts.
- `templates/kernel/jobs/`: `index.html` and its `_stats`, `_charts`, `_running`, `_queues`, `_workers`, `_nodes` fragments; `list.html` + `_rows.html`; `show.html` + `_job_status.html`; `errors.html`; `cron.html`; `confirm.html`; `_nav.html` (sections, live switch, pause-all, shortcuts dialog).
- `static/kernel/jobs.css`, `static/kernel/jobs.js`: the dashboard's own style and behavior.
- `src/tasks/jobs_demo.rs`: `app task jobs_demo`, synthetic data for development and the screenshots.
- `tests/app/jobs_dashboard_test.rs`: the tests.
