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
async fn function_roots_take_variables_over_http() {
    let pool = database(
        "http_functions",
        &["schema.sql", "seed.sql", "functions.sql"],
    )
    .await;
    let address = serve(schemas(&fixture("functions.graphql"), &pool, None)).await;
    let query = "query Search($search: String!, $since: Int) { search_articles(search: $search, since_id: $since, order_by: {id: asc}) { id title author { name } } author_totals(min_articles: 1, order_by: {author_id: asc}) { author_id articles } }";
    let actual = post(
        address,
        &role_headers("user", &json!({"user_id": 3})),
        &json!({"query": query, "variables": {"search": "title%", "since": 4}}),
    )
    .await;
    let expected = sql(
        &pool,
        "SELECT json_build_object('data', json_build_object('search_articles', (SELECT json_agg(json_build_object('id', s.id, 'title', s.title, 'author', json_build_object('name', a.name)) ORDER BY s.id) FROM (SELECT * FROM search_articles('title%', 4) WHERE author_id = 3 ORDER BY id LIMIT 3) s JOIN author a ON a.id = s.author_id), 'author_totals', (SELECT json_agg(json_build_object('author_id', author_id, 'articles', articles) ORDER BY author_id) FROM \"author totals\"(1) WHERE author_id <> 1)))",
    )
    .await;
    assert_eq!(actual, expected);
    let hidden = post(
        address,
        &role_headers("visitor", &json!({})),
        &json!({"query": "{ search_articles(search: \"x\") { id } }"}),
    )
    .await;
    assert!(hidden["errors"].is_array(), "{hidden}");
}

#[tokio::test(flavor = "multi_thread")]
async fn function_roots_are_rest_endpoints() {
    let pool = database(
        "http_functions_rest",
        &["schema.sql", "seed.sql", "functions.sql"],
    )
    .await;
    let app = rusuna_axum::rest(
        schemas(&fixture("functions.graphql"), &pool, None),
        vec![Endpoint::new(
            "totals",
            "/api/rest/totals/:min",
            [RestMethod::Get],
            "query Totals($min: Int!) { author_totals(min_articles: $min, order_by: {author_id: asc}) { author_id articles first_title } }",
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
    let (status, body) = request(address, "GET", "/api/rest/totals/3", &[], None).await;
    assert_eq!(status, 200, "{body}");
    let expected = sql(
        &pool,
        "SELECT json_build_object('author_totals', json_agg(json_build_object('author_id', author_id, 'articles', articles, 'first_title', first_title) ORDER BY author_id)) FROM \"author totals\"(3)",
    )
    .await;
    assert_eq!(body, expected);
}
