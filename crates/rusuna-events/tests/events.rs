use chrono::{DateTime, Utc};
use rusuna_events::{
    Cause, Clock, CronTrigger, Event, EventTrigger, Events, HandlerError, Operation, Retry,
    Webhook, WorkerMonitor, WorkerOptions,
};
use serde_json::{Value as Json, json};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

fn options(database: &str) -> PgConnectOptions {
    let config = std::env::var("RUSUNA_POSTGRES").expect("run the tests with make check");
    let field = |name: &str| {
        config
            .split(' ')
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("RUSUNA_POSTGRES has no `{name}`"))
            .to_string()
    };
    PgConnectOptions::new()
        .socket(field("host"))
        .username(&field("user"))
        .database(database)
}

async fn database(name: &str, events: &Events, setup: &str) -> PgPool {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options("postgres"))
        .await
        .unwrap();
    for statement in [
        format!("DROP DATABASE IF EXISTS {name}"),
        format!("CREATE DATABASE {name}"),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&admin)
            .await
            .unwrap();
    }
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options(name))
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(setup.to_string()))
        .execute(&pool)
        .await
        .unwrap();
    events.migrate(&pool).await.unwrap();
    pool
}

async fn until<F: Future<Output = bool>>(what: &str, mut condition: impl FnMut() -> F) {
    let start = Instant::now();
    while !condition().await {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn polls(monitor: &WorkerMonitor, n: u64) {
    let start = monitor.stats().polls;
    until("polls", || async { monitor.stats().polls >= start + n }).await;
}

async fn scalar(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(pool)
        .await
        .unwrap()
}

fn fast() -> WorkerOptions {
    WorkerOptions {
        poll_interval: Duration::from_millis(50),
        ..WorkerOptions::default()
    }
}

struct Running {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    monitor: WorkerMonitor,
}

impl Running {
    async fn stop(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap();
    }
}

fn start(
    events: &Events,
    pool: &PgPool,
    options: WorkerOptions,
    clock: Option<Arc<Manual>>,
) -> Running {
    let mut worker = events.worker(pool.clone(), options).unwrap();
    if let Some(clock) = clock {
        worker = worker.clock(clock);
    }
    let monitor = worker.monitor();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(worker.run(async move {
        let _ = stopped.await;
    }));
    Running {
        stop,
        task,
        monitor,
    }
}

struct Manual(Mutex<DateTime<Utc>>);

impl Manual {
    fn new(time: &str) -> Arc<Self> {
        Arc::new(Self(Mutex::new(time.parse().unwrap())))
    }

    fn set(&self, time: &str) {
        *self.0.lock().unwrap() = time.parse().unwrap();
    }
}

impl Clock for Manual {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

fn time(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn channel_handler(
    events: Events,
    name: &str,
    retry: Retry,
) -> (Events, mpsc::UnboundedReceiver<Event>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let events = events.handler(name, retry, move |event| {
        let sender = sender.clone();
        async move {
            sender.send(event).unwrap();
            Ok::<_, HandlerError>(())
        }
    });
    (events, receiver)
}

async fn receive(receiver: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(30), receiver.recv())
        .await
        .expect("an event within 30 seconds")
        .unwrap()
}

type Change = (Operation, Option<Json>, Option<Json>);

fn row(event: &Event) -> Change {
    match &event.cause {
        Cause::Row {
            schema,
            table,
            op,
            old,
            new,
        } => {
            assert_eq!((schema.as_str(), table.as_str()), ("public", "ev_article"));
            (*op, old.clone(), new.clone())
        }
        cause => panic!("not a row event: {cause:?}"),
    }
}

const ARTICLE: &str =
    "CREATE TABLE ev_article (id int PRIMARY KEY, title text NOT NULL, body text NOT NULL);";

const ARTICLE_SDL: &str = r#"
type ev_article @table(name: "ev_article") @primary_key(columns: ["id"]) {
  id: Int!
  title: String!
  body: String!
}
type Query { ev_article: [ev_article!]! @list }
"#;

async fn mutate(pool: &PgPool, mutation: &str) {
    let schema = rusuna::Schema::from_sdl(ARTICLE_SDL).unwrap();
    let fields = schema
        .compile(mutation, &json!({}), None, &rusuna::Session::new("admin"))
        .unwrap();
    rusuna_sqlx::execute(pool, &fields).await.unwrap();
}

async fn plain(pool: &PgPool, statement: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement.to_string()))
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn row_changes_through_rusuna_and_plain_sql_reach_the_handler() {
    let (events, mut received) = channel_handler(
        Events::new().trigger(EventTrigger::new("ev_article_changed", "ev_article")),
        "ev_article_changed",
        Retry::default(),
    );
    let pool = database("rusuna_events_rows", &events, ARTICLE).await;
    let worker = start(&events, &pool, fast(), None);
    let a = |title: &str| json!({"id": 1, "title": title, "body": "x"});
    let b = |title: &str| json!({"id": 2, "title": title, "body": "y"});
    let steps: Vec<(bool, &str, Change)> = vec![
        (
            true,
            r#"mutation { insert_ev_article_one(object: {id: 1, title: "first", body: "x"}) { id } }"#,
            (Operation::Insert, None, Some(a("first"))),
        ),
        (
            false,
            "INSERT INTO ev_article VALUES (2, 'second', 'y')",
            (Operation::Insert, None, Some(b("second"))),
        ),
        (
            true,
            r#"mutation { update_ev_article_by_pk(pk_columns: {id: 1}, _set: {title: "renamed"}) { id } }"#,
            (Operation::Update, Some(a("first")), Some(a("renamed"))),
        ),
        (
            false,
            "UPDATE ev_article SET title = 'edited' WHERE id = 2",
            (Operation::Update, Some(b("second")), Some(b("edited"))),
        ),
        (
            true,
            "mutation { delete_ev_article_by_pk(id: 1) { id } }",
            (Operation::Delete, Some(a("renamed")), None),
        ),
        (
            false,
            "DELETE FROM ev_article WHERE id = 2",
            (Operation::Delete, Some(b("edited")), None),
        ),
    ];
    for (through_rusuna, statement, expected) in steps {
        if through_rusuna {
            mutate(&pool, statement).await;
        } else {
            plain(&pool, statement).await;
        }
        let event = receive(&mut received).await;
        assert_eq!(row(&event), expected, "{statement}");
        assert_eq!(event.attempt, 1);
        assert_eq!(event.body()["event"]["op"], expected.0.as_str());
        assert_eq!(event.body()["trigger"]["name"], "ev_article_changed");
    }

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO ev_article VALUES (3, 'rolled back', 'z')")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();

    worker.stop().await;
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM rusuna.event_log").await,
        6
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'delivered' AND tries = 1"
        )
        .await,
        6
    );
    assert!(received.try_recv().is_err());
}

