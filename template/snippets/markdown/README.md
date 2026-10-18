# Snippet: markdown

> **TL;DR**
> - `views::markdown::to_html(md)`: Markdown in, HTML a template may show unescaped out. pulldown-cmark renders it (CommonMark, tables, strikethrough), ammonia sanitizes it (no scripts, event handlers, `javascript:` URLs, iframes or styles; links get `rel="noopener noreferrer nofollow ugc"`).
> - Render where the page struct is built, never in the template: `body_html: markdown::to_html(&post.body)`, then `{{ body_html|safe }}`.
> - Adds two crates, `pulldown-cmark` and `ammonia` (`cargo add`, versions from Cargo.lock).

## When to use it

- Posts, comments, descriptions, docs: text people format.
- Not for: one-line fields (titles stay text, escaped by Askama), or HTML you wrote yourself (a template).

## Files

| Path | What it is |
|------|------------|
| `src/views/markdown.rs` | `to_html`, `LINK_REL`, with unit tests (rendering, the XSS vectors removed, links kept) |

## Wiring

- `src/views/mod.rs` at `samples: view mods`: `pub mod markdown; // markdown`
- `cargo add pulldown-cmark ammonia`

## After adding

1. In the view struct (`src/views/<plural>.rs`), add `pub body_html: String` and fill it with `markdown::to_html(&r.body)` in `from_model`.
2. In the template: `<div class="prose dark:prose-invert">{{ body_html|safe }}</div>` (or your own styles for `h1`, `ul`, `code`…).
3. A request test that markup in the body is gone from the page (`assert_not_contains("<script>alert(1)")`), citing your AC. `cargo offrails verify`.

## Adapting it

- More syntax (footnotes, task lists): add the `Options::ENABLE_*` flag, then check what ammonia keeps (it drops `<input>`, so task-list checkboxes vanish unless you allow them).
- Allow more tags or attributes (`class` on `code` for syntax highlighting): `.add_tag_attributes("code", &["class"])` on the builder. Each one you allow is attack surface: add its XSS case to `scripts_handlers_and_javascript_urls_are_removed`.
- Long bodies on hot pages: render once on save into a `body_html` column instead of on every view.

## Remove it

`cargo offrails snippet remove markdown` (then `cargo remove pulldown-cmark ammonia` if nothing else uses them), or delete `src/views/markdown.rs` and the line marked `// markdown`.
