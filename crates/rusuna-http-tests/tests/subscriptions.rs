#[path = "../../../tests/common/mod.rs"]
mod common;
mod support;

use futures_util::{SinkExt, StreamExt};
use rusuna_sqlx::{Multiplexed, SubscriptionOptions, SubscriptionStats, Subscriptions};
use serde_json::{Value as Json, json};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use support::*;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const WAIT: Duration = Duration::from_secs(20);

fn options() -> SubscriptionOptions {
    SubscriptionOptions {
        interval: Duration::from_millis(200),
        batch_size: 100,
    }
}

fn role_schemas(
    sdl: &str,
    pool: &PgPool,
    subscriptions: &Subscriptions,
) -> HashMap<String, async_graphql::dynamic::Schema> {
    let schema = Arc::new(rusuna::Schema::from_sdl(sdl).unwrap());
    schema
        .roles()
        .into_iter()
        .chain(["visitor".to_string()])
        .map(|role| {
            let parts = rusuna_async_graphql::Builder::new(schema.clone(), &role)
                .build()
                .unwrap();
            let built = parts
                .query_field(async_graphql::dynamic::Field::new(
                    "viewer",
                    async_graphql::dynamic::TypeRef::named(async_graphql::dynamic::TypeRef::INT),
                    |_| {
                        async_graphql::dynamic::FieldFuture::new(async {
                            Ok(None::<async_graphql::Value>)
                        })
                    },
                ))
                .schema()
                .data(pool.clone())
                .data(subscriptions.clone())
                .finish()
                .unwrap();
            (role, built)
        })
        .collect()
}