#[tokio::test]
async fn writers_need_only_table_privileges_and_the_worker_needs_the_rusuna_tables() {
    let events = Events::new()
        .trigger(EventTrigger::new("ev_roles", "ev_article").operations([Operation::Insert]));
    let pool = database("rusuna_events_roles", &Events::new(), "").await;
    sqlx::raw_sql(
        "DO $$ BEGIN
           CREATE ROLE rusuna_events_owner LOGIN;
           CREATE ROLE rusuna_events_writer LOGIN;
           CREATE ROLE rusuna_events_worker LOGIN;
         EXCEPTION WHEN duplicate_object THEN NULL; END $$;
         DROP SCHEMA rusuna CASCADE;
         GRANT CREATE ON DATABASE rusuna_events_roles TO rusuna_events_owner;
         GRANT CREATE ON SCHEMA public TO rusuna_events_owner;",
    )
    .execute(&pool)
    .await
    .unwrap();
    let connect = |role: &str| {
        PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options("rusuna_events_roles").username(role))
    };
    let owner = connect("rusuna_events_owner").await.unwrap();
    plain(&owner, ARTICLE).await;
    events.migrate(&owner).await.unwrap();
    plain(
        &owner,
        "GRANT INSERT ON ev_article TO rusuna_events_writer;
         GRANT USAGE ON SCHEMA rusuna TO rusuna_events_worker;
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA rusuna TO rusuna_events_worker;",
    )
    .await;
    assert!(
        scalar(
            &owner,
            "SELECT count(*) FROM pg_roles WHERE rolname = current_user AND NOT rolsuper"
        )
        .await
            == 1
    );
    let writer = connect("rusuna_events_writer").await.unwrap();
    plain(&writer, "INSERT INTO ev_article VALUES (1, 'a', 'x')").await;
    assert!(
        sqlx::raw_sql("SELECT * FROM rusuna.event_log")
            .execute(&writer)
            .await
            .is_err()
    );
    let worker_pool = connect("rusuna_events_worker").await.unwrap();
    let (events, mut received) = channel_handler(events, "ev_roles", Retry::default());
    let worker = start(&events, &worker_pool, fast(), None);
    let event = receive(&mut received).await;
    assert_eq!(row(&event).0, Operation::Insert);
    until("the recorded delivery", || async {
        scalar(
            &owner,
            "SELECT count(*) FROM rusuna.event_invocation WHERE succeeded",
        )
        .await
            == 1
    })
    .await;
    let monitor = worker.monitor.clone();
    worker.stop().await;
    assert_eq!(monitor.stats().errors, 0);
    rusuna_events::schedule(&worker_pool, "ev_roles", Utc::now(), Json::Null)
        .await
        .unwrap();
    rusuna_events::cleanup(&worker_pool, Utc::now())
        .await
        .unwrap();
}

