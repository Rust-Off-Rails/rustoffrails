//! `seed_support::backdate` (the seed-backdated snippet). It works on any generated table (`id`,
//! `created_at`, `updated_at`), owner-scoped or public, so the test makes a table of that shape
//! instead of needing one of the app's.

use chrono::{Duration, Utc};
use offrails_app::tasks::seed_support::{backdate, days_ago};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, QueryOrder, Schema, Set};

use crate::support::TestApp;

/// A table shaped like a generated one. The test makes it (on Postgres the reused test database
/// notices the new table and is cloned afresh for the next test).
mod probe {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "seed_backdated_probes")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

async fn with_probe_table() -> TestApp {
    let app = TestApp::new().await;
    let schema = Schema::new(app.db().get_database_backend());
    let create = schema
        .create_table_from_entity(probe::Entity)
        .if_not_exists()
        .to_owned();
    app.db().execute(&create).await.unwrap();
    app
}

/// A row stamped now, as a model's `create` would.
async fn probe_row(app: &TestApp) -> probe::Model {
    let now = Utc::now();
    probe::ActiveModel {
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(app.db())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_backdated_row_is_older_and_lists_after_a_new_one() {
    let app = with_probe_table().await;
    let old = probe_row(&app).await;
    let new = probe_row(&app).await;
    backdate::<probe::Entity>(app.db(), old.id, days_ago(30))
        .await
        .unwrap();
    let old = probe::Entity::find_by_id(old.id)
        .one(app.db())
        .await
        .unwrap()
        .unwrap();
    let age = new.created_at - old.created_at;
    assert!(
        age > Duration::days(29) && age < Duration::days(31),
        "{age}"
    );
    assert_eq!(old.updated_at, old.created_at);
    let newest_first = probe::Entity::find()
        .order_by_desc(probe::Column::CreatedAt)
        .all(app.db())
        .await
        .unwrap();
    assert_eq!(
        newest_first.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![new.id, old.id]
    );
}

#[tokio::test]
async fn backdating_a_missing_row_is_an_error() {
    let app = with_probe_table().await;
    assert!(
        backdate::<probe::Entity>(app.db(), 999_999, days_ago(1))
            .await
            .is_err()
    );
}
