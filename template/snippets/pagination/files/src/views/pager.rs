//! The numbers for a paged list's links (the pagination snippet), built from the `Page` the handler
//! extracted; `templates/shared/_pager.html` shows them with the component kit's `ui::pagination`.
//! `Page` clamps `?page=` to 1..=`Page::MAX_PAGE` and `?per=` to 1..=100, so a huge or hostile page
//! number is an empty page with sane links, never an overflow (a 500). `?per=` is kept in the links.
//! GUIDE: .offrails/template/snippets/pagination/README.md

use crate::models::Page;

/// What `templates/shared/_pager.html` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLinks {
    /// 1-based, as clamped.
    pub number: u64,
    /// The pages known: this one, and the next when the query found more (no COUNT query).
    pub pages: u64,
    /// The links' prefix, `{base}page=n`: the list's path, `?`, and `per=n&` when it isn't the default.
    pub base: String,
}

impl PageLinks {
    /// `base`: the list's path (`paths::notes()`, no query). `more`: the query found a row past
    /// this page (the generated `all`/`all_for` return it).
    pub fn new(base: &str, page: Page, more: bool) -> Self {
        let number = page.number();
        let per = if page.per() == Page::DEFAULT_PER {
            String::new()
        } else {
            format!("per={}&", page.per())
        };
        Self {
            number,
            pages: if more && number < Page::MAX_PAGE {
                number + 1
            } else {
                number
            },
            base: format!("{base}?{per}"),
        }
    }

    /// Whether there's anywhere to go (a single page shows no links).
    pub fn is_needed(&self) -> bool {
        self.pages > 1
    }
}

#[cfg(test)]
mod tests {
    use askama::Template;

    use super::*;

    #[test]
    fn links_go_both_ways_and_keep_per() {
        let p = PageLinks::new("/notes", Page::new(2, 25), true);
        assert_eq!((p.number, p.pages, p.base.as_str()), (2, 3, "/notes?"));
        let p = PageLinks::new("/notes", Page::new(1, 10), true);
        assert_eq!((p.pages, p.base.as_str()), (2, "/notes?per=10&"));
        assert!(!PageLinks::new("/notes", Page::new(1, 25), false).is_needed());
    }

    /// `?page=18446744073709551615`: clamped, and the links stay in range.
    #[test]
    fn a_huge_page_is_clamped() {
        let p = PageLinks::new("/notes", Page::new(u64::MAX, u64::MAX), true);
        assert_eq!(p.number, Page::MAX_PAGE);
        assert_eq!(p.pages, Page::MAX_PAGE, "no page past MAX_PAGE");
        assert_eq!(p.base, "/notes?per=100&");
    }

    #[derive(Template)]
    #[template(path = "shared/_pager.html")]
    struct Probe {
        pager: PageLinks,
    }

    #[test]
    fn the_partial_renders_the_links() {
        let html = Probe {
            pager: PageLinks::new("/notes", Page::new(2, 25), true),
        }
        .render()
        .unwrap();
        assert!(html.contains("href=\"/notes?page=1\""), "{html}");
        assert!(html.contains("href=\"/notes?page=3\""), "{html}");
        assert!(html.contains("aria-current=\"page\">2<"), "{html}");
        let per = Probe {
            pager: PageLinks::new("/notes", Page::new(1, 10), true),
        }
        .render()
        .unwrap();
        // Askama escapes the `&` in an attribute as `&#38;` (the browser reads it as `&`).
        assert!(per.contains("href=\"/notes?per=10&#38;page=2\""), "{per}");
        let none = Probe {
            pager: PageLinks::new("/notes", Page::new(1, 25), false),
        }
        .render()
        .unwrap();
        assert!(!none.contains("<nav"), "{none}");
    }
}
