//! Markdown to HTML a page may show unescaped (the markdown snippet): pulldown-cmark renders
//! CommonMark (plus tables and ~~strikethrough~~), then ammonia keeps only safe tags and
//! attributes (no `<script>`, no `on*=`, no `javascript:` URLs; links get `rel="… nofollow ugc"`).
//! SECURITY: only `to_html`'s output may reach a template's `|safe`; never the raw input.
//! GUIDE: .offrails/template/snippets/markdown/README.md

use pulldown_cmark::{Options, Parser, html};

/// What every link in user text gets: no `window.opener`, no referrer, no search-ranking credit.
pub const LINK_REL: &str = "noopener noreferrer nofollow ugc";

/// `md` as sanitized HTML, ready for `{{ body_html|safe }}`.
pub fn to_html(md: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let mut raw = String::with_capacity(md.len() * 3 / 2);
    html::push_html(&mut raw, Parser::new_ext(md, options));
    ammonia::Builder::default()
        .link_rel(Some(LINK_REL))
        .clean(&raw)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_renders() {
        let html = to_html("# Title\n\nSome **bold** and _soft_ text, `code`.\n\n- one\n- two\n");
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(html.contains("<em>soft</em>"), "{html}");
        assert!(html.contains("<code>code</code>"), "{html}");
        assert!(html.contains("<li>two</li>"), "{html}");
        let table = to_html("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(
            table.contains("<table>") && table.contains("<td>2</td>"),
            "{table}"
        );
    }

    #[test]
    fn scripts_handlers_and_javascript_urls_are_removed() {
        for evil in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "[click](javascript:alert(1))",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<iframe src=\"https://evil.example\"></iframe>",
            "<div style=\"position:fixed\" onclick=\"alert(1)\">x</div>",
        ] {
            let html = to_html(evil).to_ascii_lowercase();
            for bad in [
                "<script",
                "onerror",
                "javascript:",
                "<iframe",
                "onclick",
                "style=",
            ] {
                assert!(!html.contains(bad), "{evil:?} kept {bad:?}: {html}");
            }
        }
    }

    #[test]
    fn links_are_kept_and_marked_as_user_content() {
        let html = to_html("[docs](https://example.com/docs)");
        assert!(html.contains("href=\"https://example.com/docs\""), "{html}");
        assert!(html.contains(&format!("rel=\"{LINK_REL}\"")), "{html}");
    }
}