async fn serve_ws(schemas: HashMap<String, async_graphql::dynamic::Schema>) -> SocketAddr {
    let app = axum::Router::new().route(
        "/graphql",
        rusuna_axum::graphql_ws(schemas, |_: &axum::http::request::Parts, payload: Json| {
            let role = payload["role"].as_str().map(str::to_string);
            let session = payload.get("session").cloned().unwrap_or(json!({}));
            async move {
                let role = role.ok_or_else(|| "the payload names no role".to_string())?;
                rusuna::Session::from_json(role, &session).map_err(|e| e.to_string())
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

struct Client {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Client {
    async fn connect(address: SocketAddr, payload: Json) -> Self {
        let mut request = format!("ws://{address}/graphql")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            "graphql-transport-ws".parse().unwrap(),
        );
        let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let mut client = Self { socket };
        client
            .send(json!({"type": "connection_init", "payload": payload}))
            .await;
        assert_eq!(client.next().await, json!({"type": "connection_ack"}));
        client
    }

    async fn send(&mut self, message: Json) {
        self.socket
            .send(Message::text(message.to_string()))
            .await
            .unwrap();
    }

    async fn subscribe(&mut self, id: &str, query: &str, variables: Json) {
        self.send(json!({
            "id": id,
            "type": "subscribe",
            "payload": {"query": query, "variables": variables},
        }))
        .await;
    }

    async fn read(&mut self, timeout: Duration) -> Option<Json> {
        loop {
            let message = tokio::time::timeout(timeout, self.socket.next())
                .await
                .ok()??
                .unwrap();
            match message {
                Message::Text(text) => return Some(serde_json::from_str(&text).unwrap()),
                Message::Close(_) => return None,
                _ => {}
            }
        }
    }

    async fn next(&mut self) -> Json {
        self.read(WAIT).await.expect("no message arrived")
    }

    async fn failure(&mut self, id: &str) -> String {
        let message = self.next().await;
        assert_eq!(message["id"], id, "{message}");
        let errors = match message["type"].as_str() {
            Some("error") => &message["payload"],
            Some("next") => {
                assert_eq!(self.next().await, json!({"type": "complete", "id": id}));
                &message["payload"]["errors"]
            }
            _ => panic!("{message}"),
        };
        errors[0]["message"].as_str().unwrap().to_string()
    }

    async fn data(&mut self, id: &str) -> Json {
        let message = self.next().await;
        assert_eq!(message["type"], "next", "{message}");
        assert_eq!(message["id"], id, "{message}");
        assert!(message["payload"].get("errors").is_none(), "{message}");
        message["payload"]["data"].clone()
    }

    async fn data_batch(&mut self, ids: &[&str]) -> HashMap<String, Json> {
        let mut results = HashMap::new();
        for _ in ids {
            let message = self.next().await;
            assert_eq!(message["type"], "next", "{message}");
            let id = message["id"].as_str().unwrap();
            assert!(ids.contains(&id), "{message}");
            assert!(message["payload"].get("errors").is_none(), "{message}");
            assert!(
                results
                    .insert(id.to_string(), message["payload"]["data"].clone())
                    .is_none(),
                "duplicate response: {message}"
            );
        }
        results
    }
}

async fn direct(
    sdl: &str,
    pool: &PgPool,
    query: &str,
    variables: &Json,
    session: &rusuna::Session,
) -> Json {
    let schema = rusuna::Schema::from_sdl(sdl).unwrap();
    let fields = schema.compile(query, variables, None, session).unwrap();
    Json::Object(rusuna_sqlx::execute(pool, &fields).await.unwrap())
}

async fn statement_calls(pool: &PgPool) -> (i64, i64) {
    let row = sqlx::query(
        "SELECT coalesce(sum(calls) FILTER (WHERE query LIKE '%__rusuna_subscription%'), 0)::int8, coalesce(sum(calls) FILTER (WHERE query NOT LIKE '%__rusuna_subscription%'), 0)::int8 FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) AND query NOT LIKE '%pg_stat_statements%'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    (row.get(0), row.get(1))
}

async fn snapshot(pool: &PgPool, subscriptions: &Subscriptions) -> (SubscriptionStats, (i64, i64)) {
    loop {
        let before = subscriptions.stats();
        let calls = statement_calls(pool).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let after = subscriptions.stats();
        if before == after && calls == statement_calls(pool).await {
            return (after, calls);
        }
    }
}

async fn until(subscriptions: &Subscriptions, condition: impl Fn(SubscriptionStats) -> bool) {
    tokio::time::timeout(WAIT, async {
        while !condition(subscriptions.stats()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the subscription counters did not reach the expected state");
}

async fn database_with_statistics(name: &str) -> PgPool {
    let pool = database(name, &["schema.sql", "seed.sql"]).await;
    sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

#[tokio::test(flavor = "multi_thread")]
async fn subscriptions_send_changed_results_until_they_end() {
    let pool = database("http_subscription_changes", &["schema.sql", "seed.sql"]).await;
    let sdl = fixture("permissions.graphql");
    let subscriptions = Subscriptions::new(pool.clone(), options());
    let address = serve_ws(role_schemas(&sdl, &pool, &subscriptions)).await;
    let query = "subscription { article(order_by: {id: asc}) { id title } }";
    let session = rusuna::Session::new("user").variable("user_id", rusuna::Value::Int(3));
    let mut client =
        Client::connect(address, json!({"role": "user", "session": {"user_id": 3}})).await;
    client.subscribe("1", query, json!({})).await;
    let first = client.data("1").await;
    assert_eq!(
        first,
        direct(&sdl, &pool, query, &json!({}), &session).await
    );
    assert_eq!(first["article"].as_array().unwrap().len(), 4);

    let mutation = rusuna::Schema::from_sdl(&sdl)
        .unwrap()
        .compile(
            "mutation { update_article(where: {id: {_eq: 4}}, _set: {title: \"renamed by rusuna\"}) { affected_rows } }",
            &json!({}),
            None,
            &rusuna::Session::new("admin"),
        )
        .unwrap();
    rusuna_sqlx::execute(&pool, &mutation).await.unwrap();
    let second = client.data("1").await;
    assert_eq!(
        second["article"][1],
        json!({"id": 4, "title": "renamed by rusuna"})
    );

    sqlx::query("UPDATE article SET title = 'renamed by SQL' WHERE id = 9")
        .execute(&pool)
        .await
        .unwrap();
    let third = client.data("1").await;
    assert_eq!(
        third["article"][3],
        json!({"id": 9, "title": "renamed by SQL"})
    );
    assert_eq!(
        third,
        direct(&sdl, &pool, query, &json!({}), &session).await
    );

    let polls = subscriptions.stats().polls;
    sqlx::query("UPDATE article SET title = title")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE article SET title = 'hidden from user 3' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    until(&subscriptions, |stats| stats.polls >= polls + 3).await;
    assert_eq!(client.read(Duration::from_millis(100)).await, None);

    assert_eq!(subscriptions.stats().groups, 1);
    client.send(json!({"id": "1", "type": "complete"})).await;
    assert_eq!(client.next().await, json!({"type": "complete", "id": "1"}));
    until(&subscriptions, |stats| stats.groups == 0).await;
    let stopped = subscriptions.stats();
    assert_eq!(stopped.subscriptions, 0);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(subscriptions.stats(), stopped);

    client.subscribe("2", query, json!({})).await;
    assert_eq!(client.data("2").await, third);
    assert_eq!(subscriptions.stats().subscriptions, 1);
    drop(client);
    until(&subscriptions, |stats| stats.groups == 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hundred_subscriptions_are_updated_by_one_statement() {
    let pool = database_with_statistics("http_subscription_hundred").await;
    let sdl = fixture("permissions.graphql");
    let subscriptions = Subscriptions::new(
        pool.clone(),
        SubscriptionOptions {
            interval: Duration::from_millis(500),
            batch_size: 100,
        },
    );
    let address = serve_ws(role_schemas(&sdl, &pool, &subscriptions)).await;
    let query = "subscription($min: Int!) { article(where: {id: {_gte: $min}}, order_by: {id: asc}) { id author_id title } }";
    let mut clients = Vec::new();
    for i in 0..100 {
        let user = i % 4 + 1;
        let min = i % 7;
        let mut client = Client::connect(
            address,
            json!({"role": "user", "session": {"user_id": user}}),
        )
        .await;
        client.subscribe("s", query, json!({"min": min})).await;
        clients.push((user, min, client));
    }
    for (user, min, client) in &mut clients {
        let data = client.data("s").await;
        let session = rusuna::Session::new("user").variable("user_id", rusuna::Value::Int(*user));
        assert_eq!(
            data,
            direct(&sdl, &pool, query, &json!({"min": min}), &session).await
        );
        for row in data["article"].as_array().unwrap() {
            assert_eq!(row["author_id"], json!(user));
            assert!(row["id"].as_i64().unwrap() >= *min);
        }
    }
    until(&subscriptions, |stats| {
        stats.groups == 1 && stats.subscriptions == 100
    })
    .await;

    let (before, (multiplexed_before, other_before)) = snapshot(&pool, &subscriptions).await;
    sqlx::query("UPDATE article SET title = 'changed' WHERE id = 6")
        .execute(&pool)
        .await
        .unwrap();
    for (user, min, client) in &mut clients {
        if *user == 4 && *min <= 6 {
            let data = client.data("s").await;
            assert_eq!(
                data["article"][0],
                json!({"id": 6, "author_id": 4, "title": "changed"})
            );
        }
    }
    let (after, (multiplexed_after, other_after)) = snapshot(&pool, &subscriptions).await;
    for (_, _, client) in &mut clients {
        assert_eq!(client.read(Duration::from_millis(1)).await, None);
    }
    let polls = after.polls - before.polls;
    assert!(polls >= 1);
    assert_eq!(after.statements - before.statements, polls);
    assert_eq!((multiplexed_after - multiplexed_before) as u64, polls);
    assert_eq!(other_after - other_before, 1);
    println!(
        "{polls} polls of 100 subscriptions ran {} statements",
        multiplexed_after - multiplexed_before
    );

    clients.truncate(10);
    until(&subscriptions, |stats| stats.subscriptions == 10).await;
    drop(clients);
    until(&subscriptions, |stats| stats.groups == 0).await;
    let stopped = snapshot(&pool, &subscriptions).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(snapshot(&pool, &subscriptions).await, stopped);
}

#[tokio::test]
async fn groups_follow_statements_roles_and_the_batch_size() {
    let pool = database_with_statistics("http_subscription_groups").await;
    let schema = rusuna::Schema::from_sdl(&fixture("permissions.graphql")).unwrap();
    let options = options();
    let subscriptions = Subscriptions::new(pool.clone(), options);
    let (resume, paused) = std::sync::mpsc::channel();
    let clock = tokio::task::spawn_blocking(move || paused.recv_timeout(WAIT));
    tokio::time::pause();
    let compile = |query: &str, role: &str, user: i64| {
        let mut session = rusuna::Session::new(role);
        if role == "user" {
            session = session.variable("user_id", rusuna::Value::Int(user));
        }
        schema
            .compile(query, &json!({}), None, &session)
            .unwrap()
            .remove(0)
            .compiled
    };
    let articles = "{ article(order_by: {id: asc}) { id } }";
    let mut members = Vec::new();
    for user in 0..101 {
        members.push(
            subscriptions
                .subscribe("user", &compile(articles, "user", user % 5 + 1))
                .await
                .unwrap(),
        );
    }
    for (user, member) in members.iter_mut().enumerate() {
        let ids: Vec<i64> = match member.next().await.unwrap().unwrap() {
            Json::Array(rows) => rows.iter().map(|r| r["id"].as_i64().unwrap()).collect(),
            other => panic!("{other}"),
        };
        let expected: Vec<i64> = match user % 5 + 1 {
            1 => vec![1, 7, 12],
            2 => vec![2, 8],
            3 => vec![3, 4, 5, 9],
            4 => vec![6],
            _ => vec![10, 11],
        };
        assert_eq!(ids, expected);
    }
    let before = subscriptions.stats();
    assert_eq!(before.groups, 1);
    let (multiplexed_before, _) = statement_calls(&pool).await;
    for poll in 1..=2 {
        tokio::time::advance(options.interval + Duration::from_millis(1)).await;
        while subscriptions.stats().polls < before.polls + poll {
            assert!(!clock.is_finished(), "the subscription poll did not finish");
            tokio::task::yield_now().await;
        }
    }
    let after = subscriptions.stats();
    let (multiplexed_after, _) = statement_calls(&pool).await;
    tokio::time::resume();
    resume.send(()).unwrap();
    clock.await.unwrap().unwrap();
    assert_eq!(
        after.statements - before.statements,
        2 * (after.polls - before.polls)
    );
    assert_eq!(
        (multiplexed_after - multiplexed_before) as u64,
        after.statements - before.statements
    );

    let same_text = compile(articles, "user", 3);
    let mut other_role = subscriptions
        .subscribe("auditor", &same_text)
        .await
        .unwrap();
    let mut reviewer = subscriptions
        .subscribe("reviewer", &compile(articles, "reviewer", 0))
        .await
        .unwrap();
    let mut other_query = subscriptions
        .subscribe("user", &compile("{ article { title } }", "user", 3))
        .await
        .unwrap();
    assert_eq!(subscriptions.stats().groups, 4);
    assert_eq!(
        other_role.next().await.unwrap().unwrap(),
        json!([{"id": 3}, {"id": 4}, {"id": 5}, {"id": 9}])
    );
    assert_eq!(
        reviewer.next().await.unwrap().unwrap(),
        json!([{"id": 1}, {"id": 2}, {"id": 3}, {"id": 4}, {"id": 5}])
    );
    assert_eq!(
        other_query
            .next()
            .await
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );

    let mut invalid = compile(articles, "user", 3);
    invalid.params = vec![rusuna::Value::Text("three".into())];
    assert!(matches!(
        subscriptions.subscribe("user", &invalid).await,
        Err(rusuna_sqlx::Error::Parameter { index: 0, .. })
    ));
    assert_eq!(subscriptions.stats().subscriptions, 104);
    drop(members);
    assert_eq!(subscriptions.stats().groups, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn roles_without_read_permission_have_no_subscription_fields() {
    let pool = database("http_subscription_roles", &["schema.sql", "seed.sql"]).await;
    let sdl = fixture("permissions.graphql");
    let subscriptions = Subscriptions::new(pool.clone(), options());
    let schemas = role_schemas(&sdl, &pool, &subscriptions);
    assert!(!schemas["visitor"].sdl().contains("type Subscription"));
    let address = serve_ws(schemas).await;
    let mut visitor = Client::connect(address, json!({"role": "visitor"})).await;
    visitor
        .subscribe("1", "subscription { article { id } }", json!({}))
        .await;
    assert_eq!(visitor.failure("1").await, "Subscription root not found");
    let mut reviewer = Client::connect(address, json!({"role": "reviewer"})).await;
    reviewer
        .subscribe("1", "subscription { comment { id } }", json!({}))
        .await;
    assert!(reviewer.failure("1").await.contains("comment"));
    reviewer
        .subscribe(
            "2",
            "subscription { article(order_by: {id: asc}) { id author_id } }",
            json!({}),
        )
        .await;
    assert!(reviewer.failure("2").await.contains("author_id"));
    assert_eq!(subscriptions.stats().subscriptions, 0);
}

async fn multiplexed_matches(
    pool: &PgPool,
    compiled: &[rusuna::Compiled],
    unordered: &[&str],
    name: &str,
) {
    let mut conn = pool.acquire().await.unwrap();
    let multiplexed = Multiplexed::prepare(&mut conn, &compiled[0]).await.unwrap();
    let arguments: Vec<Json> = compiled
        .iter()
        .map(|c| multiplexed.arguments(&c.params).unwrap())
        .collect();
    let mut expected = Vec::new();
    for c in compiled {
        assert_eq!(c.sql, compiled[0].sql, "{name}");
        expected.push(sorted(
            rusuna_sqlx::fetch(&mut conn, c).await.unwrap(),
            unordered,
        ));
    }
    for mode in ["auto", "force_generic_plan", "force_generic_plan"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET plan_cache_mode = {mode}")))
            .execute(&mut *conn)
            .await
            .unwrap();
        for _ in 0..6 {
            let actual: Vec<Json> = multiplexed
                .fetch(&mut conn, &arguments)
                .await
                .unwrap()
                .into_iter()
                .map(|value| sorted(value, unordered))
                .collect();
            assert_eq!(actual, expected, "{name} {mode}");
        }
    }
    sqlx::raw_sql("RESET plan_cache_mode")
        .execute(&mut *conn)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn multiplexed_statements_return_the_results_of_each_statement() {
    let pool = database("http_subscription_multiplexed", &["schema.sql", "seed.sql"]).await;
    let schema = common::schema();
    let admin = rusuna::Session::new("admin");
    for case in common::cases() {
        let compiled = schema.compile_selection(&case.query, &admin).unwrap();
        multiplexed_matches(
            &pool,
            &[compiled.clone(), compiled],
            case.unordered,
            case.name,
        )
        .await;
    }
    let schema = rusuna::Schema::from_sdl(&fixture("permissions.graphql")).unwrap();
    let cases: Json = serde_json::from_str(&fixture("permissions/cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let role = case["role"].as_str().unwrap();
        let query = fixture(&format!("permissions/queries/{name}.graphql"));
        let sessions: Vec<Json> = if case["session"].get("user_id").is_some() {
            vec![
                json!({"user_id": 1}),
                case["session"].clone(),
                json!({"user_id": 4}),
            ]
        } else {
            vec![case["session"].clone(), case["session"].clone()]
        };
        let compiled: Vec<rusuna::Compiled> = sessions
            .iter()
            .map(|session| {
                let session = rusuna::Session::from_json(role, session).unwrap();
                schema
                    .compile(&query, &json!({}), None, &session)
                    .unwrap()
                    .remove(0)
                    .compiled
            })
            .collect();
        let unordered: Vec<&str> = case["unordered"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        multiplexed_matches(&pool, &compiled, &unordered, name).await;
    }
    let schema = rusuna::Schema::from_sdl(&fixture("limits.graphql")).unwrap();
    let cases: Json = serde_json::from_str(&fixture("limits/cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let session = rusuna::Session::new(case["role"].as_str().unwrap());
        let query = fixture(&format!("limits/queries/{name}.graphql"));
        let compiled = schema
            .compile(&query, &json!({}), None, &session)
            .unwrap()
            .remove(0)
            .compiled;
        multiplexed_matches(&pool, &[compiled.clone(), compiled], &[], name).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn primary_key_and_aggregate_roots_are_subscribed() {
    let pool = database("http_subscription_roots", &["schema.sql", "seed.sql"]).await;
    let subscriptions = Subscriptions::new(pool.clone(), options());
    let admin = serve_ws(role_schemas(
        &fixture("schema.graphql"),
        &pool,
        &subscriptions,
    ))
    .await;
    let roles = serve_ws(role_schemas(
        &fixture("permissions.graphql"),
        &pool,
        &subscriptions,
    ))
    .await;
    let mut row = Client::connect(admin, json!({"role": "admin"})).await;
    row.subscribe(
        "1",
        "subscription($id: Int!) { author_by_pk(id: $id) { id name } }",
        json!({"id": 99}),
    )
    .await;
    assert_eq!(row.data("1").await, json!({"author_by_pk": null}));
    let mut count =
        Client::connect(roles, json!({"role": "user", "session": {"user_id": 3}})).await;
    count
        .subscribe(
            "1",
            "subscription { article_aggregate { aggregate { count } } }",
            json!({}),
        )
        .await;
    assert_eq!(
        count.data("1").await,
        json!({"article_aggregate": {"aggregate": {"count": 4}}})
    );
    sqlx::raw_sql(
        "INSERT INTO author (id, name) VALUES (99, 'new author'); INSERT INTO article VALUES (99, 3, 'new article', '2020-01-02 00:00+00')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        row.data("1").await,
        json!({"author_by_pk": {"id": 99, "name": "new author"}})
    );
    assert_eq!(
        count.data("1").await,
        json!({"article_aggregate": {"aggregate": {"count": 5}}})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn views_and_materialized_views_are_subscribed() {
    let pool = database("http_subscription_views", &["views.sql"]).await;
    let sdl = fixture("views.graphql");
    let subscriptions = Subscriptions::new(pool.clone(), options());
    let address = serve_ws(role_schemas(&sdl, &pool, &subscriptions)).await;
    let mut client =
        Client::connect(address, json!({"role": "user", "session": {"user_id": 1}})).await;
    client
        .subscribe(
            "published",
            "subscription { view_published(order_by: {id: asc}) { id title } }",
            json!({}),
        )
        .await;
    assert_eq!(
        client.data("published").await,
        json!({"view_published": [{"id": 1, "title": "first"}]})
    );
    client
        .subscribe(
            "stats",
            "subscription { view_author_stats_aggregate { aggregate { count sum { total } } } }",
            json!({}),
        )
        .await;
    assert_eq!(
        client.data("stats").await,
        json!({"view_author_stats_aggregate": {"aggregate": {"count": 1, "sum": {"total": 9}}}})
    );

    sqlx::query("UPDATE view_article SET published = true, score = 10 WHERE id = 2")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client.data("published").await,
        json!({"view_published": [{"id": 1, "title": "first"}, {"id": 2, "title": "draft"}]})
    );
    sqlx::query("REFRESH MATERIALIZED VIEW view_author_stats")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client.data("stats").await,
        json!({"view_author_stats_aggregate": {"aggregate": {"count": 2, "sum": {"total": 22}}}})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn limited_subscriptions_send_at_most_the_permitted_rows() {
    let pool = database("http_subscription_limits", &["schema.sql", "seed.sql"]).await;
    let subscriptions = Subscriptions::new(pool.clone(), options());
    let address = serve_ws(role_schemas(
        &fixture("limits.graphql"),
        &pool,
        &subscriptions,
    ))
    .await;
    let mut client = Client::connect(address, json!({"role": "limited", "session": {}})).await;
    client
        .subscribe(
            "list",
            "subscription { author(order_by: {id: desc}) { id } }",
            json!({}),
        )
        .await;
    client
        .subscribe(
            "aggregate",
            "subscription { author_aggregate(order_by: {id: desc}) { aggregate { count } nodes { id } } }",
            json!({}),
        )
        .await;
    let initial = client.data_batch(&["list", "aggregate"]).await;
    assert_eq!(initial["list"], json!({"author": [{"id": 6}, {"id": 5}]}));
    assert_eq!(
        initial["aggregate"],
        json!({"author_aggregate": {"aggregate": {"count": 6}, "nodes": [{"id": 6}, {"id": 5}]}})
    );
    sqlx::query("INSERT INTO author (id, name) VALUES (99, 'new author')")
        .execute(&pool)
        .await
        .unwrap();
    let updated = client.data_batch(&["list", "aggregate"]).await;
    assert_eq!(updated["list"], json!({"author": [{"id": 99}, {"id": 6}]}));
    assert_eq!(
        updated["aggregate"],
        json!({"author_aggregate": {"aggregate": {"count": 7}, "nodes": [{"id": 99}, {"id": 6}]}})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn statement_roots_are_subscribed_with_their_arguments() {
    let pool = database("http_subscription_natives", &["schema.sql", "seed.sql"]).await;
    let schema = rusuna::Schema::from_sdl(&fixture("natives.graphql")).unwrap();
    let compiled: Vec<rusuna::Compiled> = [(1, "title%"), (4, "title 1%"), (2, "nothing")]
        .iter()
        .map(|(min, pattern)| {
            schema
                .compile(
                    &format!(
                        "{{ article_search(min_id: {min}, pattern: \"{pattern}\", order_by: {{id: asc}}) {{ id title }} }}"
                    ),
                    &json!({}),
                    None,
                    &rusuna::Session::new("admin"),
                )
                .unwrap()
                .remove(0)
                .compiled
        })
        .collect();
    assert!(compiled[0].sql.contains("'$1'"));
    multiplexed_matches(&pool, &compiled, &[], "article_search").await;
    let subscriptions = Subscriptions::new(pool.clone(), options());
    let address = serve_ws(role_schemas(
        &fixture("natives.graphql"),
        &pool,
        &subscriptions,
    ))
    .await;
    let mut client =
        Client::connect(address, json!({"role": "user", "session": {"user_id": 5}})).await;
    client
        .subscribe(
            "1",
            "subscription($min: Int!) { article_search(min_id: $min, pattern: \"title%\", order_by: {id: asc}) { id } }",
            json!({"min": 10}),
        )
        .await;
    assert_eq!(
        client.data("1").await,
        json!({"article_search": [{"id": 10}, {"id": 11}]})
    );
    sqlx::raw_sql("INSERT INTO article VALUES (99, 5, 'title 99', '2020-01-02 00:00+00')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client.data("1").await,
        json!({"article_search": [{"id": 10}, {"id": 11}, {"id": 99}]})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn function_roots_are_subscribed_with_their_arguments() {
    let pool = database(
        "http_subscription_functions",
        &["schema.sql", "seed.sql", "functions.sql"],
    )
    .await;
    let schema = rusuna::Schema::from_sdl(&fixture("functions.graphql")).unwrap();
    let compiled: Vec<rusuna::Compiled> = [
        "{ search_articles(search: \"title%\", since_id: 3, order_by: {id: asc}) { id title } }",
        "{ search_articles(search: \"title 1%\", order_by: {id: asc}) { id title } }",
        "{ search_articles(search: \"none\", since_id: 1, order_by: {id: asc}) { id title } }",
    ]
    .iter()
    .map(|query| {
        schema
            .compile(
                query,
                &json!({}),
                None,
                &rusuna::Session::new("user").variable("user_id", rusuna::Value::Int(3)),
            )
            .unwrap()
            .remove(0)
            .compiled
    })
    .collect();
    multiplexed_matches(&pool, &compiled, &[], "search_articles").await;
    let subscriptions = Subscriptions::new(pool.clone(), options());
    let address = serve_ws(role_schemas(
        &fixture("functions.graphql"),
        &pool,
        &subscriptions,
    ))
    .await;
    let mut client = Client::connect(address, json!({"role": "admin"})).await;
    client
        .subscribe(
            "1",
            "subscription($min: Int!) { author_totals(min_articles: $min, order_by: {author_id: asc}) { author_id articles } }",
            json!({"min": 3}),
        )
        .await;
    assert_eq!(
        client.data("1").await,
        json!({"author_totals": [{"author_id": 1, "articles": 3}, {"author_id": 3, "articles": 4}]})
    );
    sqlx::raw_sql("INSERT INTO article VALUES (99, 5, 'title 99', '2020-01-02 00:00+00')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client.data("1").await,
        json!({"author_totals": [{"author_id": 1, "articles": 3}, {"author_id": 3, "articles": 4}, {"author_id": 5, "articles": 3}]})
    );
}
