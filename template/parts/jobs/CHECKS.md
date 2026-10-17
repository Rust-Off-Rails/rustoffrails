# jobs: checks

What "working and safe" means for the job queue, and the test that proves each item. `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh jobs` runs them all on SQLite and Postgres.

The queue has no HTTP surface of its own (the [jobs-dashboard](../jobs-dashboard/CHECKS.md) sample adds one). Its attack surface is what flows through it: arguments and errors written by the app (and, through it, by users), the CLI operators run, and the database rows other nodes share.

## Threat model

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | A row with a forged worker name or args runs code it shouldn't | Dispatch is an explicit `match` on registered names; unknown names are discarded; args decode only into the worker's typed struct, and undecodable args are discarded, never guessed | `unknown_workers_and_bad_args_are_discarded`, `every_registered_worker_dispatches` |
| Tampering | Two nodes run the same job (double charge, double email) | One transaction: `SELECT … FOR UPDATE SKIP LOCKED` (Postgres) or under SQLite's write lock (`BEGIN IMMEDIATE`), then `UPDATE … WHERE state = 'available' RETURNING` | `concurrent_claims_run_each_job_exactly_once`, `racing_claimers_get_disjoint_jobs`, `concurrent_claims_on_a_sqlite_file_database_never_overlap`, `two_workers_never_run_the_same_job` |
| Tampering | A slow node's late result overwrites a job an operator cancelled or the Lifeline rescued | Every result update is filtered by `state = 'executing' AND attempted_by = node`; a lost update records nothing | `a_result_never_resurrects_a_cancelled_job` |
| Tampering | Search or filter input becomes SQL (`app jobs list --q "'; DROP TABLE jobs; --"`) | Every filter is a bound parameter built by SeaORM/sea-query; `LIKE` wildcards are escaped | `filters_and_search_are_parameterized_against_injection` |
| Tampering | A row with an invalid state or priority is written by hand or by a bug | `CHECK (state IN (…))` and `CHECK (priority BETWEEN 0 AND 9)` in the migration | `the_state_column_rejects_unknown_states` |
| Repudiation | Nobody can tell what happened to a job | Every attempt's error, node, time and duration stays on the row; operator actions add an error entry and log `jobs admin` with a count | `failures_retry_with_backoff_then_discard_at_max_attempts`, `operator_actions_follow_the_state_rules` |
| Information disclosure | Secrets in args (tokens, passwords, API keys) are printed by the CLI or shown on the dashboard | Rule: no secrets in args. Defence in depth: `redact` replaces values of sensitive keys at any depth in every printed view (`SENSITIVE_KEYS`) | `sensitive_args_are_redacted_in_the_cli_and_admin_views`, `redaction_reaches_nested_values`, `sensitive_keys_are_recognised` |
| Information disclosure | Search becomes an oracle for redacted values (`q=sk_live_a`, then `sk_live_ab`, …: a hit count reveals the secret) | `q` matches the id, the worker and queue names, and args only through their redacted copy (SQL narrows on the raw text, the redacted JSON decides), in lists, counts and bulk actions alike | `search_never_matches_redacted_values` |
| Information disclosure | A failure message quotes the URL that failed, with its `user:password@` or `?token=` (stored, shown, searched, logged) | Errors go through the host's `kernel::redact::text` before they are stored or logged (message and full report) | `job_errors_are_stored_redacted`, `secrets_in_free_text_are_redacted` |
| Information disclosure | Node names reveal hostnames | Synthetic `node-<hex>` names, or `APP_JOBS_NODE`; never the hostname | `node_ids_are_synthetic_and_unique`, `the_inline_worker_runs_jobs_inside_serve` |
| Information disclosure | Error reports grow without bound or carry huge payloads | Each error capped at 4 KB (message and detail), the last 20 kept | `errors_are_capped_in_size_and_count` |
| Denial of service | A job hangs forever and holds a slot | Per-worker `TIMEOUT`; the future is dropped and the attempt fails with "timed out after …" | `cancel_discard_panic_and_timeout_outcomes` |
| Denial of service | A panicking worker kills the producer | `catch_unwind` around every job; a panic is a retryable error | `cancel_discard_panic_and_timeout_outcomes` |
| Denial of service | A job that crashes its node (OOM) takes every node down in turn | The Lifeline counts the attempt: after `max_attempts` the orphan is discarded | `the_lifeline_rescues_only_dead_nodes_jobs` |
| Denial of service | A burst of calls to a partner API gets the app banned | Rate-limited queues, counted globally inside the claim transaction | `rate_limited_queues_start_at_most_n_per_period` |
| Denial of service | `--all` on an empty filter touches every job | Bulk actions refuse an empty filter, cap at 10,000, and the CLI asks first | `bulk_actions_refuse_an_empty_filter`, `bulk_commands_need_a_filter_and_a_confirmation` |
| Elevation of privilege | n/a: the queue has no users or roles; the CLI runs with the operator's own database access, and HTTP access belongs to the dashboard sample | — | — |