#[tokio::test]
async fn update_columns_and_operations_select_the_changes_that_record_events() {
    let events = Events::new()
        .trigger(
            EventTrigger::new("ev_title", "ev_article")
                .operations([Operation::Update])
                .columns(["title"]),
        )
        .trigger(EventTrigger::new("ev_deleted", "ev_article").operations([Operation::Delete]));
    let pool = database("rusuna_events_columns", &events, ARTICLE).await;
    for statement in [
        "INSERT INTO ev_article VALUES (1, 'a', 'x'), (2, 'b', 'y')",
        "UPDATE ev_article SET body = 'changed'",
        "UPDATE ev_article SET title = title",
        "UPDATE ev_article SET title = 'renamed', body = 'again' WHERE id = 2",
        "DELETE FROM ev_article WHERE id = 1",
    ] {
        sqlx::raw_sql(statement).execute(&pool).await.unwrap();
    }
    let rows: Vec<(String, String, Json)> = sqlx::query_as(
        "SELECT trigger_name, operation, coalesce(new, old) FROM rusuna.event_log ORDER BY created_at, trigger_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "ev_title".into(),
                "UPDATE".into(),
                json!({"id": 2, "title": "renamed", "body": "again"})
            ),
            (
                "ev_deleted".into(),
                "DELETE".into(),
                json!({"id": 1, "title": "a", "body": "changed"})
            ),
        ]
    );
}

#[tokio::test]
async fn failed_attempts_are_retried_after_the_interval() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let seen = attempts.clone();
    let retry = Retry {
        max_retries: 3,
        interval: Duration::from_millis(300),
        timeout: Duration::from_secs(10),
    };
    let events = Events::new()
        .trigger(EventTrigger::new("ev_retry", "ev_article").operations([Operation::Insert]))
        .handler("ev_retry", retry, move |event: Event| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push((event.attempt, Instant::now()));
                if event.attempt <= 2 {
                    return Err(format!("attempt {} fails", event.attempt).into());
                }
                Ok(())
            }
        });
    let pool = database("rusuna_events_retry", &events, ARTICLE).await;
    let worker = start(&events, &pool, fast(), None);
    sqlx::query("INSERT INTO ev_article VALUES (1, 'a', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    until("delivery", || async {
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'delivered'",
        )
        .await
            == 1
    })
    .await;
    worker.stop().await;
    let attempts = attempts.lock().unwrap().clone();
    assert_eq!(
        attempts.iter().map(|a| a.0).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for pair in attempts.windows(2) {
        assert!(pair[1].1 - pair[0].1 >= retry.interval);
    }
    let invocations: Vec<(i32, bool, Option<String>)> = sqlx::query_as(
        "SELECT attempt, succeeded, error FROM rusuna.event_invocation ORDER BY attempt",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        invocations,
        vec![
            (1, false, Some("attempt 1 fails".into())),
            (2, false, Some("attempt 2 fails".into())),
            (3, true, None),
        ]
    );
}

