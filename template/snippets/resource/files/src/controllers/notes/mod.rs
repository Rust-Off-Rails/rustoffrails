//! Notes: list, show, create, edit and delete (htmx: POST-only forms), one file per
//! action pair. From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).
//! SECURITY: --public: anyone may change these; add accounts before real data.
//! APP: the spec is product/features/notes.md; cite its AC ids on the tests.

mod destroy;
mod edit;
mod index;
mod new;
mod show;

pub use destroy::destroy;
pub use edit::{edit, update};
pub use index::index;
pub use new::{create, new};
pub use show::show;
