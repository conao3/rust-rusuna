use crate::webhook::{Client, Response};
use crate::{
    Cause, Clock, CronTrigger, Error, Event, EventTrigger, HandlerError, Operation, Retry,
    SystemClock, Webhook, migration, validate_name,
};
use chrono::{DateTime, Utc};
use serde_json::Value as Json;
use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::postgres::{PgListener, PgPool};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::Instant;

type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), HandlerError>> + Send>>;
type Handler = Arc<dyn Fn(Event) -> HandlerFuture + Send + Sync>;

#[derive(Clone)]
enum Target {
    Handler(Handler),
    Webhook(Webhook),
}

#[derive(Clone)]
struct Delivery {
    target: Target,
    retry: Retry,
}

/// The declarations and deliveries of event triggers, cron triggers and scheduled events.
///
/// Events of a trigger name are delivered to the Rust handler or the webhook registered
/// under that name. A name may have one delivery. A worker claims only the events of the
/// names it has a delivery for, so events of other names stay pending for workers that do.
#[derive(Clone, Default)]
pub struct Events {
    triggers: Vec<EventTrigger>,
    crons: Vec<CronTrigger>,
    deliveries: HashMap<String, Delivery>,
    duplicates: Vec<String>,
}

impl Events {
    /// No triggers and no deliveries.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares an event trigger on a table. Its SQL is part of [`Events::migration`].
    pub fn trigger(mut self, trigger: EventTrigger) -> Self {
        self.triggers.push(trigger);
        self
    }

    /// Declares a cron trigger. It needs a handler or a webhook of the same name.
    pub fn cron(mut self, cron: CronTrigger) -> Self {
        self.crons.push(cron);
        self
    }

    /// Delivers the events of `name` to the async function `handler`.
    ///
    /// The handler succeeds with `Ok(())`. An error, a panic or exceeding
    /// [`Retry::timeout`] fails the attempt; at the timeout the handler's future is dropped.
    pub fn handler<F, Fut>(self, name: impl Into<String>, retry: Retry, handler: F) -> Self
    where
        F: Fn(Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        let handler: Handler = Arc::new(move |event| Box::pin(handler(event)));
        self.deliver(name.into(), Target::Handler(handler), retry)
    }

    /// Delivers the events of `name` to `webhook`.
    pub fn webhook(self, name: impl Into<String>, retry: Retry, webhook: Webhook) -> Self {
        self.deliver(name.into(), Target::Webhook(webhook), retry)
    }

    fn deliver(mut self, name: String, target: Target, retry: Retry) -> Self {
        match self.deliveries.entry(name) {
            Entry::Occupied(entry) => self.duplicates.push(entry.key().clone()),
            Entry::Vacant(entry) => {
                entry.insert(Delivery { target, retry });
            }
        }
        self
    }

    /// The declared event triggers.
    pub fn triggers(&self) -> &[EventTrigger] {
        &self.triggers
    }

    /// The SQL of [`crate::migration`] for the declared event triggers.
    pub fn migration(&self) -> Result<String, Error> {
        migration(&self.triggers)
    }

    /// Applies [`Events::migration`] with [`crate::migrate`].
    pub async fn migrate(&self, pool: &PgPool) -> Result<(), Error> {
        crate::migrate(pool, &self.triggers).await
    }

    fn validate(&self) -> Result<(), Error> {
        if let Some(name) = self.duplicates.first() {
            return Err(Error::Definition(format!(
                "{name} has more than one handler or webhook"
            )));
        }
        migration(&self.triggers)?;
        for name in self.deliveries.keys() {
            validate_name(name)?;
        }
        let mut crons = HashSet::new();
        for cron in &self.crons {
            if !crons.insert(&cron.name) {
                return Err(Error::Definition(format!(
                    "the cron trigger {} is declared twice",
                    cron.name
                )));
            }
            if !self.deliveries.contains_key(&cron.name) {
                return Err(Error::Definition(format!(
                    "the cron trigger {} has no handler or webhook",
                    cron.name
                )));
            }
        }
        Ok(())
    }

    /// A worker that delivers these events from `pool`.
    pub fn worker(&self, pool: PgPool, options: WorkerOptions) -> Result<Worker, Error> {
        self.validate()?;
        if options.concurrency == 0 {
            return Err(Error::Definition("the concurrency must be positive".into()));
        }
        Ok(Worker(Shared {
            pool,
            deliveries: self.deliveries.clone(),
            crons: self.crons.clone(),
            clock: Arc::new(SystemClock),
            options,
            client: Client::new(),
            stats: Arc::default(),
        }))
    }
}

