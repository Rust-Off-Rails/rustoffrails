---
name: upgrade
description: Upgrade a Rust off Rails app's vendored framework (.offrails/) to the installed plugin's version and migrate the app to any changed conventions, like `rails app:update`. Use when the user asks to upgrade or update Off Rails, or after a version-mismatch notice.
---

# /offrails:upgrade — `rails app:update`

Only for Rust off Rails apps (a `.offrails/` directory exists).

`$FW` is `${CLAUDE_PLUGIN_ROOT}` (this plugin, which is a release). Execute `${CLAUDE_PLUGIN_ROOT}/generators/upgrade.md` step by step, with that `$FW`. It is the same recipe other agents use, so keep it the single source of truth: if you find a gap, say so in your report rather than improvising silently.
