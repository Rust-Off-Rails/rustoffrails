//! The job queue's tables (`kernel/jobs/entities.rs` mirrors them).
//! GUIDE: .offrails/guides/jobs.md#tables-migration-create_jobs-generated-by-generatorsnewmd-every-app-has-jobs
//!
//! WHY: every id is `big_pk_auto` / `big_integer` (64-bit): a 32-bit id passes on SQLite and is a
//! 500 on every Postgres write.

use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

const STATES: [&str; 7] = [
    "available",
    "scheduled",
    "executing",
    "retryable",
    "completed",
    "discarded",
    "cancelled",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_table(
            Table::create()
                .table(Jobs::Table)
                .if_not_exists()
                .col(big_pk_auto(Jobs::Id))
                .col(string(Jobs::State))
                .col(string(Jobs::Queue).default("default"))
                .col(string(Jobs::Worker))
                .col(json(Jobs::Args))
                .col(json(Jobs::Meta))
                .col(json(Jobs::Tags))
                .col(json(Jobs::Errors))
                .col(integer(Jobs::Attempt).default(0))
                .col(integer(Jobs::MaxAttempts).default(20))
                .col(small_integer(Jobs::Priority).default(0))
                // WHY: 0-100 while executing (`JobInfo::progress`), shown live on the dashboard.
                .col(small_integer_null(Jobs::Progress))
                .col(big_integer_null(Jobs::BatchId))
                .col(string_null(Jobs::UniqueKey))
                .col(string_null(Jobs::AttemptedBy))
                .col(timestamp_with_time_zone(Jobs::InsertedAt))
                .col(timestamp_with_time_zone(Jobs::ScheduledAt))
                .col(timestamp_with_time_zone_null(Jobs::AttemptedAt))
                .col(timestamp_with_time_zone_null(Jobs::CompletedAt))
                .col(timestamp_with_time_zone_null(Jobs::CancelledAt))
                .col(timestamp_with_time_zone_null(Jobs::DiscardedAt))
                // INVARIANT: a row is always in one of the seven states (test: the_state_column_rejects_unknown_states)
                .check(Expr::col(Jobs::State).is_in(STATES))
                .check(Expr::col(Jobs::Priority).between(0, 9))
                .to_owned(),
        )
        .await?;
        for (name, cols) in [
            // The claim: WHERE state = 'available' AND queue = ? AND scheduled_at <= ? ORDER BY priority, scheduled_at, id.
            (
                "jobs_fetch_idx",
                &[
                    Jobs::State,
                    Jobs::Queue,
                    Jobs::Priority,
                    Jobs::ScheduledAt,
                    Jobs::Id,
                ][..],
            ),
            ("jobs_worker_idx", &[Jobs::Worker, Jobs::State][..]),
            ("jobs_inserted_idx", &[Jobs::InsertedAt][..]),
            // Rate-limited queues count recent starts; the dashboard's latency chart reads them.
            ("jobs_attempted_idx", &[Jobs::Queue, Jobs::AttemptedAt][..]),
            // The Pruner and the throughput chart.
            ("jobs_completed_idx", &[Jobs::CompletedAt][..]),
            ("jobs_discarded_idx", &[Jobs::DiscardedAt][..]),
            ("jobs_cancelled_idx", &[Jobs::CancelledAt][..]),
            ("jobs_batch_idx", &[Jobs::BatchId, Jobs::State][..]),
        ] {
            let mut index = Index::create();
            index.name(name).table(Jobs::Table);
            for col in cols {
                index.col(col.clone());
            }
            m.create_index(index.to_owned()).await?;
        }
        // WHY: NULLs never collide (SQLite and Postgres), so only jobs with a unique key compete.
        m.create_index(
            Index::create()
                .name("jobs_unique_key_key")
                .table(Jobs::Table)
                .col(Jobs::UniqueKey)
                .unique()
                .to_owned(),
        )
        .await?;

        m.create_table(
            Table::create()
                .table(JobQueues::Table)
                .if_not_exists()
                .col(string(JobQueues::Name).primary_key())
                .col(boolean(JobQueues::Paused).default(false))
                .col(timestamp_with_time_zone(JobQueues::UpdatedAt))
                .to_owned(),
        )
        .await?;

        m.create_table(
            Table::create()
                .table(JobNodes::Table)
                .if_not_exists()
                .col(string(JobNodes::Node).primary_key())
                .col(json(JobNodes::Queues))
                .col(timestamp_with_time_zone(JobNodes::StartedAt))
                .col(timestamp_with_time_zone(JobNodes::HeartbeatAt))
                .to_owned(),
        )
        .await?;

        m.create_table(
            Table::create()
                .table(RecurringExecutions::Table)
                .if_not_exists()
                .col(big_pk_auto(RecurringExecutions::Id))
                .col(string(RecurringExecutions::Key))
                .col(timestamp_with_time_zone(RecurringExecutions::RunAt))
                .col(big_integer_null(RecurringExecutions::JobId))
                .to_owned(),
        )
        .await?;
        // INVARIANT: one run per (key, due time) across every node (test: cron_fires_once_across_concurrent_ticks)
        m.create_index(
            Index::create()
                .name("recurring_executions_key_run_at_key")
                .table(RecurringExecutions::Table)
                .col(RecurringExecutions::Key)
                .col(RecurringExecutions::RunAt)
                .unique()
                .to_owned(),
        )
        .await?;

        m.create_table(
            Table::create()
                .table(JobBatches::Table)
                .if_not_exists()
                .col(big_pk_auto(JobBatches::Id))
                .col(string(JobBatches::Name))
                .col(integer(JobBatches::Total))
                .col(json_null(JobBatches::Callback))
                .col(big_integer_null(JobBatches::CallbackJobId))
                .col(timestamp_with_time_zone(JobBatches::InsertedAt))
                .col(timestamp_with_time_zone_null(JobBatches::FinishedAt))
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            JobBatches::Table.into_iden(),
            RecurringExecutions::Table.into_iden(),
            JobNodes::Table.into_iden(),
            JobQueues::Table.into_iden(),
            Jobs::Table.into_iden(),
        ] {
            m.drop_table(Table::drop().table(table).to_owned()).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden, Clone)]
enum Jobs {
    Table,
    Id,
    State,
    Queue,
    Worker,
    Args,
    Meta,
    Tags,
    Errors,
    Attempt,
    MaxAttempts,
    Priority,
    Progress,
    BatchId,
    UniqueKey,
    AttemptedBy,
    InsertedAt,
    ScheduledAt,
    AttemptedAt,
    CompletedAt,
    CancelledAt,
    DiscardedAt,
}

#[derive(DeriveIden)]
enum JobQueues {
    Table,
    Name,
    Paused,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum JobNodes {
    Table,
    Node,
    Queues,
    StartedAt,
    HeartbeatAt,
}

#[derive(DeriveIden)]
enum RecurringExecutions {
    Table,
    Id,
    Key,
    RunAt,
    JobId,
}

#[derive(DeriveIden)]
enum JobBatches {
    Table,
    Id,
    Name,
    Total,
    Callback,
    CallbackJobId,
    InsertedAt,
    FinishedAt,
}
