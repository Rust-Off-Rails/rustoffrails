---
name: upgrade
description: Upgrade a Rust off Rails app's vendored framework (.offrails/) to the installed plugin's version and migrate the app to any changed conventions, like `rails app:update`. Use when the user asks to upgrade or update Off Rails, or after a version-mismatch notice.
---

# /offrails:upgrade — `rails app:update`

Only for Rust off Rails apps (a `.offrails/` directory exists). The new framework is at `${CLAUDE_PLUGIN_ROOT}` (`$FW`).

1. **Detect drift by content, not just version.** Run `bash "$FW/hooks/fingerprint.sh" .offrails` and `bash "$FW/hooks/fingerprint.sh" "$FW"`. If the fingerprints match, the app is current: say so and stop. Otherwise note both versions (the first line of `.offrails/VERSION` and of `$FW/VERSION`). Equal versions with different content still need an upgrade, because docs can change without a version bump.
2. Require a clean git tree. If it isn't clean, ask the human to commit first, or commit for them if they agree.
3. Read `$FW/CHANGELOG.md` from the app's version up to the new one, including any `Unreleased` section. Diff the old docs against the new ones (`diff -ru .offrails "$FW"`, ignoring `.git/`, `.claude-plugin/`, `skills/`, `hooks/`, `evals/`, `brand/`) to catch anything the changelog doesn't mention.
4. **Reconcile, don't re-create.** For every change, first look for an equivalent the app already has, often built early from older docs or by hand: a table, migration, kernel module, route, template or test. Classify each change:
   - **new:** absent. Build it per the new docs.
   - **present, conforming:** matches the new spec. Nothing to do, though you may add missing tests.
   - **present, divergent:** exists in another shape, so reconcile it in place. **Never** re-create or edit an applied migration. Write a new migration that alters, renames, backfills or copies data, keeps existing rows, and has a working `down`. Rename modules, routes and templates to the spec's names, with redirects for any URL that moved.
   - **conflicting:** the app's version encodes a product decision (check `docs/decisions.md` and `product/`). Keep it, record it as a deviation, and ask the human only if it blocks the upgrade.
5. Show the human a plan: each change with its classification → what it means for this app (files affected, migrations, risk). Wait for a yes.
6. Replace `.offrails/` with a fresh copy of the **release** (the `rsync` in `generators/new.md` step 1, with its anchored excludes and `--delete`; from a git checkout, `git archive v<version>` first, so unreleased commits never leak into a versioned app) and write `.offrails/VERSION` as in `generators/new.md` step 1 (framework version, then `vendored <date>`).
7. Apply the plan in vertical slices, each green under `/offrails:check`. Log `YYYY-MM-DD: upgraded Off Rails <old> → <new>` and each deviation in `docs/decisions.md`.
8. **Verify nothing shows a stale framework version.** Grep the app (templates, views, `web/src`, docs, tests) for the old version string. Every place that displays or embeds the framework version must read it from `.offrails/VERSION` at boot or build time, never a literal. Run the app's own post-upgrade checks, if its `AGENTS.md` lists any under "App-specific notes".
9. Report what changed, in product terms where it matters.
