# Generator: `upgrade` (`rails app:update`)

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

If the fingerprints match, the app is current: say so and stop. (`offrails upgrade --check` does exactly this.)

## 2. Start clean

Require a clean git tree. If it isn't clean, ask the human to commit first, or commit for them if they agree.

## 3. Read what changed

Read `$FW/CHANGELOG.md` from the app's version up to the new one. Each release lists **App migration** steps written for an agent, in reconcile style (detect → final shape → converge). Then diff the docs (`diff -ru .offrails "$FW" -x .git -x .claude-plugin -x skills -x hooks -x evals -x brand`) to catch anything the changelog doesn't mention.

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

Each slice must be green under the [Definition of Done](../workflow.md#definition-of-done) before the next one. Log `YYYY-MM-DD: upgraded Off Rails <old> → <new>` and each deviation in `docs/decisions.md`.

## 8. Verify nothing is stale

- Grep the app (templates, views, `web/src`, docs, tests) for the old version string. Anything that displays the framework version must read `.offrails/VERSION`, never a literal.
- Run the app's own post-upgrade checks (its `AGENTS.md`, "Post-upgrade checks").
- `bash "$FW/hooks/fingerprint.sh" .offrails` now equals the framework's.

## 9. Report

What changed, in product terms where it matters, plus the deviations you kept.