#[tokio::test]
async fn attempts_stop_at_the_retry_limit_until_redelivered() {
    let calls = Arc::new(AtomicU32::new(0));
    let counted = calls.clone();
    let retry = Retry {
        max_retries: 2,
        interval: Duration::from_millis(50),
        timeout: Duration::from_millis(200),
    };
    let events = Events::new()
        .trigger(EventTrigger::new("ev_limit", "ev_article").operations([Operation::Insert]))
        .handler("ev_limit", retry, move |event: Event| {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                if event.attempt == 2 {
                    std::future::pending::<()>().await;
                }
                if event.attempt == 3 {
                    panic!("the third attempt panics");
                }
                Err("always fails".into())
            }
        });
    let pool = database("rusuna_events_limit", &events, ARTICLE).await;
    let worker = start(&events, &pool, fast(), None);
    sqlx::query("INSERT INTO ev_article VALUES (1, 'a', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    until("failure", || async {
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'failed'",
        )
        .await
            == 1
    })
    .await;
    polls(&worker.monitor, 10).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let errors: Vec<Option<String>> =
        sqlx::query_scalar("SELECT error FROM rusuna.event_invocation ORDER BY attempt")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        errors,
        vec![
            Some("always fails".into()),
            Some("the handler did not finish in 200ms".into()),
            Some("the handler panicked".into()),
        ]
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT tries::int8 FROM rusuna.event_log WHERE completed_at IS NOT NULL AND locked_until IS NULL"
        )
        .await,
        3
    );

    let id: String = sqlx::query_scalar("SELECT id::text FROM rusuna.event_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(rusuna_events::redeliver(&pool, &id).await.unwrap());
    assert!(!rusuna_events::redeliver(&pool, &id).await.unwrap());
    until("the redelivered attempts", || async {
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'failed'",
        )
        .await
            == 1
            && calls.load(Ordering::SeqCst) == 6
    })
    .await;
    worker.stop().await;
}

#[tokio::test]
async fn webhooks_receive_the_payload_and_headers_and_are_retried() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let app = {
        let received = received.clone();
        axum::Router::new().route(
            "/hook",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Json>| {
                    let received = received.clone();
                    async move {
                        let mut received = received.lock().unwrap();
                        received.push((
                            headers
                                .get("x-secret")
                                .and_then(|v| v.to_str().ok())
                                .map(String::from),
                            body,
                        ));
                        if received.len() <= 2 {
                            (axum::http::StatusCode::SERVICE_UNAVAILABLE, "not yet")
                        } else {
                            (axum::http::StatusCode::OK, "ok")
                        }
                    }
                },
            ),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let events = Events::new()
        .trigger(EventTrigger::new("ev_hook", "ev_article").operations([Operation::Insert]))
        .webhook(
            "ev_hook",
            Retry {
                max_retries: 2,
                interval: Duration::from_millis(100),
                timeout: Duration::from_secs(10),
            },
            Webhook::new(&format!("http://{address}/hook"))
                .unwrap()
                .header("x-secret", "s3cret")
                .unwrap(),
        );
    let pool = database("rusuna_events_webhook", &events, ARTICLE).await;
    let worker = start(&events, &pool, fast(), None);
    sqlx::query("INSERT INTO ev_article VALUES (7, 'hooked', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    until("delivery", || async {
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'delivered'",
        )
        .await
            == 1
    })
    .await;
    worker.stop().await;
    server.abort();
    let received = received.lock().unwrap().clone();
    assert_eq!(received.len(), 3);
    let id: String = sqlx::query_scalar("SELECT id::text FROM rusuna.event_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    for (retry, (secret, body)) in received.iter().enumerate() {
        assert_eq!(secret.as_deref(), Some("s3cret"));
        assert_eq!(body["id"], id);
        assert_eq!(body["trigger"], json!({"name": "ev_hook"}));
        assert_eq!(
            body["table"],
            json!({"schema": "public", "name": "ev_article"})
        );
        assert_eq!(
            body["event"],
            json!({"op": "INSERT", "data": {"old": null, "new": {"id": 7, "title": "hooked", "body": "x"}}})
        );
        assert_eq!(
            body["delivery_info"],
            json!({"max_retries": 2, "current_retry": retry})
        );
    }
    let statuses: Vec<(Option<i32>, bool)> =
        sqlx::query_as("SELECT status, succeeded FROM rusuna.event_invocation ORDER BY attempt")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![(Some(503), false), (Some(503), false), (Some(200), true)]
    );
}

