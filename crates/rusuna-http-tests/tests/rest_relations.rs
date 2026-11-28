mod support;

use axum::extract::Path;
use axum::{Json as AxumJson, Router};
use rusuna_async_graphql::{Builder, RestSource};
use serde_json::{Value as Json, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use support::{database, post, serve};
use tokio::net::TcpListener;

const SDL: &str = r#"
    type Query {
      orders(page: String!): [Order!]! @rest(source: "api", path: "/orders/{{page}}", roles: ["user", "guest"])
      bundles: [Bundle!]! @rest(source: "api", path: "/bundles", roles: ["user"])
      entries: [Entry!]! @list
    }
    type Bundle { shipments: [Order!]! }
    type Order {
      key: String @column(name: "order_key")
      amount: Float
      entries: [Entry!]! @relation(columns: ["key"], references: ["key"])
      best: Entry! @relation(columns: ["key"], references: ["key"])
      matching: [Entry!]! @relation(columns: ["key", "amount"], references: ["key", "amount"])
      private: PrivateEntry @relation(columns: ["key"], references: ["key"])
    }
    type Entry @table(name: "rest_entries") @permission(role: "user", columns: ["id", "key", "position"], where: {owner: {_eq: "$user_id"}}, limit: 2) {
      id: Int!
      key: String! @column(name: "order_key")
      position: Int!
      amount: Float!
      owner: Int!
    }
    type PrivateEntry @table(name: "rest_entries") {
      key: String! @column(name: "order_key")
    }
"#;

fn key(id: u32) -> String {
    format!("00000000-0000-0000-0000-{id:012}")
}

async fn upstream() -> String {
    let app = Router::new()
        .route(
            "/orders/{page}",
            axum::routing::get(async |Path(page): Path<String>| {
                let orders = match page.as_str() {
                    "100" | "101" => {
                        let count = page.parse().unwrap();
                        let mut orders = (1..=count)
                            .map(|id| json!({"order_key": key(id)}))
                            .collect::<Vec<_>>();
                        orders.push(json!({"order_key": key(1)}));
                        orders.push(json!({"order_key": null}));
                        orders
                    }
                    "invalid" => vec![
                        json!({"order_key": 1}),
                        json!({"order_key": "invalid uuid"}),
                        json!({"order_key": key(1)}),
                    ],
                    "null" => vec![json!({"order_key": null}), json!({})],
                    _ => vec![
                        json!({"order_key": key(1), "amount": 1.5}),
                        json!({"order_key": key(2), "amount": 2.5}),
                        json!({"order_key": key(1), "amount": 1.5}),
                        json!({"order_key": key(2), "amount": null}),
                        json!({"order_key": null, "amount": 2.5}),
                    ],
                };
                AxumJson(orders)
            }),
        )
        .route(
            "/bundles",
            axum::routing::get(async || {
                AxumJson(json!([
                    {"shipments": [{"order_key": key(1)}]},
                    {"shipments": [{"order_key": key(2)}, {"order_key": key(1)}]},
                ]))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

async fn setup(database_name: &str) -> (Arc<rusuna::Schema>, PgPool, SocketAddr) {
    let pool = database(database_name, &[]).await;
    sqlx::raw_sql(
        "CREATE EXTENSION pg_stat_statements;
         CREATE TABLE rest_entries (id int NOT NULL, order_key uuid NOT NULL, position int NOT NULL, amount numeric NOT NULL, owner int NOT NULL);
         INSERT INTO rest_entries
         SELECT 10 * group_id + position,
             ('00000000-0000-0000-0000-' || lpad(group_id::text, 12, '0'))::uuid,
             position, group_id + 0.5, CASE WHEN position = 4 THEN 2 ELSE 1 END
         FROM generate_series(1, 101) group_id CROSS JOIN generate_series(1, 5) position"
    ).execute(&pool).await.unwrap();
    let schema = Arc::new(rusuna::Schema::from_sdl(SDL).unwrap());
    let source = RestSource::new("api", &upstream().await).unwrap();
    let schemas = ["user", "guest"]
        .into_iter()
        .map(|role| {
            let schema = Builder::new(schema.clone(), role)
                .rest(source.clone())
                .build()
                .unwrap()
                .schema()
                .data(pool.clone())
                .finish()
                .unwrap();
            (role.into(), schema)
        })
        .collect::<HashMap<_, _>>();
    (schema, pool, serve(schemas).await)
}

async fn counts(pool: &PgPool) -> (i64, i64) {
    sqlx::query_as(
        "SELECT coalesce(sum(calls) FILTER (WHERE query LIKE '%__rusuna_subscription%'), 0)::int8,
             coalesce(sum(calls) FILTER (WHERE query NOT LIKE '%__rusuna_subscription%'), 0)::int8
         FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
             AND query NOT LIKE '%pg_stat_statements%'",
    ).fetch_one(pool).await.unwrap()
}

async fn query(address: SocketAddr, role: &str, query: &str, variables: Json) -> Json {
    post(
        address,
        &[
            ("x-hasura-role", role.into()),
            ("x-hasura-user-id", "1".into()),
        ],
        &json!({"query": query, "variables": variables}),
    )
    .await
}

#[tokio::test]
async fn matches_single_queries_with_one_statement_and_applies_permissions() {
    let (schema, pool, address) = setup("rest_relation_results").await;
    let before = counts(&pool).await;
    let response = query(address, "user", r#"
        query($min: Int!) {
          orders(page: "small") {
            entries(where: {position: {_gte: $min}}, order_by: [{position: desc}], limit: 10, offset: 1) { id }
          }
        }
        "#, json!({"min": 2})).await;
    let after = counts(&pool).await;
    assert_eq!(after, (before.0 + 1, before.1));
    assert!(response.get("errors").is_none(), "{response}");
    let session = rusuna::Session::from_json("user", &json!({"user_id": 1})).unwrap();
    let mut expected = Vec::new();
    for key in [key(1), key(2), key(1), key(2)] {
        let fields = schema.compile(
            "query($key: String!) { entries(where: { key: {_eq: $key}, position: {_gte: 2}}, order_by: [{position: desc}], limit: 10, offset: 1) { id } }",
            &json!({"key": key}),
            None,
            &session,
        ).unwrap();
        expected.push(Json::Object(
            rusuna_sqlx::execute(&pool, &fields).await.unwrap(),
        ));
    }
    expected.push(json!({"entries": []}));
    assert_eq!(response["data"]["orders"], json!(expected));
    assert_eq!(
        response["data"]["orders"][0],
        json!({"entries": [{"id": 13}, {"id": 12}]})
    );
    assert_eq!(response["data"]["orders"][4], json!({"entries": []}));

    let sdl = schema.expanded_sdl("user").unwrap();
    assert!(sdl.contains("entries(where: Entry_bool_exp"));
    assert!(!sdl.contains("private("));
    assert!(!schema.expanded_sdl("guest").unwrap().contains("entries("));
    for query_text in [
        "{ orders(page: \"small\") { private { key } } }",
        "{ orders(page: \"small\") { entries { amount } } }",
        "{ orders(page: \"small\") { entries(where: {amount: {_eq: 1.5}}) { id } } }",
    ] {
        let rejected = query(address, "user", query_text, json!({})).await;
        assert!(rejected["errors"].is_array(), "{rejected}");
    }
    let rejected = query(
        address,
        "guest",
        "{ orders(page: \"small\") { entries { id } } }",
        json!({}),
    )
    .await;
    assert!(rejected["errors"].is_array(), "{rejected}");
}

#[tokio::test]
async fn batches_distinct_keys_in_groups_of_one_hundred() {
    let (_, pool, address) = setup("rest_relation_batch_size").await;
    let mut before = counts(&pool).await;
    for (page, statements) in [("100", 1), ("101", 2), ("null", 0)] {
        let response = query(address, "user",
            "query($page: String!) { orders(page: $page) { entries(order_by: [{position: desc}]) { id } } }",
            json!({"page": page}),
        ).await;
        let after = counts(&pool).await;
        assert_eq!(after, (before.0 + statements, before.1));
        before = after;
        assert!(response.get("errors").is_none(), "{response}");
        let orders = response["data"]["orders"].as_array().unwrap();
        if let Ok(count) = page.parse::<u32>() {
            for (id, order) in (1..=count).zip(orders) {
                assert_eq!(
                    order["entries"],
                    json!([{"id": 10 * id + 5}, {"id": 10 * id + 3}])
                );
            }
            assert_eq!(orders[count as usize], orders[0]);
        } else {
            assert_eq!(orders, &[json!({"entries": []}), json!({"entries": []})]);
        }
        assert_eq!(orders.last().unwrap()["entries"], json!([]));
    }
}

#[tokio::test]
async fn groups_aliases_and_nested_lists_and_handles_composite_and_invalid_keys() {
    let (_, pool, address) = setup("rest_relation_aliases").await;
    let before = counts(&pool).await;
    let response = query(
        address,
        "user",
        r#"
        {
          orders(page: "small") {
            high: entries(order_by: [{position: desc}]) { id }
            ...WithPosition
            low: entries(order_by: [{position: asc}]) { id }
            best(order_by: [{position: desc}]) { id }
            matching(order_by: [{position: desc}]) { id }
            skipped: best @skip(if: true) { id }
          }
          bundles { shipments { entries(limit: 1, order_by: [{position: desc}]) { id } } }
        }
        fragment WithPosition on Order {
          high: entries(order_by: [{position: desc}]) { position }
        }
        "#,
        json!({}),
    )
    .await;
    assert!(response.get("errors").is_none(), "{response}");
    assert_eq!(counts(&pool).await, (before.0 + 5, before.1));
    assert_eq!(
        response["data"]["orders"][0],
        json!({
            "high": [{"id": 15, "position": 5}, {"id": 13, "position": 3}],
            "low": [{"id": 11}, {"id": 12}],
            "best": {"id": 15},
            "matching": [{"id": 15}, {"id": 13}],
        })
    );
    assert_eq!(response["data"]["orders"][3]["matching"], json!([]));
    assert_eq!(
        response["data"]["orders"][4],
        json!({"high": [], "low": [], "best": null, "matching": []})
    );
    assert_eq!(
        response["data"]["bundles"],
        json!([
            {"shipments": [{"entries": [{"id": 15}]}]},
            {"shipments": [{"entries": [{"id": 25}]}, {"entries": [{"id": 15}]}]},
        ])
    );

    let before = counts(&pool).await;
    let response = query(
        address,
        "user",
        "{ orders(page: \"invalid\") { best(order_by: [{position: desc}]) { id } } }",
        json!({}),
    )
    .await;
    assert_eq!(counts(&pool).await, (before.0 + 1, before.1));
    assert_eq!(
        response["data"]["orders"],
        json!([{"best": null}, {"best": null}, {"best": {"id": 15}}])
    );
    let errors = response["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2, "{response}");
    for index in 0..2 {
        assert!(
            errors.iter().any(|error| {
                error["path"] == json!(["orders", index, "best"])
                    && error["extensions"]["code"] == "validation-failed"
            }),
            "{response}"
        );
    }
}

#[test]
fn rejects_invalid_rest_relationship_mappings() {
    for invalid in [
        "columns: [], references: []",
        "columns: [\"absent\"], references: [\"key\"]",
        "columns: [\"entries\"], references: [\"key\"]",
        "columns: [\"key\"], references: [\"absent\"]",
        "columns: [\"key\", \"key\"], references: [\"key\", \"id\"]",
    ] {
        let invalid = SDL.replacen("columns: [\"key\"], references: [\"key\"]", invalid, 1);
        assert!(rusuna::Schema::from_sdl(&invalid).is_err(), "{invalid}");
    }
    let invalid = SDL.replacen(
        "type PrivateEntry @table(name: \"rest_entries\")",
        "type PrivateEntry",
        1,
    );
    assert!(rusuna::Schema::from_sdl(&invalid).is_err());
}
