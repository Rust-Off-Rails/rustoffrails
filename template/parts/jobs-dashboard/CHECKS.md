# jobs-dashboard: checks

What "working and safe" means for `/admin/jobs`, and the test that proves each item. `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh jobs-dashboard` runs them all on SQLite and Postgres.

The dashboard can retry, cancel, discard and delete work, pause every queue, and shows job arguments and error reports, which carry personal data and, by mistake, secrets. Treat it as an admin console: every visitor is hostile until proven an admin, and every byte of job data is hostile markup.

## For the reviewer

- Access: `src/controllers/admin_access.rs` (the gate, applied as a `route_layer` to the whole router in the `routes` wiring in `sample.toml`) and `dashboard_enabled` (no mount in production without a gate).
- State changes: only the `post(...)` routes in `src/kernel/jobs_web/mod.rs`; `GET` handlers only read. CSRF is the app's `CsrfLayer` (Fetch Metadata, then `Origin`), outside the dashboard.
- Output: Askama auto-escaping; `rg '\|safe' templates/kernel/jobs` finds nothing. Args and meta go through `kernel::jobs::redact` in `views.rs`. `jobs.js` builds no HTML.
- SQL: every filter reaches `kernel::jobs::admin::condition` (the jobs sample) as a bound value.
- Accepted risks are at the end.

