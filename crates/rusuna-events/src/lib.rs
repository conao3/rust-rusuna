//! Event triggers and cron triggers on PostgreSQL for rusuna.
//!
//! A row trigger of PostgreSQL records each insert, update or delete of a declared table
//! in the table `rusuna.event_log`, in the transaction of the change. A [`Worker`] claims
//! pending events with `FOR UPDATE SKIP LOCKED`, delivers each to the Rust handler or the
//! webhook registered under its trigger name, and records the result. Cron triggers and
//! one-off scheduled events are rows of the same table and are delivered the same way.
//!
//! [`migration`] returns the SQL of the tables, the trigger function and one trigger per
//! [`EventTrigger`]; add it to your migrations or apply it with [`migrate`].

mod cron;
mod migration;
mod webhook;
mod worker;

pub use cron::CronTrigger;
pub use migration::{migrate, migration};
pub use webhook::Webhook;
pub use worker::{Events, Worker, WorkerMonitor, WorkerOptions, WorkerStats};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::time::Duration;

/// The errors of this crate.
#[derive(Debug)]
pub enum Error {
    /// A trigger, cron or delivery definition is invalid.
    Definition(String),
    /// A database operation failed.
    Database(sqlx::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Definition(message) => write!(f, "{message}"),
            Error::Database(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Definition(_) => None,
            Error::Database(error) => Some(error),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        Error::Database(error)
    }
}

/// The error a handler returns to have its event retried.
pub type HandlerError = Box<dyn std::error::Error + Send + Sync>;

/// A row operation that fires an event trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Operation {
    /// `INSERT`: the event has `new` only.
    Insert,
    /// `UPDATE`: the event has `old` and `new`.
    Update,
    /// `DELETE`: the event has `old` only.
    Delete,
}

impl Operation {
    /// The SQL keyword, which is also the `op` of the event payload.
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Insert => "INSERT",
            Operation::Update => "UPDATE",
            Operation::Delete => "DELETE",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "INSERT" => Some(Operation::Insert),
            "UPDATE" => Some(Operation::Update),
            "DELETE" => Some(Operation::Delete),
            _ => None,
        }
    }
}

/// The declaration of an event trigger on a table.
///
/// The JSON form, which `rusuna events generate-migration` reads, uses the field names
/// `Name`, `Table`, `Schema`, `Operations` and `Columns`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct EventTrigger {
    /// The trigger name: 1 to 42 ASCII letters, digits, `_` or `-`. Events carry it, and a
    /// worker delivers them to the handler or webhook registered under it.
    pub name: String,
    /// The SQL table.
    pub table: String,
    /// The SQL schema of the table. Defaults to `public`.
    #[serde(default = "public")]
    pub schema: String,
    /// The operations that record an event.
    pub operations: Vec<Operation>,
    /// The SQL columns an update must change to record an event. Empty means every update.
    #[serde(default)]
    pub columns: Vec<String>,
}

fn public() -> String {
    "public".into()
}

impl EventTrigger {
    /// Declares a trigger on `table` in the schema `public` for inserts, updates and
    /// deletes.
    pub fn new(name: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            table: table.into(),
            schema: public(),
            operations: vec![Operation::Insert, Operation::Update, Operation::Delete],
            columns: Vec::new(),
        }
    }

    /// Sets the SQL schema of the table.
    pub fn schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = schema.into();
        self
    }

    /// Sets the operations that record an event.
    pub fn operations(mut self, operations: impl IntoIterator<Item = Operation>) -> Self {
        self.operations = operations.into_iter().collect();
        self
    }

    /// Records an update only when it changes one of these SQL columns.
    pub fn columns<S: Into<String>>(mut self, columns: impl IntoIterator<Item = S>) -> Self {
        self.columns = columns.into_iter().map(Into::into).collect();
        self
    }
}

/// How often and how long a delivery is tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    /// Attempts after the first one. Defaults to 0.
    pub max_retries: u32,
    /// The time between a failed attempt and the next one. Defaults to 10 seconds.
    pub interval: Duration,
    /// The time an attempt may take before it counts as failed. Defaults to 60 seconds.
    pub timeout: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            max_retries: 0,
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(60),
        }
    }
}

/// What caused an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Cause {
    /// A row change recorded by an event trigger.
    Row {
        /// The SQL schema of the table.
        schema: String,
        /// The SQL table.
        table: String,
        /// The operation.
        op: Operation,
        /// The row before the change, keyed by SQL column names.
        old: Option<Json>,
        /// The row after the change, keyed by SQL column names.
        new: Option<Json>,
    },
    /// An occurrence of a cron trigger.
    Cron {
        /// The occurrence.
        scheduled_at: DateTime<Utc>,
    },
    /// A one-off event created by [`schedule`].
    Scheduled {
        /// The time the event was due.
        scheduled_at: DateTime<Utc>,
    },
}

/// One delivery attempt of an event, as a handler receives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The event's UUID.
    pub id: String,
    /// The trigger name.
    pub trigger: String,
    /// The row change or the schedule.
    pub cause: Cause,
    /// The payload of a cron trigger or a scheduled event; null for row events.
    pub payload: Json,
    /// When the event was recorded.
    pub created_at: DateTime<Utc>,
    /// The number of this attempt, starting at 1.
    pub attempt: u32,
    /// The configured retries of the trigger.
    pub max_retries: u32,
}

fn timestamp(time: &DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Micros, true)
}