#[tokio::test]
async fn two_workers_deliver_each_event_once() {
    let counts = Arc::new(Mutex::new(HashMap::<String, u32>::new()));
    let counted = counts.clone();
    let events = Events::new()
        .trigger(EventTrigger::new("ev_pair", "ev_article").operations([Operation::Insert]))
        .handler("ev_pair", Retry::default(), move |event: Event| {
            let counted = counted.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                *counted.lock().unwrap().entry(event.id).or_default() += 1;
                Ok(())
            }
        });
    let pool = database("rusuna_events_pair", &events, ARTICLE).await;
    let options = WorkerOptions {
        concurrency: 4,
        ..fast()
    };
    let first = start(&events, &pool, options, None);
    let second = start(&events, &pool, options, None);
    sqlx::query("INSERT INTO ev_article SELECT i, 't' || i, 'b' FROM generate_series(1, 200) AS i")
        .execute(&pool)
        .await
        .unwrap();
    until("all deliveries", || async {
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'delivered'",
        )
        .await
            == 200
    })
    .await;
    let monitors = [first.monitor.clone(), second.monitor.clone()];
    first.stop().await;
    second.stop().await;
    let [a, b] = monitors.map(|monitor| monitor.stats());
    let counts = counts.lock().unwrap().clone();
    assert_eq!(counts.len(), 200);
    assert!(counts.values().all(|count| *count == 1));
    assert_eq!(a.delivered + b.delivered, 200);
    assert_eq!(a.claimed + b.claimed, 200);
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM rusuna.event_invocation").await,
        200
    );
}

#[tokio::test]
async fn an_aborted_delivery_is_claimed_again_after_its_lease() {
    let clock = Manual::new("2026-01-01T00:00:00Z");
    let (started, mut starts) = mpsc::unbounded_channel();
    let trigger = EventTrigger::new("ev_crash", "ev_article").operations([Operation::Insert]);
    let retry = Retry {
        max_retries: 0,
        interval: Duration::from_secs(10),
        timeout: Duration::from_secs(60),
    };
    let stuck =
        Events::new()
            .trigger(trigger.clone())
            .handler("ev_crash", retry, move |event: Event| {
                let started = started.clone();
                async move {
                    started.send(event.attempt).unwrap();
                    std::future::pending::<()>().await;
                    Ok(())
                }
            });
    let (events, mut received) = channel_handler(Events::new().trigger(trigger), "ev_crash", retry);
    let pool = database("rusuna_events_abort", &events, ARTICLE).await;
    let options = WorkerOptions {
        lease_margin: Duration::from_secs(30),
        ..fast()
    };
    let crashing = start(&stuck, &pool, options, Some(clock.clone()));
    sqlx::query("INSERT INTO ev_article VALUES (1, 'a', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(starts.recv().await, Some(1));
    crashing.task.abort();
    assert!(crashing.task.await.unwrap_err().is_cancelled());

    let worker = start(&events, &pool, options, Some(clock.clone()));
    polls(&worker.monitor, 3).await;
    assert!(received.try_recv().is_err());
    assert_eq!(worker.monitor.stats().claimed, 0);
    clock.set("2026-01-01T00:01:29Z");
    polls(&worker.monitor, 3).await;
    assert_eq!(worker.monitor.stats().claimed, 0);
    clock.set("2026-01-01T00:01:31Z");
    let event = receive(&mut received).await;
    assert_eq!(event.attempt, 2);
    until("the recorded delivery", || async {
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'delivered' AND tries = 2",
        )
        .await
            == 1
    })
    .await;
    worker.stop().await;
}