/// Settings of a [`Worker`].
#[derive(Debug, Clone, Copy)]
pub struct WorkerOptions {
    /// The time between two polls of the event table. Defaults to one second.
    pub poll_interval: Duration,
    /// The largest number of deliveries in progress. Defaults to 10.
    pub concurrency: usize,
    /// The time a claim outlives the trigger's timeout. Another worker claims an event
    /// again when its claim has expired without a recorded result. Defaults to 30
    /// seconds.
    pub lease_margin: Duration,
    /// Whether to poll at once on a notification of the channel `rusuna_events`, which
    /// the trigger function sends for each recorded row event. Defaults to true.
    pub listen: bool,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            concurrency: 10,
            lease_margin: Duration::from_secs(30),
            listen: true,
        }
    }
}

/// Counters of a [`Worker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkerStats {
    /// Completed polls: each records due cron occurrences and claims pending events.
    pub polls: u64,
    /// Claimed events.
    pub claimed: u64,
    /// Successful attempts whose result was recorded.
    pub delivered: u64,
    /// Failed attempts whose result was recorded.
    pub failed: u64,
    /// Database errors of polls, notifications and results.
    pub errors: u64,
}

#[derive(Default)]
struct Counters {
    polls: AtomicU64,
    claimed: AtomicU64,
    delivered: AtomicU64,
    failed: AtomicU64,
    errors: AtomicU64,
}

impl Counters {
    fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }
}

struct Shared {
    pool: PgPool,
    deliveries: HashMap<String, Delivery>,
    crons: Vec<CronTrigger>,
    clock: Arc<dyn Clock>,
    options: WorkerOptions,
    client: Client,
    stats: Arc<Counters>,
}

/// Reads the counters of a worker while it runs.
#[derive(Clone)]
pub struct WorkerMonitor(Arc<Counters>);

impl WorkerMonitor {
    /// The counters at this moment.
    pub fn stats(&self) -> WorkerStats {
        let s = &self.0;
        WorkerStats {
            polls: s.polls.load(Ordering::Relaxed),
            claimed: s.claimed.load(Ordering::Relaxed),
            delivered: s.delivered.load(Ordering::Relaxed),
            failed: s.failed.load(Ordering::Relaxed),
            errors: s.errors.load(Ordering::Relaxed),
        }
    }
}

/// Polls the event table, records due cron occurrences and delivers events.
///
/// Run it with `tokio::spawn(worker.run(shutdown))`. Several workers, in one process or
/// many, can run on the same database: a claim takes rows with `FOR UPDATE SKIP LOCKED`
/// and sets a lease of the trigger's timeout plus [`WorkerOptions::lease_margin`], and a
/// result is recorded only by the claim that is still current.
pub struct Worker(Shared);

struct Claimed {
    id: String,
    kind: String,
    trigger_name: String,
    schema_name: Option<String>,
    table_name: Option<String>,
    operation: Option<String>,
    old: Option<Json>,
    new: Option<Json>,
    payload: Option<Json>,
    scheduled_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    tries: i32,
}

impl Claimed {
    fn read(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            kind: row.try_get("kind")?,
            trigger_name: row.try_get("trigger_name")?,
            schema_name: row.try_get("schema_name")?,
            table_name: row.try_get("table_name")?,
            operation: row.try_get("operation")?,
            old: row.try_get("old")?,
            new: row.try_get("new")?,
            payload: row.try_get("payload")?,
            scheduled_at: row.try_get("scheduled_at")?,
            created_at: row.try_get("created_at")?,
            tries: row.try_get("tries")?,
        })
    }
}

const CLAIM: &str = "WITH config AS (SELECT * FROM unnest($2::text[], $3::float8[]) AS c(name, lease)), picked AS (SELECT e.id, c.lease FROM rusuna.event_log AS e JOIN config AS c ON c.name = e.trigger_name WHERE e.status = 'pending' AND (e.next_attempt_at IS NULL OR e.next_attempt_at <= $1) AND (e.locked_until IS NULL OR e.locked_until <= $1) ORDER BY e.created_at, e.id LIMIT $4 FOR UPDATE OF e SKIP LOCKED) UPDATE rusuna.event_log AS e SET tries = e.tries + 1, locked_until = $1 + make_interval(secs => picked.lease) FROM picked WHERE e.id = picked.id RETURNING e.id::text AS id, e.kind, e.trigger_name, e.schema_name, e.table_name, e.operation, e.old, e.new, e.payload, e.scheduled_at, e.created_at, e.tries";

const RECORD: &str = "WITH done AS (UPDATE rusuna.event_log SET status = CASE WHEN $4 THEN 'delivered' WHEN tries > $5 THEN 'failed' ELSE 'pending' END, next_attempt_at = CASE WHEN $4 THEN next_attempt_at ELSE $6 END, locked_until = NULL, completed_at = CASE WHEN $4 OR tries > $5 THEN $3 END WHERE id = $1::uuid AND tries = $2 AND status = 'pending' RETURNING id) INSERT INTO rusuna.event_invocation (event_id, attempt, succeeded, status, error, started_at, finished_at) SELECT id, $2, $4, $7, $8, $9, $3 FROM done";

