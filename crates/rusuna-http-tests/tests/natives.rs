mod support;

use rusuna_axum::{Endpoint, RestMethod};
use serde_json::{Value as Json, json};
use support::*;
use tokio::net::TcpListener;

async fn sql(pool: &sqlx::PgPool, statement: &str) -> Json {
    sqlx::query_scalar::<_, Json>(sqlx::AssertSqlSafe(statement.to_string()))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn statement_roots_take_variables_over_http() {
    let pool = database("http_natives", &["schema.sql", "seed.sql"]).await;
    let address = serve(schemas(&fixture("natives.graphql"), &pool, None)).await;
    let query = "query Search($min: Int!, $pattern: String) { article_search(min_id: $min, pattern: $pattern, order_by: {id: asc}) { id title author { name } comments_aggregate { aggregate { count } } } }";
    let actual = post(
        address,
        &role_headers("user", &json!({"user_id": 3})),
        &json!({"query": query, "variables": {"min": 4, "pattern": "title%"}}),
    )
    .await;
    let expected = sql(
        &pool,
        "SELECT json_build_object('data', json_build_object('article_search', json_agg(json_build_object('id', a.id, 'title', a.title, 'author', json_build_object('name', au.name), 'comments_aggregate', json_build_object('aggregate', json_build_object('count', (SELECT count(*) FROM comment c WHERE c.article_id = a.id)))) ORDER BY a.id))) FROM article a JOIN author au ON au.id = a.author_id WHERE a.author_id = 3 AND a.id >= 4",
    )
    .await;
    assert_eq!(actual, expected);
    let missing = post(
        address,
        &role_headers("user", &json!({"user_id": 3})),
        &json!({"query": "{ article_search { id } }"}),
    )
    .await;
    assert!(missing["errors"].is_array(), "{missing}");
    let hidden = post(
        address,
        &role_headers("visitor", &json!({})),
        &json!({"query": "{ article_search(min_id: 1) { id } }"}),
    )
    .await;
    assert!(hidden["errors"].is_array(), "{hidden}");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_scalar_argument_and_null_reach_the_statement() {
    let pool = database("http_natives_scalars", &["schema.sql", "seed.sql"]).await;
    let address = serve(schemas(&fixture("natives.graphql"), &pool, None)).await;
    let query = "query T($i: Int!, $f: Float!, $s: String!, $b: Boolean!, $d: DateTime!, $n: Int) { typed(i: $i, f: $f, s: $s, b: $b, d: $d, n: $n) { i f s b d n } }";
    let expected = sql(
        &pool,
        "SELECT json_build_object('data', json_build_object('typed', json_build_array(json_build_object('i', -3, 'f', 2.5::float8, 's', 'it''s $1', 'b', true, 'd', '2020-01-01T00:01:00+00:00'::timestamptz, 'n', null))))",
    )
    .await;
    for n in [json!(null), json!("omitted")] {
        let mut variables = json!({"i": -3, "f": 2.5, "s": "it's $1", "b": true, "d": "2020-01-01T00:01:00+00:00", "n": n});
        if n == json!("omitted") {
            variables.as_object_mut().unwrap().remove("n");
        }
        let actual = post(
            address,
            &[],
            &json!({"query": query, "variables": variables}),
        )
        .await;
        assert_eq!(actual, expected, "{variables}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn statement_roots_are_rest_endpoints() {
    let pool = database("http_natives_rest", &["schema.sql", "seed.sql"]).await;
    let schemas = schemas(&fixture("natives.graphql"), &pool, None);
    let app = rusuna_axum::rest(
        schemas,
        vec![Endpoint::new(
            "search",
            "/api/rest/search/:min",
            [RestMethod::Get],
            "query Search($min: Int!, $pattern: String) { article_search(min_id: $min, pattern: $pattern, order_by: {id: asc}) { id title } }",
        )],
        |parts: &axum::http::request::Parts| {
            let session = session(parts);
            async move { Ok(session) }
        },
    )
    .await
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (status, body) = request(
        address,
        "GET",
        "/api/rest/search/10?pattern=title%251%25",
        &role_headers("user", &json!({"user_id": 5})),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let expected = sql(
        &pool,
        "SELECT json_build_object('article_search', json_agg(json_build_object('id', id, 'title', title) ORDER BY id)) FROM article WHERE author_id = 5 AND id >= 10 AND title LIKE 'title%1%'",
    )
    .await;
    assert_eq!(body, expected);
}