impl Event {
    /// The JSON a webhook receives.
    ///
    /// A row event has the shape of a Hasura v2 event trigger payload: `id`, `created_at`,
    /// `trigger.name`, `table.schema`, `table.name`, `event.op`, `event.data.old`,
    /// `event.data.new` and `delivery_info`. A cron or scheduled event has `id`, `name`,
    /// `scheduled_time` and `payload`.
    pub fn body(&self) -> Json {
        match &self.cause {
            Cause::Row {
                schema,
                table,
                op,
                old,
                new,
            } => json!({
                "id": self.id,
                "created_at": timestamp(&self.created_at),
                "trigger": {"name": self.trigger},
                "table": {"schema": schema, "name": table},
                "event": {"op": op.as_str(), "data": {"old": old, "new": new}},
                "delivery_info": {
                    "max_retries": self.max_retries,
                    "current_retry": self.attempt - 1,
                },
            }),
            Cause::Cron { scheduled_at } | Cause::Scheduled { scheduled_at } => json!({
                "id": self.id,
                "name": self.trigger,
                "scheduled_time": timestamp(scheduled_at),
                "payload": self.payload,
            }),
        }
    }
}

/// The source of the current time for a [`Worker`].
///
/// Every comparison with a due time, a retry time or a lease uses this clock, and cron
/// occurrences are computed from it.
pub trait Clock: Send + Sync + 'static {
    /// The current time.
    fn now(&self) -> DateTime<Utc>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub(crate) fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > 42
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::Definition(format!(
            "invalid trigger name {name:?}: use 1 to 42 ASCII letters, digits, `_` or `-`"
        )));
    }
    Ok(())
}

/// Records a one-off event for the trigger `name`, due at `at`.
///
/// A worker delivers it to the handler or webhook registered under `name` once its clock
/// reaches `at`. Pass a transaction to record the event only if the transaction commits.
/// Returns the event's UUID.
pub async fn schedule<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
    at: DateTime<Utc>,
    payload: Json,
) -> Result<String, Error> {
    validate_name(name)?;
    let id: String = sqlx::query_scalar(
        "INSERT INTO rusuna.event_log (kind, trigger_name, scheduled_at, next_attempt_at, payload) VALUES ('scheduled', $1, $2, $2, $3) RETURNING id::text",
    )
    .bind(name)
    .bind(at)
    .bind(payload)
    .fetch_one(executor)
    .await?;
    Ok(id)
}

/// Makes a failed event pending again, with its attempts counted from zero.
///
/// Returns whether the event was failed.
pub async fn redeliver<'e>(executor: impl sqlx::PgExecutor<'e>, id: &str) -> Result<bool, Error> {
    let result = sqlx::query(
        "UPDATE rusuna.event_log SET status = 'pending', tries = 0, next_attempt_at = NULL, locked_until = NULL, completed_at = NULL WHERE id = $1::uuid AND status = 'failed'",
    )
    .bind(id)
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Deletes delivered events completed before `before`, with their invocations.
///
/// The latest occurrence of each cron trigger stays: a worker that starts reads it to
/// find the occurrences it missed. Failed and pending events stay. Returns the number of
/// deleted events.
pub async fn cleanup<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    before: DateTime<Utc>,
) -> Result<u64, Error> {
    let result = sqlx::query(
        "DELETE FROM rusuna.event_log AS e WHERE e.status = 'delivered' AND e.completed_at < $1 AND NOT (e.kind = 'cron' AND e.scheduled_at = (SELECT max(c.scheduled_at) FROM rusuna.event_log AS c WHERE c.kind = 'cron' AND c.trigger_name = e.trigger_name))",
    )
    .bind(before)
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_names_are_bounded_identifiers() {
        assert!(validate_name("article_created-2").is_ok());
        assert!(validate_name(&"a".repeat(42)).is_ok());
        for name in ["", "a b", "a\"", "ä", &"a".repeat(43)] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn row_events_have_the_hasura_payload_shape() {
        let event = Event {
            id: "00000000-0000-0000-0000-000000000001".into(),
            trigger: "article_created".into(),
            cause: Cause::Row {
                schema: "public".into(),
                table: "article".into(),
                op: Operation::Update,
                old: Some(json!({"id": 1, "title": "a"})),
                new: Some(json!({"id": 1, "title": "b"})),
            },
            payload: Json::Null,
            created_at: DateTime::from_timestamp(1_700_000_000, 5_000).unwrap(),
            attempt: 2,
            max_retries: 3,
        };
        assert_eq!(
            event.body(),
            json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "created_at": "2023-11-14T22:13:20.000005Z",
                "trigger": {"name": "article_created"},
                "table": {"schema": "public", "name": "article"},
                "event": {"op": "UPDATE", "data": {"old": {"id": 1, "title": "a"}, "new": {"id": 1, "title": "b"}}},
                "delivery_info": {"max_retries": 3, "current_retry": 1},
            })
        );
    }

    #[test]
    fn trigger_json_uses_the_cli_field_names() {
        let trigger: EventTrigger = serde_json::from_value(json!({
            "Name": "article_title",
            "Table": "article",
            "Operations": ["UPDATE"],
            "Columns": ["title"],
        }))
        .unwrap();
        assert_eq!(
            trigger,
            EventTrigger::new("article_title", "article")
                .operations([Operation::Update])
                .columns(["title"])
        );
        assert!(
            serde_json::from_value::<EventTrigger>(
                json!({"Name": "a", "Table": "t", "Operations": [], "Extra": 1})
            )
            .is_err()
        );
    }
}
