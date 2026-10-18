//! Notes test data (.offrails/guides/testing.md#factories-testsappfactoriesrs): valid params
//! through `validate` + `create`, so a test can't build an invalid row. From the resource snippet (what `cargo offrails generate resource notes title:string body:text --public` writes).

/// Valid NoteParams (unique values numbered), for forms and `note_with`.
pub async fn note_params(
    db: &sea_orm::DatabaseConnection,
) -> offrails_app::models::note::NoteParams {
    let _ = db; // no parent records to create
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    offrails_app::models::note::NoteParams {
        title: format!("Title {n}"),
        body: format!("Body {n}, long enough to read."),
    }
}

/// A note, through `NoteParams::validate` + `note::create`.
pub async fn note(db: &sea_orm::DatabaseConnection) -> offrails_app::models::note::Model {
    let params = note_params(db).await;
    note_with(db, params).await
}

/// A note from these params (they must be valid).
pub async fn note_with(
    db: &sea_orm::DatabaseConnection,
    params: offrails_app::models::note::NoteParams,
) -> offrails_app::models::note::Model {
    let new = params.validate().expect("valid note params");
    offrails_app::models::note::create(db, new)
        .await
        .expect("create note")
}

/// Reads a note back through the entity (tests can't call the model-private `find`).
pub async fn reload_note(
    db: &sea_orm::DatabaseConnection,
    id: i64,
) -> Option<offrails_app::models::note::Model> {
    use sea_orm::EntityTrait;
    offrails_app::models::note::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("reload note")
}
