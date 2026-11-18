mod support;

use rusuna_axum::{Endpoint, RestMethod};
use serde_json::{Value as Json, json};
use sqlx::Row;
use std::collections::HashMap;
use std::net::SocketAddr;
use support::*;
use tokio::net::TcpListener;

const ITEM: &str =
    "query Item($id: Int!) { mutation_item_by_pk(id: $id) { id title score owner { name } } }";
const ITEMS: &str = "query Items($min: Int! = 0, $owner: Boolean! = false, $title: String) { mutation_item(where: {score: {_gte: $min}}, order_by: {id: asc}) { id title owner @include(if: $owner) { name } } search: mutation_item(where: {title: {_like: $title}}) @include(if: $owner) { id } }";
const CREATE: &str = "mutation Create($title: String!, $score: Int!) { insert_mutation_item_one(object: {title: $title, score: $score}) { id title score owner_id } }";
const RENAME: &str = "mutation Rename($id: Int!, $title: String!) { update_mutation_item_by_pk(pk_columns: {id: $id}, _set: {title: $title}) { id title note } }";
const SECRETS: &str = "{ mutation_item(order_by: {id: asc}) { id secret } }";

fn endpoints() -> Vec<Endpoint> {
    vec![
        Endpoint::new("item", "/api/rest/items/:id", [RestMethod::Get], ITEM),
        Endpoint::new("items", "/api/rest/items", [RestMethod::Get], ITEMS),
        Endpoint::new("create", "/api/rest/items", [RestMethod::Post], CREATE),
        Endpoint::new(
            "rename",
            "/api/rest/items/:id",
            [RestMethod::Patch, RestMethod::Put],
            RENAME,
        ),
        Endpoint::new("secrets", "/api/rest/secrets", [RestMethod::Get], SECRETS),
    ]
}

async fn build(
    schemas: HashMap<String, async_graphql::dynamic::Schema>,
    endpoints: Vec<Endpoint>,
) -> Result<axum::Router, rusuna_axum::EndpointError> {
    rusuna_axum::rest(schemas, endpoints, |parts: &axum::http::request::Parts| {
        let session = session(parts);
        async move { Ok(session) }
    })
    .await
}

