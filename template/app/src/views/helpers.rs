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

#[cfg(test)]
mod tests {
    use super::*;

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
