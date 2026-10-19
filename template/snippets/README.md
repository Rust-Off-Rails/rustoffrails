# Snippets

> **TL;DR**
> - A snippet is small, tested code to copy: smaller than a [part](../README.md#choosing-parts), more specific than a [generator](../../generators/README.md). Each is a folder here with a `README.md` (what, when, the files, the anchors it wires into, how to remove it), a `snippet.toml` (a part's `sample.toml` format) and `files/` at app paths.
> - `cargo offrails snippets` lists them; `cargo offrails snippet show <name>` prints the README, every file and every wire, to read or copy by hand; `cargo offrails snippet add <name> [--as <model>]` copies and wires it, recorded in `docs/generated.toml`; `cargo offrails snippet remove <name>` takes it back (unchanged files, marked lines).
> - Every snippet is CI-tested: `tools/ci/test-template.sh snippets` adds them all to a bare app and to an app with every part, runs the Definition of Done on SQLite and Postgres, then removes them and checks the app is byte-for-byte what it was.
> - Once added, the code is the app's: adapt it, and the README's "Adapting it" says where.

## The snippets

| Snippet | What it is | Needs |
|---------|------------|-------|
| [`resource`](resource/README.md) | htmx CRUD for one model (`notes`: title, body), public: exactly what `g resource notes title:string body:text --public` writes, as files. `--as article` renames it | — |
| [`atom-feed`](atom-feed/README.md) | `GET /feed.xml`: an Atom feed (escaped, RFC 3339 dates, newest 20) and its `<link rel="alternate">` on every page | — |
| [`slug`](slug/README.md) | `slugify(title)` (accents folded, 80 characters) and `unique(base, query)`: `-2`, `-3`…, unique per owner or global | — |
| [`markdown`](markdown/README.md) | `markdown::to_html`: CommonMark rendered by pulldown-cmark, sanitized by ammonia (no scripts, handlers, `javascript:`) | crates `pulldown-cmark`, `ammonia` |
| [`pagination`](pagination/README.md) | `PageLinks::new(base, page, more)` and `shared/_pager.html` (the kit's `ui::pagination`); `?page=` clamped, so a huge one is never a 500 | — |
| [`theme-toggle`](theme-toggle/README.md) | a System/Light/Dark button in the nav: an app-side partial and one delegated handler in `static/app.js` | — |
| [`seed-backdated`](seed-backdated/README.md) | seeds with a history: create through the model, then `backdate::<E>(db, id, days_ago(30))` | nothing (any generated table, owner-scoped or public) |
| [`public-show-by-slug`](public-show-by-slug/README.md) | `GET /read/notes/{slug}`: a public page per record by slug, a route with a path parameter end to end | the `resource` and `slug` snippets (not a generated owner-scoped resource: [why](public-show-by-slug/README.md)) |

## Snippet, generator, part, or by hand?

- **A part** when it's kernel: mail, a job queue, sign-in, sockets, the dashboards (`cargo offrails parts`).
- **A generator** when the shape is standard and the names are yours: a CRUD resource for your fields, a model, a JSON API, a job, a mailer, a page with a path param (`cargo offrails g --help`).
- **A snippet** when the code is a recipe you'd otherwise look up: a feed, slugs, Markdown, a pager, a theme button, backdated seeds. Read it with `snippet show` even when you don't add it.
- **By hand**, from the guides, for everything else, and whenever starting from a template would be more editing than writing.

All of them write the same way: small files in the feature's own folders, and one line per spot in a shared file, marked with the owner (`// atom-feed`, `{# theme-toggle #}`), so a human, `snippet remove` or `destroy` can find and remove it.

## The format

```
template/snippets/<name>/
  README.md        # TL;DR, When to use it, Files, Wiring, After adding, Adapting it, Remove it
  snippet.toml     # name, version, summary, depends_on (parts), crates, [[wire]] (file, anchor, text)
                   #   + as = "<model>" (what --as renames), snippets = [ … ] (snippets it builds on)
  files/           # at app paths; `offrails_app` becomes the app's crate name
```

`snippet add` checks everything first (every anchor there, no file in the way, the parts and snippets it needs) and changes nothing on a refusal. It is a recorded generation (kind `snippet`), so re-adding is a no-op and `snippet remove` (= `cargo offrails destroy snippet <name>`) deletes the files nobody changed and the marked lines. The crates it added stay (`cargo remove` them if nothing else uses them). `--as <model>` renames the model a snippet is written for, on word boundaries (`note`, `notes`, `Note`, `Notes`: `NoteParams`, `/notes/{id}`, "Note was saved"), one-word names only; add a snippet that builds on another `--as` the same model.

To add a snippet to the framework: a folder here, files that compile in a bare app and in one with every part, a test of what it promises (the README cites it), and a line in `SNIPPETS` in `tools/ci/test-template.sh` (the CI refuses a snippet it doesn't run).
