//! Newer / older links for a paged list (the pagination snippet), built from the `Page` the
//! handler extracted. `Page` clamps `?page=` to 1..=`Page::MAX_PAGE` and `?per=` to 1..=100, so a
//! huge or hostile page number is an empty page with sane links, never an overflow (a 500).
//! GUIDE: .offrails/template/snippets/pagination/README.md

use crate::models::Page;

/// What `templates/shared/_pager.html` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pager {
    /// 1-based, as clamped.
    pub number: u64,
    pub newer: Option<String>,
    pub older: Option<String>,
}

impl Pager {
    /// `base`: the list's path (`paths::notes()`, no query). `more`: the query found a row past
    /// this page (the generated `all`/`all_for` return it).
    pub fn new(base: &str, page: Page, more: bool) -> Self {
        let number = page.number();
        let per = if page.per() == Page::DEFAULT_PER {
            String::new()
        } else {
            format!("&per={}", page.per())
        };
        let link = |n: u64| format!("{base}?page={n}{per}");
        Self {
            number,
            newer: (number > 1).then(|| link(number - 1)),
            older: (more && number < Page::MAX_PAGE).then(|| link(number + 1)),
        }
    }

    /// Whether there's anywhere to go (a single page shows no pager).
    pub fn is_needed(&self) -> bool {
        self.newer.is_some() || self.older.is_some()
    }
}

#[cfg(test)]
mod tests {
    use askama::Template;

    use super::*;

    #[test]
    fn links_go_both_ways_and_keep_per() {
        let p = Pager::new("/notes", Page::new(2, 25), true);
        assert_eq!(p.newer.as_deref(), Some("/notes?page=1"));
        assert_eq!(p.older.as_deref(), Some("/notes?page=3"));
        let p = Pager::new("/notes", Page::new(1, 10), true);
        assert_eq!(p.newer, None);
        assert_eq!(p.older.as_deref(), Some("/notes?page=2&per=10"));
        assert!(!Pager::new("/notes", Page::new(1, 25), false).is_needed());
    }

    /// `?page=18446744073709551615`: clamped, and the links stay in range.
    #[test]
    fn a_huge_page_is_clamped() {
        let p = Pager::new("/notes", Page::new(u64::MAX, u64::MAX), true);
        assert_eq!(p.number, Page::MAX_PAGE);
        assert_eq!(
            p.newer,
            Some(format!("/notes?page={}&per=100", Page::MAX_PAGE - 1))
        );
        assert_eq!(p.older, None);
    }

    #[derive(Template)]
    #[template(path = "shared/_pager.html")]
    struct Probe {
        pager: Pager,
    }

    #[test]
    fn the_partial_renders_the_links() {
        let html = Probe {
            pager: Pager::new("/notes", Page::new(2, 25), true),
        }
        .render()
        .unwrap();
        assert!(html.contains("href=\"/notes?page=1\""), "{html}");
        assert!(html.contains("href=\"/notes?page=3\""), "{html}");
        assert!(html.contains("Page 2"), "{html}");
        let none = Probe {
            pager: Pager::new("/notes", Page::new(1, 25), false),
        }
        .render()
        .unwrap();
        assert!(!none.contains("<nav"), "{none}");
    }
}