## Threat model

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | An anonymous visitor opens the dashboard in production | No gate configured: the router isn't mounted (404). With `APP_ADMIN_PASSWORD`: every route answers 401 with `WWW-Authenticate` until the right password arrives. With accounts: `admin_from_session` decides | `production_without_an_admin_gate_is_404`, `the_admin_password_gates_every_dashboard_route`, `non_public_routes_never_answer_2xx_anonymously` |
| Spoofing | The admin password is brute-forced | 16+ characters enforced at boot; 10 failed tries per 3 minutes per client IP, after which even the right password gets 429 until the window passes; credential-less requests don't count | `failed_admin_passwords_are_rate_limited`, `a_short_admin_password_is_refused` |
| Spoofing | Timing reveals how much of a guessed password matched | SHA-256 digests compared in constant time | `secrets_compare_exactly`, `basic_credentials_parse` |
| Tampering | A page on another site makes an admin's browser retry, delete or pause (CSRF) | Every change is a POST through the app's `CsrfLayer` (Fetch Metadata, then `Origin`) | `cross_site_dashboard_posts_are_rejected`, `cross_site_posts_to_every_unsafe_route_are_403` |
| Tampering | A link or image (`<img src="/admin/jobs/1/delete">`) changes jobs on GET | Routes that change anything are `post` only; GET answers 405/404 | `get_never_changes_anything` |
| Tampering | A mis-click or a forged form deletes thousands of jobs | Bulk actions and single-job cancel/discard/delete render a confirmation with the exact count; an empty bulk filter is refused even when "confirmed"; bulk is capped at 10,000 | `bulk_actions_confirm_with_the_exact_count_and_refuse_an_empty_filter`, `single_job_actions_post_redirect_and_confirm_when_destructive`, `queues_pause_and_resume_and_pause_all_asks_first` |
| Tampering | Job args, tags, worker names or errors carry markup that runs in the admin's browser (stored XSS) | Askama escapes every value; charts are SVG templates of numbers; no `|safe`; the CSP forbids inline script and style; `jobs.js` uses no `eval`/`innerHTML` | `job_data_is_escaped_and_redacted_everywhere`, `the_dashboard_loads_its_own_css_and_js` |
| Tampering | Filter or search input becomes SQL | Bound parameters only; `LIKE` wildcards escaped; bad `state`/`before` values are ignored, never a 500 | `filters_and_search_are_parameterized_against_injection`, `the_job_list_filters_searches_and_pages` |
| Information disclosure | Search probes a redacted arg value (match counts as an oracle) | The jobs kernel matches args only through their redacted copy | `search_never_matches_redacted_values` |
| Tampering | `return_to` sends the admin to an attacker's site after an action (open redirect) | `safe_return_to` accepts only `/admin/jobs`, `/admin/jobs/…` and `/admin/jobs?…`, no `//`, backslashes or control characters; anything else falls back to the job or list | `return_to_only_accepts_dashboard_paths`, `actions_never_redirect_off_the_dashboard` |
| Repudiation | Nobody can tell who retried or deleted what | Every action logs `jobs admin` with the action and count (`tracing`, with the request id); cancel and discard add an error entry to the job | `operator_actions_follow_the_state_rules`, `single_job_actions_post_redirect_and_confirm_when_destructive` |
| Information disclosure | Secrets in args (API keys, tokens, passwords) are shown on screen or in a screenshot | `redact` on args and meta at any depth, keyed by `SENSITIVE_KEYS` (`APP:`); the stored row keeps its value for the worker | `job_data_is_escaped_and_redacted_everywhere`, `sensitive_args_are_redacted_in_the_cli_and_admin_views` |
| Information disclosure | Error messages and backtraces reach end users | Errors are shown only on dashboard pages, behind the gate; the app's own error pages never include job data, and a dashboard query failure is the app's generic 500 page | `the_error_pages_do_not_leak_internals_in_production`, `production_without_an_admin_gate_is_404` |
| Information disclosure | Job data stays in a shared cache or comes back with Back after it was deleted | `Cache-Control: no-store` on every response (pages, fragments, redirects); `admin.js` reloads bfcache restores | `every_dashboard_response_is_no_store`, `every_dev_response_is_no_store` |
| Information disclosure | Production pages link to `/dev` tools | Development tabs are filtered out of the header outside development | `the_admin_password_gates_every_dashboard_route` |
| Denial of service | Many open dashboards overload the database | Each panel polls on its own timer (1-10 s) with `hx-sync="this:drop"`; polls stop while the tab is hidden; counts are one `GROUP BY`, lists are keyset pages of 50, metrics read at most 100,000 timestamps; query counts don't grow with data | `the_overview_and_list_queries_dont_grow_with_jobs`, `every_fragment_renders_alone` |
| Elevation of privilege | A signed-in non-admin reaches the dashboard | With accounts, `admin_from_session` returns `Some(false)` and the gate answers 404 (the hook is `APP:`; an auth sample's install fills it, with its own test) | `a_signed_in_non_admin_gets_a_404` |
| Elevation of privilege | The dashboard is reachable in development from the local network | Accepted: development is open by design (the framework's kernel-page rule), like `/dev`; the dev server is for the developer's machine. Never run `APP_ENV=development` on a shared host | `dev_routes_are_404_outside_development` |

## Invariants

| Invariant | Test |
|-----------|------|
| The dashboard links its own stylesheet and script, and both are served (the CSS-never-loads bug) | `the_dashboard_loads_its_own_css_and_js` |
| Every page, the confirmation included, follows the kernel header's theme switch (`data-theme`, `light-dark()` tokens) | `the_dashboard_follows_the_theme_switch`, `every_kernel_page_renders_data_theme` |
| Every class a dashboard template uses has a rule in `admin.css` or `jobs.css` | `every_dashboard_class_has_a_css_rule` |
| The overview shows counters, both charts, the running lane, queues with their status pills, worker success rates and nodes, with empty states; `live=0` renders no polling | `the_overview_shows_counts_charts_queues_workers_and_nodes` |
| Every live fragment renders alone; an unknown part is a 404 | `every_fragment_renders_alone` |
| The list filters by state, queue, worker, tag, error and search, shows chips and pages by id | `the_job_list_filters_searches_and_pages` |
| The job page shows args, meta, a timeline, each attempt's error and report, and only the actions its state allows | `the_job_page_shows_args_errors_per_attempt_and_timeline` |
| A deleted job's page redirects to the list with a flash | `a_deleted_jobs_page_redirects_to_the_list_with_a_flash` |
| Errors group by worker and root cause (variants listed), and their bulk retry goes through the confirmation | `the_errors_page_groups_failures_and_offers_a_bulk_retry`, `one_root_cause_is_one_group_with_its_variants` |
| The cron page shows next and last runs and runs an entry now | `the_cron_page_lists_next_runs_and_runs_an_entry_now` |
| Queues pause and resume on every node; pause-all asks first and shows a banner | `queues_pause_and_resume_and_pause_all_asks_first` |
| The demo task writes only synthetic URLs | `demo_urls_are_synthetic` |
| Chart legends summarize the window the chart shows (queue wait p95 and p50 over the last 60 minutes), "—" with no data | `chart_legends_summarize_the_whole_window` |

## In a browser (not part of the test suite)

Run with the demo data (`app task jobs_demo`, or `tools/demo.sh jobs-dashboard`), then:

- axe (WCAG 2.0/2.1/2.2 A and AA) on every screen, light and dark (the OS scheme and the switch forcing each), desktop (1440×900) and mobile (390×844): 0 violations; no console errors or CSP violations; no horizontal scroll on mobile.
- Keyboard only: Tab reaches every control with a visible focus ring; `?` opens the shortcuts dialog and Escape closes it; `j`/`k`/`x`/`Enter` work on the list.
- `prefers-reduced-motion: reduce`: the live dot, the executing badge and indeterminate progress bars stand still.

## Commands

```sh
TEST_DATABASE_URL=postgres://…/postgres tools/ci.sh jobs-dashboard   # with jobs: fmt, clippy, SQLite, Postgres, offrails-check, deny, audit
tools/security-check.sh
tools/demo.sh jobs-dashboard && (cd demo/app && ../app-bin task jobs_demo)   # then open /admin/jobs
```
