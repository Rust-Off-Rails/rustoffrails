//! Notes: the entity (it mirrors the migration), its relations and its label. Validation is in
//! params.rs, the queries in queries.rs; both are re-exported here (`note::find`, `note::NoteParams`).
//! From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).
//! APP: say what a note is, and cite its spec (product/features/notes.md).

use sea_orm::entity::prelude::*;

mod params;
mod queries;

pub use params::*;
pub use queries::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "notes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub title: String,
    #[sea_orm(column_type = "Text")]
    pub body: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    // samples: columns
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    // samples: relations
}

// samples: related

/// Must stay empty: callbacks are banned (.offrails/guides/models.md#side-effects-no-callbacks).
impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// How a note is named in lists and `<select>`s.
    pub fn label(&self) -> String {
        self.title.clone()
    }
}