#[tokio::test]
async fn a_cron_occurrence_fires_once_across_workers() {
    let clock = Manual::new("2026-01-01T00:04:30Z");
    let (events, mut received) = channel_handler(
        Events::new()
            .cron(CronTrigger::new("ev_cron", "*/5 * * * *", json!({"job": "report"})).unwrap()),
        "ev_cron",
        Retry::default(),
    );
    let pool = database("rusuna_events_cron", &events, "").await;
    let workers = [
        start(&events, &pool, fast(), Some(clock.clone())),
        start(&events, &pool, fast(), Some(clock.clone())),
    ];
    for worker in &workers {
        polls(&worker.monitor, 1).await;
    }
    clock.set("2026-01-01T00:05:30Z");
    let event = receive(&mut received).await;
    assert_eq!(
        event.cause,
        Cause::Cron {
            scheduled_at: time("2026-01-01T00:05:00Z")
        }
    );
    assert_eq!(event.payload, json!({"job": "report"}));
    assert_eq!(
        event.body()["scheduled_time"],
        "2026-01-01T00:05:00.000000Z"
    );
    clock.set("2026-01-01T00:09:59Z");
    for worker in &workers {
        polls(&worker.monitor, 3).await;
    }
    assert!(received.try_recv().is_err());
    clock.set("2026-01-01T00:10:00Z");
    let event = receive(&mut received).await;
    assert_eq!(
        event.cause,
        Cause::Cron {
            scheduled_at: time("2026-01-01T00:10:00Z")
        }
    );
    for worker in &workers {
        polls(&worker.monitor, 3).await;
    }
    assert!(received.try_recv().is_err());
    for worker in workers {
        worker.stop().await;
    }
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE kind = 'cron' AND status = 'delivered'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn a_started_worker_records_the_missed_occurrences_within_the_tolerance() {
    let clock = Manual::new("2026-01-01T00:20:30Z");
    let (events, mut received) = channel_handler(
        Events::new().cron(
            CronTrigger::new("ev_missed", "*/5 * * * *", Json::Null)
                .unwrap()
                .tolerance(Duration::from_secs(12 * 60)),
        ),
        "ev_missed",
        Retry::default(),
    );
    let pool = database("rusuna_events_missed", &events, "").await;
    sqlx::query("INSERT INTO rusuna.event_log (kind, trigger_name, scheduled_at, status, completed_at) VALUES ('cron', 'ev_missed', '2026-01-01T00:00:00Z', 'delivered', '2026-01-01T00:00:01Z')")
        .execute(&pool)
        .await
        .unwrap();
    let worker = start(&events, &pool, fast(), Some(clock.clone()));
    let mut times = Vec::new();
    for _ in 0..3 {
        match receive(&mut received).await.cause {
            Cause::Cron { scheduled_at } => times.push(scheduled_at),
            cause => panic!("not a cron event: {cause:?}"),
        }
    }
    times.sort();
    assert_eq!(
        times,
        vec![
            time("2026-01-01T00:10:00Z"),
            time("2026-01-01T00:15:00Z"),
            time("2026-01-01T00:20:00Z")
        ]
    );
    polls(&worker.monitor, 3).await;
    assert!(received.try_recv().is_err());
    worker.stop().await;

    let fresh = Manual::new("2026-01-01T00:20:30Z");
    let (events, mut received) = channel_handler(
        Events::new().cron(CronTrigger::new("ev_new", "*/5 * * * *", Json::Null).unwrap()),
        "ev_new",
        Retry::default(),
    );
    let worker = start(&events, &pool, fast(), Some(fresh.clone()));
    polls(&worker.monitor, 3).await;
    assert!(received.try_recv().is_err());
    fresh.set("2026-01-01T00:25:00Z");
    let event = receive(&mut received).await;
    assert_eq!(
        event.cause,
        Cause::Cron {
            scheduled_at: time("2026-01-01T00:25:00Z")
        }
    );
    worker.stop().await;
}

#[tokio::test]
async fn a_scheduled_event_is_delivered_at_its_time() {
    let clock = Manual::new("2026-01-01T00:00:00Z");
    let (events, mut received) = channel_handler(Events::new(), "ev_once", Retry::default());
    let pool = database("rusuna_events_once", &events, "").await;
    let mut transaction = pool.begin().await.unwrap();
    rusuna_events::schedule(
        &mut *transaction,
        "ev_once",
        time("2026-01-01T01:00:00Z"),
        json!({"remind": 1}),
    )
    .await
    .unwrap();
    transaction.rollback().await.unwrap();
    let id = rusuna_events::schedule(
        &pool,
        "ev_once",
        time("2026-01-01T01:00:00Z"),
        json!({"remind": 2}),
    )
    .await
    .unwrap();
    let worker = start(&events, &pool, fast(), Some(clock.clone()));
    polls(&worker.monitor, 3).await;
    assert!(received.try_recv().is_err());
    clock.set("2026-01-01T01:00:00Z");
    let event = receive(&mut received).await;
    assert_eq!(event.id, id);
    assert_eq!(
        event.body(),
        json!({"id": id, "name": "ev_once", "scheduled_time": "2026-01-01T01:00:00.000000Z", "payload": {"remind": 2}})
    );
    polls(&worker.monitor, 3).await;
    assert!(received.try_recv().is_err());
    worker.stop().await;
}

#[tokio::test]
async fn shutdown_waits_for_the_deliveries_in_progress() {
    let (started, mut starts) = mpsc::unbounded_channel();
    let events = Events::new()
        .trigger(EventTrigger::new("ev_shutdown", "ev_article").operations([Operation::Insert]))
        .handler("ev_shutdown", Retry::default(), move |_event: Event| {
            let started = started.clone();
            async move {
                started.send(()).unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(())
            }
        });
    let pool = database("rusuna_events_shutdown", &events, ARTICLE).await;
    let worker = start(&events, &pool, fast(), None);
    sqlx::query("INSERT INTO ev_article VALUES (1, 'a', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    starts.recv().await.unwrap();
    worker.stop().await;
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE status = 'delivered'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn the_migration_is_idempotent_and_replaces_changed_triggers() {
    let events = Events::new().trigger(
        EventTrigger::new("ev_migrated", "ev_article")
            .operations([Operation::Update])
            .columns(["title"]),
    );
    let pool = database("rusuna_events_migration", &events, ARTICLE).await;
    events.migrate(&pool).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(events.migration().unwrap()))
        .execute(&pool)
        .await
        .unwrap();
    let changed = Events::new()
        .trigger(EventTrigger::new("ev_migrated", "ev_article").operations([Operation::Insert]));
    changed.migrate(&pool).await.unwrap();
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM pg_trigger WHERE tgname LIKE 'rusuna_event_%'"
        )
        .await,
        1
    );
    sqlx::raw_sql("INSERT INTO ev_article VALUES (1, 'a', 'x'); UPDATE ev_article SET title = 'b'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM rusuna.event_log WHERE operation = 'INSERT'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM rusuna.event_log").await,
        1
    );
}

