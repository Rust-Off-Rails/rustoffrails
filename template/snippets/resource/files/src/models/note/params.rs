//! Note input: `NoteParams` (untrusted, all strings) → `validate()` → `NewNote` (typed).
//! From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).

use serde::Deserialize;

use super::Model;
use crate::models::Errors;

/// Untrusted input from the note form. This struct IS the permit list. All strings, so a
/// failed submit re-renders exactly what was typed.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct NoteParams {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// Validated, typed values. Only `NoteParams::validate` makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct NewNote {
    pub title: String,
    pub body: String,
}

impl NoteParams {
    /// Rails wording: "can't be blank", "is too long (maximum is N characters)".
    pub fn validate(&self) -> Result<NewNote, Errors> {
        let mut errors = Errors::default();
        let title = self.title.trim();
        if title.is_empty() {
            errors.add("title", "can't be blank");
        } else if title.chars().count() > 255 {
            errors.add("title", "is too long (maximum is 255 characters)");
        }
        let body = self.body.trim();
        if body.is_empty() {
            errors.add("body", "can't be blank");
        } else if body.chars().count() > 100000 {
            errors.add("body", "is too long (maximum is 100000 characters)");
        }
        errors.into_result(|| NewNote {
            title: title.to_owned(),
            body: body.to_owned(),
        })
    }
}

/// Prefills the edit form.
impl From<&Model> for NoteParams {
    fn from(r: &Model) -> Self {
        Self {
            title: r.title.clone(),
            body: r.body.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> NoteParams {
        NoteParams {
            title: "Title".into(),
            body: "Body text".into(),
        }
    }

    #[test]
    fn valid_params_validate() {
        assert!(valid().validate().is_ok(), "{:?}", valid().validate());
    }

    #[test]
    fn required_fields_cant_be_blank() {
        let errors = NoteParams::default().validate().unwrap_err();
        assert_eq!(errors.on("title"), ["can't be blank"]);
        assert_eq!(errors.on("body"), ["can't be blank"]);
    }

    #[test]
    fn strings_have_a_maximum_length() {
        let at_max = NoteParams {
            title: "x".repeat(255),
            ..valid()
        };
        assert!(at_max.validate().is_ok());
        let over = NoteParams {
            title: "x".repeat(256),
            ..valid()
        };
        assert_eq!(
            over.validate().unwrap_err().on("title"),
            ["is too long (maximum is 255 characters)"]
        );
        let at_max = NoteParams {
            body: "x".repeat(100000),
            ..valid()
        };
        assert!(at_max.validate().is_ok());
        let over = NoteParams {
            body: "x".repeat(100001),
            ..valid()
        };
        assert_eq!(
            over.validate().unwrap_err().on("body"),
            ["is too long (maximum is 100000 characters)"]
        );
    }
}