const LAST_CRON: &str =
    "SELECT max(scheduled_at) FROM rusuna.event_log WHERE kind = 'cron' AND trigger_name = $1";

const INSERT_CRON: &str = "INSERT INTO rusuna.event_log (kind, trigger_name, scheduled_at, next_attempt_at, payload) SELECT 'cron', $1, t, t, $3 FROM unnest($2::timestamptz[]) AS t ON CONFLICT (trigger_name, scheduled_at) WHERE kind = 'cron' DO NOTHING";

fn seconds(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

fn later(time: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|d| time.checked_add_signed(d))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

impl Worker {
    /// Uses `clock` instead of the system clock.
    pub fn clock(self, clock: Arc<dyn Clock>) -> Self {
        Self(Shared { clock, ..self.0 })
    }

    /// A handle that reads the counters while the worker runs.
    pub fn monitor(&self) -> WorkerMonitor {
        WorkerMonitor(self.0.stats.clone())
    }

    /// Runs until `shutdown` completes, then stops claiming and waits for the deliveries in
    /// progress to record their results.
    ///
    /// Aborting the task instead drops the deliveries in progress; their events are
    /// claimed again when their leases expire.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) {
        let shared = Arc::new(self.0);
        let mut shutdown = std::pin::pin!(shutdown);
        let mut listener = None;
        let mut cron_state: HashMap<String, DateTime<Utc>> = HashMap::new();
        let mut deliveries = JoinSet::new();
        'polls: loop {
            if shared.options.listen && listener.is_none() {
                listener = Self::listen(&shared).await;
            }
            Self::record_crons(&shared, &mut cron_state).await;
            let capacity = shared.options.concurrency - deliveries.len();
            let mut more = capacity == 0;
            if capacity > 0 {
                match Self::claim(&shared, capacity).await {
                    Ok(events) => {
                        more = events.len() == capacity;
                        Counters::add(&shared.stats.claimed, events.len() as u64);
                        for event in events {
                            deliveries.spawn(Self::deliver(shared.clone(), event));
                        }
                    }
                    Err(_) => Counters::add(&shared.stats.errors, 1),
                }
            }
            Counters::add(&shared.stats.polls, 1);
            let deadline = Instant::now() + shared.options.poll_interval;
            loop {
                let notification = async {
                    match listener.as_mut() {
                        Some(listener) => listener.recv().await.map(|_| ()),
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    () = &mut shutdown => break 'polls,
                    Some(_) = deliveries.join_next(), if !deliveries.is_empty() => {
                        if more {
                            continue 'polls;
                        }
                    }
                    result = notification => {
                        if result.is_err() {
                            Counters::add(&shared.stats.errors, 1);
                            listener = None;
                        }
                        continue 'polls;
                    }
                    () = tokio::time::sleep_until(deadline) => continue 'polls,
                }
            }
        }
        while deliveries.join_next().await.is_some() {}
    }

    async fn listen(shared: &Shared) -> Option<PgListener> {
        let result = async {
            let mut listener = PgListener::connect_with(&shared.pool).await?;
            listener.listen("rusuna_events").await?;
            Ok::<_, sqlx::Error>(listener)
        }
        .await;
        result
            .map_err(|_| Counters::add(&shared.stats.errors, 1))
            .ok()
    }

    async fn record_crons(shared: &Shared, state: &mut HashMap<String, DateTime<Utc>>) {
        for cron in &shared.crons {
            let now = shared.clock.now();
            let earliest = chrono::Duration::from_std(cron.tolerance)
                .ok()
                .and_then(|d| now.checked_sub_signed(d))
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            let last = match state.get(&cron.name) {
                Some(last) => *last,
                None => {
                    let recorded: Result<Option<DateTime<Utc>>, _> = sqlx::query_scalar(LAST_CRON)
                        .bind(&cron.name)
                        .fetch_one(&shared.pool)
                        .await;
                    match recorded {
                        Ok(Some(recorded)) => recorded.max(earliest),
                        Ok(None) => now,
                        Err(_) => {
                            Counters::add(&shared.stats.errors, 1);
                            continue;
                        }
                    }
                }
            };
            let times: Vec<_> = cron
                .occurrences(last.max(earliest), now)
                .into_iter()
                .filter(|time| *time > earliest)
                .collect();
            let next = if times.len() == crate::cron::MAX_CATCH_UP {
                *times.last().unwrap()
            } else {
                now.max(last)
            };
            if !times.is_empty() {
                let inserted = sqlx::query(INSERT_CRON)
                    .bind(&cron.name)
                    .bind(&times)
                    .bind(&cron.payload)
                    .execute(&shared.pool)
                    .await;
                if inserted.is_err() {
                    Counters::add(&shared.stats.errors, 1);
                    state.insert(cron.name.clone(), last);
                    continue;
                }
            }
            state.insert(cron.name.clone(), next);
        }
    }

    async fn claim(shared: &Shared, limit: usize) -> Result<Vec<Claimed>, sqlx::Error> {
        let (names, leases): (Vec<_>, Vec<_>) = shared
            .deliveries
            .iter()
            .map(|(name, d)| {
                (
                    name.clone(),
                    seconds(d.retry.timeout + shared.options.lease_margin),
                )
            })
            .unzip();
        sqlx::query(CLAIM)
            .bind(shared.clock.now())
            .bind(names)
            .bind(leases)
            .bind(limit as i64)
            .fetch_all(&shared.pool)
            .await?
            .iter()
            .map(Claimed::read)
            .collect()
    }

    fn event(claimed: Claimed, retry: &Retry) -> Result<Event, String> {
        let invalid = |what: &str| format!("the event row has {what}");
        let scheduled_at = || {
            claimed
                .scheduled_at
                .ok_or_else(|| invalid("no scheduled_at"))
        };
        let cause = match claimed.kind.as_str() {
            "cron" => Cause::Cron {
                scheduled_at: scheduled_at()?,
            },
            "scheduled" => Cause::Scheduled {
                scheduled_at: scheduled_at()?,
            },
            "row" => Cause::Row {
                schema: claimed
                    .schema_name
                    .ok_or_else(|| invalid("no schema_name"))?,
                table: claimed.table_name.ok_or_else(|| invalid("no table_name"))?,
                op: claimed
                    .operation
                    .as_deref()
                    .and_then(Operation::parse)
                    .ok_or_else(|| invalid("no operation"))?,
                old: claimed.old,
                new: claimed.new,
            },
            kind => return Err(invalid(&format!("the unknown kind {kind:?}"))),
        };
        Ok(Event {
            id: claimed.id,
            trigger: claimed.trigger_name,
            cause,
            payload: claimed.payload.unwrap_or(Json::Null),
            created_at: claimed.created_at,
            attempt: claimed.tries.max(1) as u32,
            max_retries: retry.max_retries,
        })
    }

    async fn attempt(shared: &Shared, delivery: &Delivery, event: Event) -> Response {
        let timeout = delivery.retry.timeout;
        match &delivery.target {
            Target::Handler(handler) => {
                let mut task = JoinSet::new();
                let handler = handler.clone();
                task.spawn(async move { handler(event).await });
                match tokio::time::timeout(timeout, task.join_next()).await {
                    Ok(Some(Ok(Ok(())))) => Response {
                        status: None,
                        error: None,
                    },
                    Ok(Some(Ok(Err(error)))) => Response {
                        status: None,
                        error: Some(error.to_string()),
                    },
                    Ok(Some(Err(_))) | Ok(None) => Response {
                        status: None,
                        error: Some("the handler panicked".into()),
                    },
                    Err(_) => Response {
                        status: None,
                        error: Some(format!("the handler did not finish in {timeout:?}")),
                    },
                }
            }
            Target::Webhook(webhook) => {
                match tokio::time::timeout(timeout, shared.client.post(webhook, &event.body()))
                    .await
                {
                    Ok(response) => response,
                    Err(_) => Response {
                        status: None,
                        error: Some(format!("the webhook did not answer in {timeout:?}")),
                    },
                }
            }
        }
    }

    async fn deliver(shared: Arc<Shared>, claimed: Claimed) {
        let Some(delivery) = shared.deliveries.get(&claimed.trigger_name) else {
            return;
        };
        let tries = claimed.tries;
        let id = claimed.id.clone();
        let started = shared.clock.now();
        let response = match Self::event(claimed, &delivery.retry) {
            Ok(event) => Self::attempt(&shared, delivery, event).await,
            Err(error) => Response {
                status: None,
                error: Some(error),
            },
        };
        let finished = shared.clock.now();
        let succeeded = response.error.is_none();
        let recorded = sqlx::query(RECORD)
            .bind(&id)
            .bind(tries)
            .bind(finished)
            .bind(succeeded)
            .bind(i32::try_from(delivery.retry.max_retries).unwrap_or(i32::MAX))
            .bind(later(finished, delivery.retry.interval))
            .bind(response.status.map(i32::from))
            .bind(response.error)
            .bind(started)
            .execute(&shared.pool)
            .await;
        match recorded {
            Ok(result) if result.rows_affected() == 1 => Counters::add(
                if succeeded {
                    &shared.stats.delivered
                } else {
                    &shared.stats.failed
                },
                1,
            ),
            Ok(_) => {}
            Err(_) => Counters::add(&shared.stats.errors, 1),
        }
    }
}
