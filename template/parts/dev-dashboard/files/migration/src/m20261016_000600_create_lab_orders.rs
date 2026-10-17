//! The load lab's fake data (`kernel/dashboard/lab.rs` mirrors these tables): orders and their
//! items, so the lab can show a fast page, an N+1 and a slow query on real tables.
//! APP: drop these two tables (a new migration) once `lab::seed` writes your own models instead.
//!
//! WHY: every id is `big_pk_auto` / `big_integer` (64-bit): a 32-bit id passes on SQLite and is a
//! 500 on every Postgres write.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(LabOrders::Table)
                .if_not_exists()
                .col(big_pk_auto(LabOrders::Id))
                .col(string(LabOrders::Reference))
                .col(string(LabOrders::Customer))
                .col(string(LabOrders::Status))
                .col(big_integer(LabOrders::TotalCents))
                .col(timestamp_with_time_zone(LabOrders::CreatedAt))
                .to_owned(),
        )
        .await?;
        m.create_table(
            Table::create()
                .table(LabOrderItems::Table)
                .if_not_exists()
                .col(big_pk_auto(LabOrderItems::Id))
                .col(big_integer(LabOrderItems::OrderId))
                .col(string(LabOrderItems::Sku))
                .col(integer(LabOrderItems::Quantity))
                .col(big_integer(LabOrderItems::PriceCents))
                .foreign_key(
                    ForeignKey::create()
                        .from(LabOrderItems::Table, LabOrderItems::OrderId)
                        .to(LabOrders::Table, LabOrders::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("lab_order_items_order_id_idx")
                .table(LabOrderItems::Table)
                .col(LabOrderItems::OrderId)
                .if_not_exists()
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(LabOrderItems::Table).to_owned())
            .await?;
        m.drop_table(Table::drop().table(LabOrders::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum LabOrders {
    Table,
    Id,
    Reference,
    Customer,
    Status,
    TotalCents,
    CreatedAt,
}

#[derive(DeriveIden)]
enum LabOrderItems {
    Table,
    Id,
    OrderId,
    Sku,
    Quantity,
    PriceCents,
}
