pub use sea_orm_migration::MigrationStatus;
pub use sea_orm_migration::prelude::*;

mod m20261016_000000_create_cache_entries;
// samples: migration mods

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261016_000000_create_cache_entries::Migration),
            // samples: migrations
        ]
    }
}
