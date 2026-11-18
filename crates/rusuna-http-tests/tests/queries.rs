#[path = "../../../tests/common/mod.rs"]
mod common;
mod support;

use serde_json::{Value as Json, json};
use sqlx::Row;
use support::*;

async fn hasura(pool: &sqlx::PgPool, user: Option<Json>, sql: &str) -> Json {
    let mut conn = pool.acquire().await.unwrap();
    if let Some(user) = user {
        sqlx::query("SELECT set_config('hasura.user', $1, false)")
            .bind(user.to_string())
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&mut *conn)
        .await
        .unwrap()
        .get::<Option<Json>, _>(0)
        .unwrap_or(Json::Null)
}

fn response_key(schema: &rusuna::Schema, query: &str, session: &rusuna::Session) -> String {
    schema
        .compile(query, &json!({}), None, session)
        .unwrap()
        .remove(0)
        .response_key
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_queries_over_http_match_hasura() {
    let pool = database("http_admin", &["schema.sql", "seed.sql"]).await;
    let sdl = fixture("schema.graphql");
    let address = serve(schemas(&sdl, &pool, None)).await;
    let schema = common::schema();
    let session = rusuna::Session::new("admin");
    let cases = common::cases();
    assert_eq!(cases.len(), 43);
    for case in cases {
        let query = fixture(&format!("queries/{}.graphql", case.name));
        let expected = hasura(&pool, None, &fixture(&format!("hasura/{}.sql", case.name))).await;
        let key = response_key(&schema, &query, &session);
        let actual = post(address, &[], &json!({ "query": query })).await;
        assert_eq!(
            sorted(actual, case.unordered),
            sorted(json!({ "data": { key: expected } }), case.unordered),
            "{}",
            case.name
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn role_queries_over_http_match_hasura() {
    let pool = database("http_roles", &["schema.sql", "seed.sql"]).await;
    let sdl = fixture("permissions.graphql");
    let address = serve(schemas(&sdl, &pool, Some("article"))).await;
    let schema = rusuna::Schema::from_sdl(&sdl).unwrap();
    let cases: Json = serde_json::from_str(&fixture("permissions/cases.json")).unwrap();
    assert_eq!(cases.as_array().unwrap().len(), 19);
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let role = case["role"].as_str().unwrap();
        let mut user = json!({ "x-hasura-role": role });
        if let Some(id) = case["session"].get("user_id") {
            user["x-hasura-user-id"] = Json::String(id.to_string());
        }
        let expected = hasura(
            &pool,
            Some(user),
            &fixture(&format!("permissions/hasura/{name}.sql")),
        )
        .await;
        let query = fixture(&format!("permissions/queries/{name}.graphql"));
        let session = rusuna::Session::from_json(role, &case["session"]).unwrap();
        let key = response_key(&schema, &query, &session);
        let unordered: Vec<&str> = case["unordered"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let actual = post(
            address,
            &role_headers(role, &case["session"]),
            &json!({ "query": query }),
        )
        .await;
        assert_eq!(
            sorted(actual, &unordered),
            sorted(json!({ "data": { key: expected } }), &unordered),
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn variables_fragments_and_aliases_pass_through_the_resolver() {
    let pool = database("http_documents", &["schema.sql", "seed.sql"]).await;
    let address = serve(schemas(&fixture("renamed-schema.graphql"), &pool, None)).await;
    let expected = hasura(&pool, None, &fixture("hasura/a.sql")).await;
    let document = "query A($limit: Int = 20, $show: Boolean! = true, $missing: Int) { list: articles(limit: $limit, offset: $missing, order_by: {publishedAt: desc}) { ...Fields comments(limit: 5, order_by: {id: asc}) @include(if: $show) { ... on Comment { body } } ignored: id @skip(if: true) } } fragment Fields on Article { id title author { name } }";
    for variables in [json!({}), json!({"limit": 20, "show": true})] {
        let actual = post(
            address,
            &[],
            &json!({ "query": document, "variables": variables }),
        )
        .await;
        assert_eq!(
            actual,
            json!({ "data": { "list": expected } }),
            "{variables}"
        );
    }
    let skipped = post(
        address,
        &[],
        &json!({ "query": document, "variables": {"show": false} }),
    )
    .await;
    let mut without = expected.clone();
    for row in without.as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("comments");
    }
    assert_eq!(skipped, json!({ "data": { "list": without } }));
    let text = "{ articles(limit: 1, order_by: {id: asc}, where: {title: {_eq: \"tab\\tquote\\\"\\u0001\"}}) { id } }";
    assert_eq!(
        post(address, &[], &json!({ "query": text })).await,
        json!({ "data": { "articles": [] } })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hand_written_fields_share_the_schema_in_both_directions() {
    let pool = database("http_hand_written", &["schema.sql", "seed.sql"]).await;
    let sdl = fixture("permissions.graphql");
    let address = serve(schemas(&sdl, &pool, Some("article"))).await;
    let session = json!({"user_id": 3});
    let actual = post(
        address,
        &role_headers("user", &session),
        &json!({ "query": "{ viewer { role mine: article(order_by: {id: asc}) { id wordCount } } article(order_by: {id: asc}, limit: 2) { title wordCount author { name } } }" }),
    )
    .await;
    let expected = hasura(
        &pool,
        None,
        "SELECT json_build_object('viewer', json_build_object('role', 'user', 'mine', (SELECT json_agg(json_build_object('id', id, 'wordCount', array_length(regexp_split_to_array(title, '\\s+'), 1)) ORDER BY id) FROM article WHERE author_id = 3)), 'article', (SELECT json_agg(x) FROM (SELECT a.title, array_length(regexp_split_to_array(a.title, '\\s+'), 1) AS \"wordCount\", json_build_object('name', au.name) AS author FROM article a JOIN author au ON au.id = a.author_id WHERE a.author_id = 3 ORDER BY a.id LIMIT 2) x))",
    )
    .await;
    assert_eq!(actual, json!({ "data": expected }));
    let hidden = post(
        address,
        &role_headers("reviewer", &json!({})),
        &json!({ "query": "{ article(limit: 1, order_by: {id: asc}) { id title wordCount } }" }),
    )
    .await;
    assert_eq!(
        hidden,
        json!({ "data": { "article": [{ "id": 1, "title": "title 1", "wordCount": 2 }] } })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn roles_without_permissions_see_neither_types_nor_root_fields() {
    let pool = database("http_hidden", &["schema.sql", "seed.sql"]).await;
    let sdl = fixture("permissions.graphql");
    let address = serve(schemas(&sdl, &pool, None)).await;
    let user = role_headers("user", &json!({"user_id": 3}));
    let types = post(
        address,
        &user,
        &json!({ "query": "{ __schema { types { name } queryType { fields { name } } } }" }),
    )
    .await;
    let names: Vec<&str> = types["data"]["__schema"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"article"));
    assert!(
        !names.iter().any(|n| n.starts_with("revision")),
        "{names:?}"
    );
    let roots: Vec<&str> = types["data"]["__schema"]["queryType"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        roots.contains(&"article") && !roots.contains(&"revision"),
        "{roots:?}"
    );
    let denied = post(address, &user, &json!({ "query": "{ revision { note } }" })).await;
    assert!(denied.get("data").is_none_or(Json::is_null), "{denied}");
    assert!(
        denied["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("revision"),
        "{denied}"
    );
    let reviewer = role_headers("reviewer", &json!({}));
    let hidden_column = post(
        address,
        &reviewer,
        &json!({ "query": "{ author { name } }" }),
    )
    .await;
    assert!(hidden_column["errors"].is_array(), "{hidden_column}");
    let unknown = post(
        address,
        &role_headers("nobody", &json!({})),
        &json!({ "query": "{ viewer { role } }" }),
    )
    .await;
    assert_eq!(unknown["errors"][0]["extensions"]["code"], "access-denied");
}
