//! Rails helpers as plain functions. Timezone conversion and formatting happen here.

use crate::kernel::cookie;

/// `"?status=reading&q=dune"`, or `""` when there are no pairs. Empty values are skipped.
pub fn query_string(pairs: &[(&str, &str)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{}={}", cookie::encode(k), cookie::encode(v)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// `pluralize(2, "book")` → `"2 books"`.
pub fn pluralize(count: u64, singular: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {singular}s")
    }
}

// ---- the component kit's helpers (templates/components/ui.html) -------------------------------

/// `a == b` for any two displayable values (`&&str`, `&String`, `&i64`…): macro arguments arrive
/// with different levels of reference, and a `<select>`'s option values may be ids.
pub fn same(a: impl std::fmt::Display, b: impl std::fmt::Display) -> bool {
    a.to_string() == b.to_string()
}

/// The key a form control's errors are under: `key` when given, else the control's `name`.
pub fn error_key<'a, K, N>(key: &'a K, name: &'a N) -> &'a str
where
    K: AsRef<str> + ?Sized,
    N: AsRef<str> + ?Sized,
{
    let key = key.as_ref();
    if key.is_empty() { name.as_ref() } else { key }
}

/// The icon (components/_icons.html) for a tone.
pub fn tone_icon(tone: impl AsRef<str>) -> &'static str {
    match tone.as_ref() {
        "success" => "check-circle",
        "warning" => "alert",
        "danger" => "x-circle",
        _ => "info",
    }
}

/// `"Ada Lovelace"` → `"AL"`, `"ada"` → `"A"`: an avatar without a picture.
pub fn initials(name: impl AsRef<str>) -> String {
    name.as_ref()
        .split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// Any integer a template passes as a page number (by value or by reference).
pub trait PageNumber {
    fn page_number(&self) -> u64;
}
macro_rules! page_number {
    ($($t:ty),*) => {$(
        impl PageNumber for $t {
            fn page_number(&self) -> u64 {
                u64::try_from(*self).unwrap_or(0)
            }
        }
    )*};
}
page_number!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);
impl<T: PageNumber + ?Sized> PageNumber for &T {
    fn page_number(&self) -> u64 {
        (**self).page_number()
    }
}

/// The pagination component's numbers: previous, next, and a window of page links around the
/// current one with `None` for a gap (`1 … 4 5 [6] 7 8 … 20`).
pub struct Pager {
    pub page: u64,
    pub pages: u64,
    pub prev: Option<u64>,
    pub next: Option<u64>,
    pub window: Vec<Option<u64>>,
}

impl Pager {
    pub fn new(page: impl PageNumber, pages: impl PageNumber) -> Self {
        let pages = pages.page_number();
        let page = page.page_number().clamp(1, pages.max(1));
        let mut window = Vec::new();
        let mut last = 0;
        for n in 1..=pages {
            let near = n.abs_diff(page) <= 2;
            if n == 1 || n == pages || near {
                if n > last + 1 {
                    window.push(None);
                }
                window.push(Some(n));
                last = n;
            }
        }
        Self {
            page,
            pages,
            prev: (page > 1).then(|| page - 1),
            next: (page < pages).then(|| page + 1),
            window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pager_shows_the_ends_and_a_window() {
        let pages = 20_u64;
        let by_ref = &pages;
        let p = Pager::new(6_i64, by_ref);
        assert_eq!((p.prev, p.next), (Some(5), Some(7)));
        assert_eq!(
            p.window,
            vec![
                Some(1),
                None,
                Some(4),
                Some(5),
                Some(6),
                Some(7),
                Some(8),
                None,
                Some(20)
            ]
        );
        let first = Pager::new(1_u64, 3_u64);
        assert_eq!(first.prev, None);
        assert_eq!(first.window, vec![Some(1), Some(2), Some(3)]);
        let clamped = Pager::new(99_u64, 2_u64);
        assert_eq!((clamped.page, clamped.next), (2, None));
        assert!(Pager::new(1_u64, 0_u64).window.is_empty());
    }

    #[test]
    fn component_helpers() {
        let (a, owned) = ("a", String::from("a"));
        let (a_ref, owned_ref) = (&a, &owned);
        assert!(same(a_ref, owned_ref) && !same("a", "b"));
        let id = 7_i64;
        let id_ref = &id;
        assert!(
            same(id_ref, "7") && !same(id_ref, ""),
            "a select's option values may be ids"
        );
        assert_eq!(error_key("", "title"), "title");
        assert_eq!(error_key("book", "book_id"), "book");
        assert_eq!(error_key(&"", &"title"), "title");
        assert_eq!(initials("Ada  Lovelace Byron"), "AL");
        assert_eq!(initials("éa"), "É");
        assert_eq!(initials(""), "");
        assert_eq!(tone_icon("danger"), "x-circle");
        let nope = "nope";
        let nope_ref = &nope;
        assert_eq!(tone_icon(nope_ref), "info");
    }

    #[test]
    fn builds_query_strings() {
        assert_eq!(query_string(&[("status", ""), ("q", "")]), "");
        assert_eq!(
            query_string(&[("status", "reading"), ("q", "war & peace")]),
            "?status=reading&q=war%20%26%20peace"
        );
    }

    #[test]
    fn pluralizes() {
        assert_eq!(pluralize(1, "book"), "1 book");
        assert_eq!(pluralize(0, "book"), "0 books");
    }
}
