//! The queue's tables, mirroring `migration/src/m20261016_000200_create_jobs.rs`.
//! GUIDE: .offrails/guides/jobs.md#tables-migration-create_jobs-generated-by-generatorsnewmd-every-app-has-jobs

pub mod job {
    use sea_orm::entity::prelude::*;
    use serde::Serialize;

    /// The state machine. GUIDE: .offrails/guides/jobs.md#the-state-machine
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum, Serialize)]
    #[sea_orm(rs_type = "String", db_type = "Text", rename_all = "snake_case")]
    #[serde(rename_all = "snake_case")]
    pub enum JobState {
        Available,
        Scheduled,
        Executing,
        Retryable,
        Completed,
        Discarded,
        Cancelled,
    }

    impl JobState {
        /// Every state, in the order operators care about (the dashboard's counters).
        pub const ALL: [JobState; 7] = [
            Self::Executing,
            Self::Available,
            Self::Scheduled,
            Self::Retryable,
            Self::Discarded,
            Self::Cancelled,
            Self::Completed,
        ];

        /// `completed`, `discarded` and `cancelled` never change again (until an operator retries).
        pub fn is_final(self) -> bool {
            matches!(self, Self::Completed | Self::Discarded | Self::Cancelled)
        }

        pub fn as_str(self) -> &'static str {
            match self {
                Self::Available => "available",
                Self::Scheduled => "scheduled",
                Self::Executing => "executing",
                Self::Retryable => "retryable",
                Self::Completed => "completed",
                Self::Discarded => "discarded",
                Self::Cancelled => "cancelled",
            }
        }

        /// The state named `raw`, or `None`. Never panics on input from a URL or a CLI flag.
        pub fn parse(raw: &str) -> Option<Self> {
            Self::ALL.into_iter().find(|s| s.as_str() == raw)
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
    #[sea_orm(table_name = "jobs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub state: JobState,
        pub queue: String,
        pub worker: String,
        pub args: Json,
        pub meta: Json,
        pub tags: Json,
        /// `[{attempt, at, error, detail?, node?, duration_ms?}]`, oldest first, the last 20 kept.
        pub errors: Json,
        pub attempt: i32,
        pub max_attempts: i32,
        pub priority: i16,
        pub progress: Option<i16>,
        pub batch_id: Option<i64>,
        pub unique_key: Option<String>,
        pub attempted_by: Option<String>,
        pub inserted_at: DateTimeUtc,
        pub scheduled_at: DateTimeUtc,
        pub attempted_at: Option<DateTimeUtc>,
        pub completed_at: Option<DateTimeUtc>,
        pub cancelled_at: Option<DateTimeUtc>,
        pub discarded_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl Model {
        /// The tags as strings (non-strings are skipped).
        pub fn tag_list(&self) -> Vec<String> {
            self.tags
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        }

        /// When the job reached its final state, if it did.
        pub fn finished_at(&self) -> Option<DateTimeUtc> {
            self.completed_at
                .or(self.discarded_at)
                .or(self.cancelled_at)
        }
    }
}

pub mod job_queue {
    use sea_orm::entity::prelude::*;

    /// One row per queue any node has run or an operator paused: pause/resume across nodes.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "job_queues")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub name: String,
        pub paused: bool,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod job_node {
    use sea_orm::entity::prelude::*;

    /// A running worker process. `queues` is `{"default": {"limit": 10, "rate": "60/1m"}}`.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "job_nodes")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub node: String,
        pub queues: Json,
        pub started_at: DateTimeUtc,
        pub heartbeat_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod recurring_execution {
    use sea_orm::entity::prelude::*;

    /// One row per cron run: the unique `(key, run_at)` makes each run happen once across nodes.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "recurring_executions")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub key: String,
        pub run_at: DateTimeUtc,
        /// The job this run enqueued (the cron page links it).
        pub job_id: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod job_batch {
    use sea_orm::entity::prelude::*;

    /// A group of jobs with an optional callback job, enqueued once when every member is final.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "job_batches")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub total: i32,
        /// The callback as `{worker, args, queue, priority, max_attempts, tags, meta}`.
        pub callback: Option<Json>,
        pub callback_job_id: Option<i64>,
        pub inserted_at: DateTimeUtc,
        pub finished_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
