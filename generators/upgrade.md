# Generator: `upgrade` (`rails app:update`)

> **TL;DR**
> - Brings an app to a newer **release**: `$FW` is `~/.offrails/current`, the plugin root, or a tag export, never a moving working tree.
> - Detect drift by content fingerprint; start from a clean git tree; read the CHANGELOG's App migration steps between versions.
> - Reconcile, don't re-create: classify each change (new / conforming / divergent / conflicting); never edit applied migrations; show the plan and wait for a yes.
> - Re-vendor, apply in green vertical slices, verify nothing shows a stale version, and log the upgrade in `docs/decisions.md`.
>
> Read it all whenever you upgrade an app (`offrails upgrade` launches this recipe).

Brings an existing app up to a newer Rust off Rails release: it re-vendors `.offrails/` and migrates the app to any changed conventions. Any agent can run this recipe (Claude Code, Codex, opencode, Cursor…). In Claude Code, `/offrails:upgrade` runs it for you.

**`$FW`** is the new framework:
- after the one-line install: `~/.offrails/current` (run `offrails update` first to get the latest release);
- in Claude Code: the plugin root;
- otherwise: a checkout of the framework at a release tag (`git -C <checkout> archive v<version> | tar -x -C <tmp>`). **Never vendor a moving working tree:** unreleased commits would leak into a versioned app.

Run it from the app's root (the directory with `.offrails/`).

## 1. Is there anything to upgrade?

Compare content, not just version numbers: docs can change without a bump.

```sh
bash "$FW/hooks/fingerprint.sh" .offrails
bash "$FW/hooks/fingerprint.sh" "$FW"
head -n1 .offrails/VERSION "$FW/VERSION"
```

**An installed `offrails` older than 0.6.0 silently ignores `--fw`** (and every other flag): `offrails upgrade --check --fw "$FW"` then compares against `~/.offrails/current` instead and can report "current" for the wrong release. Run the target release's own script, `sh "$FW/tools/offrails" upgrade --check --fw "$FW"`, or `offrails update` first (`offrails version` prints the installed release). From 0.6.1 the CLI rejects unknown flags with exit 2 and answers `-h`/`--help` on every subcommand without doing anything else.

If the fingerprints match, the app is current: say so and stop. (`offrails upgrade --check` does exactly this against `~/.offrails/current`; `offrails upgrade --check --fw "$FW"` against any other release, e.g. a tag export: `git -C <framework checkout> archive v0.6.0 | tar -x -C /tmp/offrails-0.6.0`. Exit 0 = current, 10 = an upgrade is due. `--check` never starts an agent; plain `offrails upgrade` from a script or another agent needs `--yes`, else it prints the agent command and exits 3.)

## 2. Start clean

Require a clean git tree. If it isn't clean, ask the human to commit first, or commit for them if they agree.

**Preconditions for the proofs.** The App migration steps prove UI changes with the framework's browser smokes (`.offrails/tools/browser-smoke/run.sh`), which need **Node ≥ 22.12** (Node 24 LTS recommended; puppeteer-core 25 refuses older) and an installed Chrome. Check `node --version` now. Too old or missing: say so in the plan, and run the smokes once a Node that qualifies is on `PATH` (`run.sh` refuses older with that message). Postgres for `TEST_DATABASE_URL` is the other precondition ([workflow](../workflow.md#definition-of-done)).

## 3. Read what changed

Read `$FW/CHANGELOG.md` from the app's version up to the new one. Each release lists **App migration** steps written for an agent, in reconcile style (detect → final shape → converge). Then diff the docs (`diff -ru .offrails "$FW" -x .git -x .claude-plugin -x skills -x hooks -x evals -x brand -x .in_use`) to catch anything the changelog doesn't mention.

## 4. Reconcile, don't re-create

For every change, first look for an equivalent the app already has, often built early from older docs or by hand: a table, migration, kernel module, route, template or test. Classify each change:

- **new:** absent. Build it per the new docs.
- **present, conforming:** matches the new spec. Nothing to do, though you may add missing tests.
- **present, divergent:** exists in another shape, so reconcile it in place. **Never** re-create or edit an applied migration. Write a new migration that alters, renames, backfills or copies data, keeps existing rows, and has a working `down`. Rename modules, routes and templates to the spec's names, with redirects for any URL that moved.
- **conflicting:** the app's version encodes a product decision (check `docs/decisions.md` and `product/`). Keep it, record it as a deviation, and ask the human only if it blocks the upgrade.

## 5. Show the plan, wait for a yes

List each change with its classification and what it means for this app: files affected, migrations, risk. Wait for the human's yes.

Non-interactive runs (a headless agent, or a request relayed by another agent): proceed only if the human's request explicitly asked for the upgrade and nothing in the plan is outward-facing (no deploys, no production data). Write the plan to `docs/decisions.md` first, so the human can review what was decided.

## 6. Re-vendor the framework

Replace `.offrails/` with the release, using the same anchored `rsync` as [`new.md` step 1](new.md#1-create-the-project) (with `--delete`), and write `.offrails/VERSION` (framework version, then `vendored <date>`).

## 7. Apply in vertical slices

Each slice must be green under the [Definition of Done](../workflow.md#definition-of-done) before the next one.

**A step applies only where its *Detect* matches.** Many steps are for one kind of app: accounts, live regions, a development pending-migrations page, a CSS banner, channels. Run the detect first; a step for a feature the app doesn't have is **n/a**, listed as such in the plan with the reason, not built "to match". Where a step's proof needs something the app lacks, use the alternative it names (no accounts: `run.sh a11y` instead of `run.sh login`; its preflight still fails on an untrusted certificate). A detect that would match every app for a change that only some need is a changelog bug: apply what clearly applies, and say so in your report. Log `YYYY-MM-DD: upgraded Off Rails <old> → <new>` and each deviation in `docs/decisions.md`.

## 8. Verify nothing is stale

- `sh .offrails/tools/offrails-check` exits 0. Its `FAIL` lines are convergence work for this upgrade, not pre-existing debt to report and leave. Two that older specs and configs hit:
  - **`n/a` threat rows.** *Detect:* `grep -nE '^\|[^|]*\|[[:space:]]*(n/a|N/A|none|—)[[:space:]]*\|' product/features/*.md`, or `[security] … says "n/a" with no reason` / `names no test`. *Converge:* a STRIDE category that can't apply becomes `| <Category> | n/a: <why it can't happen here> | — | — |`; a row that names a real threat gets its test written and named, never `—`.
  - **`.env.example` gaps.** *Detect:* `[env] .env.example is missing variable(s)` (the old DoD grep only matched `var("…")`, so variables read through `parse("X", …)` or another helper were never checked). *Converge:* add each as `NAME=value` or `# NAME=value` with a comment ([config](../guides/config.md#envexample)); a `"SCREAMING_SNAKE"` literal in `src/config.rs` that isn't an env var gets a trailing `// not an env var`.
- **Reconcile `docs/decisions.md`.** Read every line. For each decision this release makes obsolete (a workaround for a bug the release fixes, a deviation the framework now adopts, a limit the release resizes), add a line that supersedes it by date (`YYYY-MM-DD: supersedes 2026-10-04 (stall watchdog): 0.6.0 serves HTTP/2, removed the watchdog`) and remove the code it justified in the same slice. Never edit or delete the old line ([workflow](../workflow.md#decisions)). Keep a workaround only if you can still reproduce its evidence on the new release, and say so in the superseding line.
- Copy the app header's `NEVER` list from [`blueprints/AGENTS.md`](../blueprints/AGENTS.md) into the app's `AGENTS.md` when it's missing or older.

- Grep the app (templates, views, `web/src`, docs, tests) for the old version string. Anything that displays the framework version must read `.offrails/VERSION`, never a literal.
- Run the app's own post-upgrade checks (its `AGENTS.md`, "Post-upgrade checks"). If the app's `AGENTS.md` predates that section, add it from [`blueprints/AGENTS.md`](../blueprints/AGENTS.md) and list anything this upgrade taught you to verify next time.
- `bash "$FW/hooks/fingerprint.sh" .offrails` now equals the framework's.

## 9. Report

What changed, in product terms where it matters, plus the deviations you kept.
