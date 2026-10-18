//! The Atom feed (RFC 4287) the atom-feed snippet serves, from `templates/feed/atom.xml`. Askama
//! escapes every value (`.xml` templates escape like HTML), so a title can't break the XML.

use askama::Template;
use chrono::{DateTime, SecondsFormat, Utc};

/// One entry, as the controller loads it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub title: String,
    /// The entry's page on this site (`paths::note(id)`), made absolute with the app's URL.
    pub path: String,
    pub updated: DateTime<Utc>,
    /// Plain text (APP: a few sentences, never the whole body).
    pub summary: String,
}

/// One entry as the template shows it: every value a string.
#[derive(Debug, Clone)]
pub struct EntryView {
    pub title: String,
    pub url: String,
    pub updated: String,
    pub summary: String,
}

#[derive(Template)]
#[template(path = "feed/atom.xml")]
pub struct Feed {
    pub title: String,
    pub base: String,
    pub updated: String,
    pub entries: Vec<EntryView>,
}

impl Feed {
    /// Readers poll; a long feed is wasted bytes.
    pub const MAX: usize = 20;

    /// `base`: the app's URL (`config.app_url`), no trailing slash.
    pub fn new(title: &str, base: &str, mut entries: Vec<Entry>) -> Self {
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated));
        entries.truncate(Self::MAX);
        let stamp = |t: &DateTime<Utc>| t.to_rfc3339_opts(SecondsFormat::Secs, true);
        let base = base.trim_end_matches('/').to_owned();
        // The feed changed when its newest entry did (a fixed date when it's empty, so a reader's
        // conditional GET still works).
        let updated = entries
            .first()
            .map(|e| stamp(&e.updated))
            .unwrap_or_else(|| "2026-01-01T00:00:00Z".to_owned());
        Self {
            title: title.to_owned(),
            updated,
            entries: entries
                .iter()
                .map(|e| EntryView {
                    title: e.title.clone(),
                    url: format!("{base}{}", e.path),
                    updated: stamp(&e.updated),
                    summary: e.summary.clone(),
                })
                .collect(),
            base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, days: i64) -> Entry {
        Entry {
            title: title.into(),
            path: format!("/notes/{days}"),
            updated: DateTime::from_timestamp(1_760_000_000 + days * 86_400, 0).unwrap(),
            summary: "A summary.".into(),
        }
    }

    #[test]
    fn newest_first_absolute_links_and_rfc3339_dates() {
        let xml = Feed::new(
            "App",
            "https://example.com/",
            vec![entry("Old", 1), entry("New", 2)],
        )
        .render()
        .unwrap();
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
        assert!(xml.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"));
        assert!(xml.find("New").unwrap() < xml.find("Old").unwrap());
        assert!(xml.contains("<link href=\"https://example.com/notes/2\"/>"));
        assert!(xml.contains("<updated>2025-10-11T08:53:20Z</updated>"));
    }

    #[test]
    fn markup_in_a_title_is_escaped() {
        let xml = Feed::new(
            "App",
            "https://example.com",
            vec![entry("<b>Tom & Jerry</b>", 1)],
        )
        .render()
        .unwrap();
        assert!(
            !xml.contains("<b>") && !xml.contains("Tom & Jerry"),
            "{xml}"
        );
        // Askama writes numeric entities (`&#60;`), which XML reads like `&lt;`.
        assert!(
            xml.contains("&#60;b&#62;Tom &#38; Jerry&#60;/b&#62;"),
            "{xml}"
        );
    }

    #[test]
    fn at_most_max_entries() {
        let many: Vec<Entry> = (0..30).map(|d| entry("x", d)).collect();
        let feed = Feed::new("App", "https://example.com", many);
        assert_eq!(feed.entries.len(), Feed::MAX);
    }
}
