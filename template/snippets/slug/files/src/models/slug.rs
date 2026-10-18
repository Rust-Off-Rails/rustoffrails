//! Slugs (the slug snippet): `slugify("Café, déjà vu!")` is `"cafe-deja-vu"`, and `unique` adds
//! `-2`, `-3`… until your query says the slug is free. Scope that query to the owner and slugs are
//! unique per owner (two users may both have `/hello-world`); leave it global for a public URL.
//! GUIDE: .offrails/template/snippets/slug/README.md

use std::future::Future;

/// The longest slug, suffix included: URLs stay readable, and it fits any `string` column.
pub const MAX_LEN: usize = 80;

/// What a title with no letters or digits becomes. APP: your fallback.
pub const FALLBACK: &str = "untitled";

/// Lowercase ASCII words joined by `-`: accents folded (é → e, ß → ss), everything else a separator.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.chars() {
        let folded = fold(c);
        if folded.is_empty() {
            gap = !out.is_empty();
            continue;
        }
        for f in folded.chars() {
            if gap {
                out.push('-');
                gap = false;
            }
            out.push(f);
        }
    }
    let cut = truncate(&out, MAX_LEN);
    if cut.is_empty() {
        FALLBACK.to_owned()
    } else {
        cut
    }
}

/// `base`, or `base-2`, `base-3`…: the first one `taken` says is free (after 50 tries, a random
/// suffix). `taken` runs your query: `Ok(true)` when a row already has that slug.
pub async fn unique<F, Fut, E>(base: &str, mut taken: F) -> Result<String, E>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<bool, E>>,
{
    let base = if base.is_empty() { FALLBACK } else { base };
    let first = truncate(base, MAX_LEN);
    if !taken(first.clone()).await? {
        return Ok(first);
    }
    for n in 2..=50 {
        let candidate = with_suffix(base, &n.to_string());
        if !taken(candidate.clone()).await? {
            return Ok(candidate);
        }
    }
    loop {
        let random = uuid::Uuid::new_v4().simple().to_string();
        let candidate = with_suffix(base, &random[..8]);
        if !taken(candidate.clone()).await? {
            return Ok(candidate);
        }
    }
}

fn with_suffix(base: &str, suffix: &str) -> String {
    let room = MAX_LEN.saturating_sub(suffix.len() + 1);
    format!("{}-{suffix}", truncate(base, room))
}

/// At most `max` bytes (slugs are ASCII), never ending in `-`.
fn truncate(slug: &str, max: usize) -> String {
    let mut s: String = slug.chars().take(max).collect();
    while s.ends_with('-') {
        s.pop();
    }
    s
}

/// A character as slug letters: ASCII alphanumerics lowercased, common accented Latin letters
/// folded, anything else nothing (a word break).
fn fold(c: char) -> &'static str {
    const ASCII: [&str; 36] = [
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f", "g", "h",
        "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    if c.is_ascii_digit() {
        return ASCII[(c as u8 - b'0') as usize];
    }
    if c.is_ascii_alphabetic() {
        return ASCII[(c.to_ascii_lowercase() as u8 - b'a') as usize + 10];
    }
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => "a",
        'æ' | 'Æ' => "ae",
        'ç' | 'Ç' => "c",
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => "i",
        'ñ' | 'Ñ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => "o",
        'œ' | 'Œ' => "oe",
        'ß' => "ss",
        'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => "u",
        'ý' | 'ÿ' | 'Ý' => "y",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::convert::Infallible;

    use super::*;

    #[test]
    fn titles_become_slugs() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Café, déjà vu…  "), "cafe-deja-vu");
        assert_eq!(slugify("Straße & Œuvre"), "strasse-oeuvre");
        assert_eq!(slugify("Rust 2026: what's new?"), "rust-2026-what-s-new");
        assert_eq!(slugify("¡¿!?"), FALLBACK);
        assert_eq!(slugify("日本語"), FALLBACK);
        let long = slugify(&"word ".repeat(40));
        assert!(long.len() <= MAX_LEN && !long.ends_with('-'), "{long}");
    }

    #[tokio::test]
    async fn unique_counts_up_until_the_query_says_free() {
        let taken: HashSet<String> = ["hello", "hello-2"].iter().map(|s| s.to_string()).collect();
        let slug = unique("hello", |s| {
            let hit = taken.contains(&s);
            async move { Ok::<_, Infallible>(hit) }
        })
        .await
        .unwrap();
        assert_eq!(slug, "hello-3");
        let free = unique("other", |_| async { Ok::<_, Infallible>(false) })
            .await
            .unwrap();
        assert_eq!(free, "other");
    }

    #[tokio::test]
    async fn a_suffix_never_makes_a_slug_too_long() {
        let base = "x".repeat(MAX_LEN);
        let mut calls = 0;
        let slug = unique(&base, |_| {
            calls += 1;
            let hit = calls < 3;
            async move { Ok::<_, Infallible>(hit) }
        })
        .await
        .unwrap();
        assert!(slug.ends_with("-3") && slug.len() <= MAX_LEN, "{slug}");
    }
}
