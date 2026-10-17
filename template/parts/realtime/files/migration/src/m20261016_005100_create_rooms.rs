//! Live rooms (the realtime sample's demo channel): rooms, who may join a members-only room, and
//! the messages, whose ids are the resume cursor (`seq`).

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(Rooms::Table)
                .if_not_exists()
                .col(big_pk_auto(Rooms::Id))
                .col(string(Rooms::Slug))
                .col(string(Rooms::Name))
                .col(string(Rooms::Headline).default(""))
                .col(boolean(Rooms::MembersOnly).default(false))
                .col(timestamp_with_time_zone(Rooms::CreatedAt))
                .col(timestamp_with_time_zone(Rooms::UpdatedAt))
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("rooms_slug_key")
                .table(Rooms::Table)
                .col(Rooms::Slug)
                .unique()
                .to_owned(),
        )
        .await?;

        m.create_table(
            Table::create()
                .table(RoomMembers::Table)
                .if_not_exists()
                .col(big_pk_auto(RoomMembers::Id))
                .col(big_integer(RoomMembers::RoomId))
                // The app's person id (a guest here, a user with accounts): no foreign key, so the
                // table survives swapping the identity source.
                .col(big_integer(RoomMembers::PersonId))
                .col(timestamp_with_time_zone(RoomMembers::CreatedAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("room_members_room_id_fkey")
                        .from(RoomMembers::Table, RoomMembers::RoomId)
                        .to(Rooms::Table, Rooms::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .name("room_members_room_id_person_id_key")
                .table(RoomMembers::Table)
                .col(RoomMembers::RoomId)
                .col(RoomMembers::PersonId)
                .unique()
                .to_owned(),
        )
        .await?;

        m.create_table(
            Table::create()
                .table(RoomMessages::Table)
                .if_not_exists()
                .col(big_pk_auto(RoomMessages::Id))
                .col(big_integer(RoomMessages::RoomId))
                .col(big_integer(RoomMessages::AuthorId))
                .col(string(RoomMessages::AuthorName))
                .col(text(RoomMessages::Body))
                .col(timestamp_with_time_zone(RoomMessages::CreatedAt))
                .foreign_key(
                    ForeignKey::create()
                        .name("room_messages_room_id_fkey")
                        .from(RoomMessages::Table, RoomMessages::RoomId)
                        .to(Rooms::Table, Rooms::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        // Replays read "this room's messages after id N", newest pages read the tail.
        m.create_index(
            Index::create()
                .name("room_messages_room_id_id_idx")
                .table(RoomMessages::Table)
                .col(RoomMessages::RoomId)
                .col(RoomMessages::Id)
                .to_owned(),
        )
        .await?;

        // APP: the rooms the app starts with.
        let insert = Query::insert()
            .into_table(Rooms::Table)
            .columns([
                Rooms::Slug,
                Rooms::Name,
                Rooms::Headline,
                Rooms::MembersOnly,
                Rooms::CreatedAt,
                Rooms::UpdatedAt,
            ])
            .values([
                "lobby".into(),
                "Lobby".into(),
                "Say hello. Everyone who is signed in can join.".into(),
                false.into(),
                Expr::current_timestamp(),
                Expr::current_timestamp(),
            ])
            .map_err(|e| DbErr::Custom(e.to_string()))?
            .values([
                "design".into(),
                "Design review".into(),
                "Screens, copy and the small things.".into(),
                false.into(),
                Expr::current_timestamp(),
                Expr::current_timestamp(),
            ])
            .map_err(|e| DbErr::Custom(e.to_string()))?
            .values([
                "backstage".into(),
                "Backstage".into(),
                "Members only.".into(),
                true.into(),
                Expr::current_timestamp(),
                Expr::current_timestamp(),
            ])
            .map_err(|e| DbErr::Custom(e.to_string()))?
            .to_owned();
        m.exec_stmt(insert).await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(RoomMessages::Table).to_owned())
            .await?;
        m.drop_table(Table::drop().table(RoomMembers::Table).to_owned())
            .await?;
        m.drop_table(Table::drop().table(Rooms::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Rooms {
    Table,
    Id,
    Slug,
    Name,
    Headline,
    MembersOnly,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum RoomMembers {
    Table,
    Id,
    RoomId,
    PersonId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum RoomMessages {
    Table,
    Id,
    RoomId,
    AuthorId,
    AuthorName,
    Body,
    CreatedAt,
}
