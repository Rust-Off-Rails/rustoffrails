# Snippet: theme-toggle

> **TL;DR**
> - A theme button in the app's nav: it shows the current theme and switches to the next (System → Light → Dark), instantly with JS, by a POST to `/theme` without it.
> - Two pieces: an app-side partial, `templates/shared/_theme_toggle.html`, and one delegated `submit` handler in `static/app.js` (CSP: no inline script; boosted pages: never bound to an element).
> - It uses the kernel's theme (`kernel/theme.rs`: the `offrails_theme` cookie, `<html data-theme>`), the same one the dev and admin pages' switch sets; a test checks the script and the server agree on the cookie.

## When to use it

- Any app with a nav: people pick light or dark once and every page follows (the cookie lasts a year).
- Not for: a per-user preference stored in the database (keep the cookie, and also save it on the account).

## Files

| Path | What it is |
|------|------------|
| `templates/shared/_theme_toggle.html` | the form and its one button (`hx-boost="false"`: the handler, not htmx, takes the submit) |
| `tests/app/theme_toggle_test.rs` | the button names the next theme, the no-JS POST sets the cookie, the script writes the cookie the server reads |

## Wiring

- `templates/shared/_nav.html` at `samples: nav links`: `{% include "shared/_theme_toggle.html" %} {# theme-toggle #}`
- `static/app.js` at `samples: app js`: the handler, under a `// theme-toggle` line
- `tests/app/main.rs` at `samples: test mods`

## After adding

The button is the kit's `btn-ghost btn-sm` (`assets/app.css`; the kit has no theme switch of its own), so no CSS rebuild; then `cargo offrails verify`, and look at it in both schemes (`run.sh a11y` checks contrast in light and dark).

## Adapting it

- An icon instead of the word: replace the button's text with an inline `<svg>` per theme (`{% if ctx.theme.as_str() == "dark" %}…`), keep the `aria-label`.
- The labels: `Theme::label` in `src/kernel/theme.rs` (the server), `labels` in the handler (the script): change both.
- Elsewhere than the nav: include the partial anywhere a `ctx` is in scope.

## Remove it

`cargo offrails snippet remove theme-toggle`, or delete the two files and the lines marked `theme-toggle` (the nav include, the block in `static/app.js`, the test mod).