async fn serve_rest(pool: &sqlx::PgPool) -> SocketAddr {
    let schemas = schemas(&fixture("mutations/schema.graphql"), pool, None);
    let app = build(schemas.clone(), endpoints()).await.unwrap().route(
        "/graphql",
        rusuna_axum::graphql(schemas, |parts: &axum::http::request::Parts| {
            let session = session(parts);
            async move { Ok(session) }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

async fn reseed(pool: &sqlx::PgPool) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(fixture("mutations/seed.sql")))
        .execute(pool)
        .await
        .unwrap();
}

async fn graphql(
    address: SocketAddr,
    headers: &[(&str, String)],
    query: &str,
    variables: Json,
) -> Json {
    let response = post(
        address,
        headers,
        &json!({"query": query, "variables": variables}),
    )
    .await;
    assert!(response.get("errors").is_none(), "{response}");
    response["data"].clone()
}

fn user(id: i64) -> Vec<(&'static str, String)> {
    role_headers("user", &json!({ "user_id": id }))
}

async fn items(pool: &sqlx::PgPool) -> Json {
    sqlx::query("SELECT json_agg(json_build_array(id, owner_id, title, score) ORDER BY id) FROM mutation_item")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0)
}

fn code(body: &Json) -> &str {
    body["errors"][0]["extensions"]["code"]
        .as_str()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_variables_come_from_the_path_the_query_string_and_the_body() {
    let pool = database("http_rest_variables", &["mutations/schema.sql"]).await;
    reseed(&pool).await;
    let address = serve_rest(&pool).await;

    let (status, body) = request(address, "GET", "/api/rest/items/1", &user(1), None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        graphql(address, &user(1), ITEM, json!({"id": 1})).await
    );
    assert_eq!(
        body,
        json!({"mutation_item_by_pk": {"id": 1, "title": "first", "score": 1, "owner": {"name": "Ada"}}})
    );

    let (status, body) = request(
        address,
        "GET",
        "/api/rest/items?min=2&owner&title=s%25",
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        graphql(
            address,
            &[],
            ITEMS,
            json!({"min": 2, "owner": true, "title": "s%"})
        )
        .await
    );
    assert_eq!(body["mutation_item"].as_array().unwrap().len(), 2);
    assert_eq!(body["search"], json!([{"id": 2}]));
    let (status, body) = request(
        address,
        "GET",
        "/api/rest/items?owner=false&min=",
        &[],
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = request(address, "GET", "/api/rest/items?owner=false", &[], None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        graphql(address, &[], ITEMS, json!({"owner": false})).await
    );

    let created = request(
        address,
        "POST",
        "/api/rest/items",
        &user(2),
        Some(r#"{"title": "posted", "score": 4}"#),
    )
    .await;
    assert_eq!(created.0, 200, "{}", created.1);
    let after_rest = items(&pool).await;
    reseed(&pool).await;
    let direct = graphql(
        address,
        &user(2),
        CREATE,
        json!({"title": "posted", "score": 4}),
    )
    .await;
    assert_eq!(created.1, direct);
    assert_eq!(after_rest, items(&pool).await);
    assert_eq!(
        direct["insert_mutation_item_one"]["owner_id"],
        json!(2),
        "{direct}"
    );

    reseed(&pool).await;
    let renamed = request(
        address,
        "PATCH",
        "/api/rest/items/1?title=renamed",
        &user(1),
        None,
    )
    .await;
    assert_eq!(renamed.0, 200, "{}", renamed.1);
    assert_eq!(
        renamed.1,
        json!({"update_mutation_item_by_pk": {"id": 1, "title": "renamed", "note": "updated"}})
    );
    let path_wins = request(
        address,
        "PUT",
        "/api/rest/items/2",
        &user(2),
        Some(r#"{"id": 1, "title": "from the body"}"#),
    )
    .await;
    assert_eq!(
        path_wins.1,
        json!({"update_mutation_item_by_pk": {"id": 2, "title": "from the body", "note": "updated"}})
    );
    let foreign = request(
        address,
        "PATCH",
        "/api/rest/items/2?title=stolen",
        &user(1),
        None,
    )
    .await;
    assert_eq!(foreign, (200, json!({"update_mutation_item_by_pk": null})));
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_errors_have_statuses_and_codes() {
    let pool = database("http_rest_errors", &["mutations/schema.sql"]).await;
    reseed(&pool).await;
    let address = serve_rest(&pool).await;
    let before = items(&pool).await;

    for (method, path, headers, body) in [
        ("GET", "/api/rest/items/abc", user(1), None),
        ("GET", "/api/rest/items/1.5", user(1), None),
        ("GET", "/api/rest/items?owner=maybe", vec![], None),
        ("GET", "/api/rest/items?unknown=1", vec![], None),
        (
            "POST",
            "/api/rest/items",
            user(1),
            Some(r#"{"title": "t", "score": 1, "extra": 1}"#),
        ),
        ("POST", "/api/rest/items", user(1), Some("[1]")),
        ("POST", "/api/rest/items", user(1), Some("{")),
        (
            "POST",
            "/api/rest/items",
            user(1),
            Some(r#"{"title": "t", "score": "x"}"#),
        ),
        (
            "POST",
            "/api/rest/items",
            user(1),
            Some(r#"{"title": "t"}"#),
        ),
    ] {
        let (status, response) = request(address, method, path, &headers, body).await;
        assert_eq!(status, 400, "{path} {body:?} {response}");
        assert!(response["errors"][0]["message"].is_string(), "{response}");
    }
    let (status, response) = request(address, "GET", "/api/rest/items/abc", &user(1), None).await;
    assert_eq!(
        (status, code(&response)),
        (400, "bad-request"),
        "{response}"
    );

    let (status, response) = request(
        address,
        "POST",
        "/api/rest/items",
        &user(1),
        Some(r#"{"title": "negative", "score": -1}"#),
    )
    .await;
    assert_eq!(status, 400, "{response}");
    assert_eq!(code(&response), "permission-error");
    assert_eq!(
        response["errors"][0]["message"],
        "check constraint of an insert/update permission has failed"
    );
    assert_eq!(items(&pool).await, before);

    let (status, response) = request(
        address,
        "GET",
        "/api/rest/items/1",
        &role_headers("visitor", &json!({})),
        None,
    )
    .await;
    assert_eq!(
        (status, code(&response)),
        (403, "access-denied"),
        "{response}"
    );
    let (status, response) = request(address, "GET", "/api/rest/secrets", &user(1), None).await;
    assert_eq!(
        (status, code(&response)),
        (403, "access-denied"),
        "{response}"
    );
    let (status, response) = request(address, "GET", "/api/rest/secrets", &[], None).await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["mutation_item"][0]["secret"], "private");
    let (status, response) = request(
        address,
        "POST",
        "/api/rest/items",
        &role_headers("static", &json!({})),
        Some(r#"{"title": "t", "score": 1}"#),
    )
    .await;
    assert_eq!(
        (status, code(&response)),
        (403, "access-denied"),
        "{response}"
    );

    let (status, _) = request(address, "DELETE", "/api/rest/items/1", &user(1), None).await;
    assert_eq!(status, 405);
    let (status, _) = request(address, "GET", "/api/rest/nothing", &user(1), None).await;
    assert_eq!(status, 404);
    assert_eq!(items(&pool).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_endpoints_are_validated_when_they_are_built() {
    let pool = database("http_rest_validation", &["mutations/schema.sql"]).await;
    reseed(&pool).await;
    let schemas = schemas(&fixture("mutations/schema.graphql"), &pool, None);
    let before = items(&pool).await;
    let delete = "mutation Delete { delete_mutation_item(where: {}) { affected_rows } }";
    assert!(
        build(
            schemas.clone(),
            vec![Endpoint::new(
                "delete",
                "/delete",
                [RestMethod::Delete],
                delete,
            )],
        )
        .await
        .is_ok()
    );
    assert!(build(schemas.clone(), endpoints()).await.is_ok());
    assert_eq!(items(&pool).await, before);

    let get =
        |name: &str, url: &str, query: &str| Endpoint::new(name, url, [RestMethod::Get], query);
    for (endpoints, message) in [
        (
            vec![get(
                "watch",
                "/watch",
                "subscription { mutation_item { id } }",
            )],
            "subscriptions",
        ),
        (
            vec![get("unknown", "/unknown", "{ nothing { id } }")],
            "no role",
        ),
        (
            vec![get("unbound", "/items/:id", "{ mutation_item { id } }")],
            "URL variable id",
        ),
        (
            vec![get(
                "two",
                "/two",
                "query A { mutation_item { id } } query B { mutation_item { id } }",
            )],
            "exactly one operation",
        ),
        (vec![get("syntax", "/syntax", "{")], ""),
        (vec![get("pattern", "items/:id", ITEM)], "invalid URL"),
        (vec![get("braces", "/items/{id}", ITEM)], "invalid URL"),
        (
            vec![get("a", "/items/:id", ITEM), get("b", "/items/:id", ITEM)],
            "another endpoint",
        ),
        (
            vec![
                get("a", "/items/:id", ITEM),
                Endpoint::new(
                    "b",
                    "/items/:key",
                    [RestMethod::Post],
                    ITEM.replace("$id", "$key"),
                ),
            ],
            "other variable names",
        ),
        (vec![get("a", "/a", ITEM), get("a", "/b", ITEM)], "distinct"),
        (vec![Endpoint::new("none", "/none", [], ITEM)], "methods"),
    ] {
        let error = build(schemas.clone(), endpoints).await.unwrap_err();
        assert!(error.message.contains(message), "{error}");
    }
}

#[test]
fn endpoints_are_read_from_json() {
    let endpoints: Vec<Endpoint> = serde_json::from_str(
        r#"[{"Name": "item", "Url": "/api/rest/items/:id", "Methods": ["GET"], "Query": "query Item($id: Int!) { mutation_item_by_pk(id: $id) { id } }"}]"#,
    )
    .unwrap();
    assert_eq!(
        endpoints,
        vec![Endpoint::new(
            "item",
            "/api/rest/items/:id",
            [RestMethod::Get],
            "query Item($id: Int!) { mutation_item_by_pk(id: $id) { id } }"
        )]
    );
    assert!(
        serde_json::from_str::<Endpoint>(
            r#"{"Name": "x", "Url": "/x", "Methods": ["HEAD"], "Query": "{ a }"}"#
        )
        .is_err()
    );
}