## Invariants

| Invariant | Test |
|-----------|------|
| A job runs, records `completed` with its timestamps and node | `a_job_runs_and_completes` |
| `Err` retries with exponential backoff, keeps the error chain and full report per attempt, and discards at `max_attempts` | `failures_retry_with_backoff_then_discard_at_max_attempts`, `backoff_grows_exponentially_with_bounded_jitter` |
| `Snooze` reschedules and gives the attempt back | `snooze_gives_the_attempt_back` |
| `Cancel`, `Discard`, panics and timeouts end in the right state with the right error | `cancel_discard_panic_and_timeout_outcomes` |
| Args that don't serialize insert nothing | `args_that_fail_to_serialize_insert_nothing` |
| A job enqueued in a rolled-back transaction never exists | `jobs_enqueued_in_a_rolled_back_transaction_never_exist` |
| Unique jobs insert once, even concurrently; the key frees at a final state unless `UniqueStates::All` | `unique_jobs_insert_once_even_concurrently`, `uniqueness_by_args_keys_worker_and_period`, `unique_keys_are_canonical_and_scoped`, `periods_bucket_time` |
| `insert_all` inserts thousands in chunks and refuses unique jobs | `insert_all_inserts_in_bulk_and_refuses_unique_jobs` |
| Priority 0 runs first; scheduled jobs wait for their time; queues are claimed separately | `priorities_run_first_and_scheduled_jobs_wait`, `queues_are_claimed_separately` |
| A rate-limited queue never starts more than `allowed` jobs per period across all nodes | `rate_limited_queues_start_at_most_n_per_period` |
| A batch's callback is enqueued once, after its last job is final (worker or operator) | `a_batch_enqueues_its_callback_once_when_every_job_is_final` |
| Only dead nodes' jobs are rescued; a crashed node's job runs again elsewhere | `the_lifeline_rescues_only_dead_nodes_jobs`, `a_crashed_nodes_job_runs_again_elsewhere`, `a_dead_nodes_job_is_rescued_and_run_by_the_next_worker` |
| Graceful shutdown hands unfinished jobs back at once and gives back the attempt | `shutdown_returns_unfinished_jobs_to_the_queue` |
| Cancelling an executing job aborts its future within a heartbeat | `cancelling_an_executing_job_aborts_it` |
| A paused queue isn't fetched until resumed, on every node | `a_paused_queue_is_not_fetched_until_resumed` |
| Progress is visible while a job runs | `progress_shows_while_a_job_runs` |
| The Pruner deletes only old final jobs | `the_pruner_keeps_recent_and_unfinished_jobs` |
| Cron fires once per due time across nodes, skips stale due times, and runs on demand; expressions are five fields, UTC | `cron_fires_once_across_concurrent_ticks`, `cron_skips_stale_due_times_and_runs_on_demand`, `the_worker_fires_cron_entries`, `cron_expressions_are_utc_and_five_fields`, `every_schedule_entry_parses_and_has_a_next_run` |
| A worker deregisters on shutdown; the inline worker runs inside `serve` under `APP_JOBS_NODE` | `a_worker_runs_jobs_and_deregisters_on_shutdown`, `the_inline_worker_runs_jobs_inside_serve` |
| Every schedule entry enqueues a registered worker | `keys_are_unique_and_jobs_are_registered` |
| Operator actions follow the state rules and keep the error history | `operator_actions_follow_the_state_rules`, `actions_parse_and_know_their_states` |
| Stats, queues, throughput, latency percentiles, worker success rates and error groups are correct | `stats_queues_metrics_and_error_groups`, `percentiles_use_nearest_rank`, `error_signatures_group_by_shape` |
| One root cause (innermost error + host, numbers ignored) is one error group, whatever the outer context; the contexts are its variants | `one_root_cause_is_one_group_with_its_variants`, `root_causes_hosts_and_variants` |
| Listing pages by id and searches ids, names, redacted args and errors | `listing_pages_by_id_and_searches_ids_and_errors`, `search_never_matches_redacted_values` |
| `APP_QUEUES` parses limits and rates and rejects anything else | `queue_specs_parse_limits_and_rates` |
| The CLI prints tables and JSON, and its bulk commands need a filter and a yes | `stats_list_and_show_print_tables_and_json`, `bulk_commands_need_a_filter_and_a_confirmation`, `queues_cron_and_prune_commands` |
| The example workers behave as documented | `the_example_workers_behave_as_documented` |

## Commands

```sh
TEST_DATABASE_URL=postgres://…/postgres tools/ci.sh jobs     # alone: fmt, clippy (+ unwrap/expect gate), SQLite, Postgres, offrails-check, deny, audit
TEST_DATABASE_URL=postgres://…/postgres tools/ci.sh all      # with every other sample
tools/security-check.sh                                      # every test named here and in SECURITY/INVARIANT markers exists
cargo nextest run jobs_test                                  # in an app: just these tests
```