#[tokio::test]
async fn cleanup_deletes_old_delivered_events_and_keeps_the_latest_cron_occurrence() {
    let events = Events::new();
    let pool = database("rusuna_events_cleanup", &events, "").await;
    sqlx::raw_sql(
        "INSERT INTO rusuna.event_log (kind, trigger_name, scheduled_at, status, completed_at) VALUES
           ('cron', 'c', '2026-01-01T00:00:00Z', 'delivered', '2026-01-01T00:00:00Z'),
           ('cron', 'c', '2026-01-01T00:05:00Z', 'delivered', '2026-01-01T00:05:00Z'),
           ('scheduled', 's', '2026-01-01T00:00:00Z', 'delivered', '2026-01-01T00:00:00Z'),
           ('scheduled', 's', '2026-01-01T00:00:00Z', 'failed', '2026-01-01T00:00:00Z'),
           ('scheduled', 's', '2026-01-01T00:00:00Z', 'delivered', '2026-03-01T00:00:00Z'),
           ('scheduled', 's', '2026-01-01T00:00:00Z', 'pending', NULL);
         INSERT INTO rusuna.event_invocation (event_id, attempt, succeeded, started_at, finished_at)
           SELECT id, 1, true, now(), now() FROM rusuna.event_log",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        rusuna_events::cleanup(&pool, time("2026-02-01T00:00:00Z"))
            .await
            .unwrap(),
        2
    );
    let rest: Vec<(String, String)> = sqlx::query_as(
        "SELECT kind, status FROM rusuna.event_log ORDER BY kind, status, completed_at",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rest,
        vec![
            ("cron".into(), "delivered".into()),
            ("scheduled".into(), "delivered".into()),
            ("scheduled".into(), "failed".into()),
            ("scheduled".into(), "pending".into()),
        ]
    );
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM rusuna.event_invocation").await,
        4
    );
}

#[tokio::test]
async fn workers_reject_incomplete_definitions() {
    let pool = PgPoolOptions::new().connect_lazy_with(options("postgres"));
    let handler = |_event: Event| async { Ok::<_, HandlerError>(()) };
    let cron = || CronTrigger::new("c", "* * * * *", Json::Null).unwrap();
    let cases = [
        Events::new().cron(cron()),
        Events::new()
            .cron(cron())
            .cron(cron())
            .handler("c", Retry::default(), handler),
        Events::new()
            .handler("h", Retry::default(), handler)
            .handler("h", Retry::default(), handler),
        Events::new().handler("bad name", Retry::default(), handler),
        Events::new().trigger(EventTrigger::new("t", "a").operations([])),
    ];
    for events in cases {
        assert!(
            events
                .worker(pool.clone(), WorkerOptions::default())
                .is_err()
        );
    }
    assert!(
        Events::new()
            .handler("h", Retry::default(), handler)
            .worker(
                pool,
                WorkerOptions {
                    concurrency: 0,
                    ..WorkerOptions::default()
                }
            )
            .is_err()
    );
}
