//! Active Record equivalents: one file per table (entity + params + queries).
//! A model never imports controllers or views.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// samples: model mods

// samples: model aliases
/// Validation errors by field, using Rails wording ("can't be blank").
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Errors(pub BTreeMap<String, Vec<String>>);

impl Errors {
    pub fn add(&mut self, field: &str, msg: &str) {
        self.0
            .entry(field.to_owned())
            .or_default()
            .push(msg.to_owned());
    }

    pub fn single(field: &str, msg: &str) -> Self {
        let mut errors = Self::default();
        errors.add(field, msg);
        errors
    }

    /// Messages for one field. Used by templates.
    pub fn on(&self, field: &str) -> &[String] {
        self.0.get(field).map_or(&[], Vec::as_slice)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `"Title can't be blank"`.
    pub fn full_messages(&self) -> Vec<String> {
        self.0
            .iter()
            .flat_map(|(field, msgs)| {
                let name = humanize(field);
                msgs.iter().map(move |m| format!("{name} {m}"))
            })
            .collect()
    }

    pub fn into_result<T>(self, ok: impl FnOnce() -> T) -> Result<T, Errors> {
        if self.is_empty() { Ok(ok()) } else { Err(self) }
    }
}

fn humanize(field: &str) -> String {
    let spaced = field.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `?page=&per=` pagination. `per` is clamped to 1..=100 (default 25), `page` to
/// 1..=`MAX_PAGE`, so a huge `?page=` is an empty page, never an overflowed offset (a 500).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Page {
    #[serde(default)]
    page: Option<u64>,
    #[serde(default)]
    per: Option<u64>,
}

impl Page {
    pub const DEFAULT_PER: u64 = 25;
    /// The last page anyone can ask for: 100 000 pages of 100 is ten million rows deep.
    pub const MAX_PAGE: u64 = 100_000;

    pub fn new(page: u64, per: u64) -> Self {
        Self {
            page: Some(page),
            per: Some(per),
        }
    }

    /// 1-based page number.
    pub fn number(&self) -> u64 {
        self.page.unwrap_or(1).clamp(1, Self::MAX_PAGE)
    }

    pub fn per(&self) -> u64 {
        self.per.unwrap_or(Self::DEFAULT_PER).clamp(1, 100)
    }

    pub fn offset(&self) -> u64 {
        (self.number() - 1) * self.per()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_messages_use_rails_wording() {
        let mut e = Errors::default();
        e.add("title", "can't be blank");
        e.add("finished_on", "is invalid");
        assert_eq!(
            e.full_messages(),
            vec!["Finished on is invalid", "Title can't be blank"]
        );
    }

    #[test]
    fn page_clamps() {
        let p = Page::new(0, 1000);
        assert_eq!((p.number(), p.per(), p.offset()), (1, 100, 0));
        assert_eq!(Page::default().per(), 25);
        assert_eq!(Page::new(3, 10).offset(), 20);
    }

    /// `?page=18446744073709551615` used to overflow `(page - 1) * per` (a panic, a 500).
    #[test]
    fn a_huge_page_is_clamped_not_overflowed() {
        let p = Page::new(u64::MAX, u64::MAX);
        assert_eq!(p.number(), Page::MAX_PAGE);
        assert_eq!(p.offset(), (Page::MAX_PAGE - 1) * 100);
        assert!(i64::try_from(p.offset()).is_ok(), "fits a SQL OFFSET");
    }
}
