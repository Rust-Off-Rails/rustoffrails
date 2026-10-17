//! A guest: the realtime sample's stand-in for an account, so the live room works before the app
//! has accounts. A display name, nothing else (no email, no password).
//!
//! APP: with accounts installed (an auth sample), delete this model, `guest_session.rs` and the
//! development sign-in, and point `src/channels/identity.rs` at your users and sessions.

use chrono::{DateTime, Utc};
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::{ActiveModelTrait, ConnectionTrait};

use crate::error::AppError;
use crate::models::Errors;

pub use entity::{Column, Entity, Model};

pub mod entity {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "guests")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// The longest display name. APP: names your people use.
pub const MAX_NAME: usize = 40;

/// The sign-in form (untrusted).
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct GuestParams {
    #[serde(default)]
    pub name: String,
}

/// A validated guest.
#[derive(Debug, Clone, PartialEq)]
pub struct NewGuest {
    pub name: String,
}

impl GuestParams {
    pub fn validate(&self) -> Result<NewGuest, Errors> {
        let name = self.name.trim();
        let mut errors = Errors::default();
        if name.is_empty() {
            errors.add("name", "can't be blank");
        } else if name.chars().count() > MAX_NAME {
            errors.add("name", "is too long (maximum is 40 characters)");
        } else if name.chars().any(char::is_control) {
            errors.add("name", "is invalid");
        }
        errors.into_result(|| NewGuest {
            name: name.to_owned(),
        })
    }
}

pub async fn create(
    db: &impl ConnectionTrait,
    new: NewGuest,
    now: DateTime<Utc>,
) -> Result<Model, AppError> {
    Ok(entity::ActiveModel {
        id: NotSet,
        name: Set(new.name),
        created_at: Set(now),
    }
    .insert(db)
    .await?)
}

/// "Avery Quinn" → "AQ"; one word → its first two letters.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let letters: String = match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [first, .., last] => first.chars().take(1).chain(last.chars().take(1)).collect(),
    };
    letters.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_bounded() {
        let ok = GuestParams {
            name: "  Avery Quinn ".into(),
        };
        assert_eq!(ok.validate().unwrap().name, "Avery Quinn");
        assert!(GuestParams { name: " ".into() }.validate().is_err());
        assert!(
            GuestParams {
                name: "x".repeat(41)
            }
            .validate()
            .is_err()
        );
        assert!(
            GuestParams {
                name: "a\u{7}b".into()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn initials_come_from_the_first_and_last_words() {
        assert_eq!(initials("Avery Quinn"), "AQ");
        assert_eq!(initials("blake river stone"), "BS");
        assert_eq!(initials("Casey"), "CA");
        assert_eq!(initials(""), "?");
    }
}
