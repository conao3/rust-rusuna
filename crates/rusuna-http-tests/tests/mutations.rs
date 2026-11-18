mod support;

use serde_json::{Value as Json, json};
use sqlx::Row;
use support::*;

fn canonical(value: Json) -> Json {
    match value {
        Json::Array(values) => Json::Array(values.into_iter().map(canonical).collect()),
        Json::Object(values) => Json::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let mut value = canonical(value);
                    if key == "returning"
                        && let Json::Array(values) = &mut value
                    {
                        values.sort_by_key(|v| v.to_string());
                    }
                    (key, value)
                })
                .collect(),
        ),
        value => value,
    }
}

async fn state(pool: &sqlx::PgPool) -> Json {
    sqlx::raw_sql(sqlx::AssertSqlSafe(fixture("mutations/state.sql")))
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0)
}

async fn reseed(pool: &sqlx::PgPool) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(fixture("mutations/seed.sql")))
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn mutations_over_http_match_hasura_responses_and_tables() {
    let pool = database("http_mutations", &["mutations/schema.sql"]).await;
    let address = serve(schemas(&fixture("mutations/schema.graphql"), &pool, None)).await;
    let mut paths: Vec<_> = std::fs::read_dir(format!("{ROOT}/tests/fixtures/mutations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|s| s == "json") && p.file_stem().unwrap() != "metadata"
        })
        .collect();
    paths.sort();
    let (mut compared, mut violations, mut multiple) = (0, 0, 0);
    for path in paths {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let fixture: Json = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        reseed(&pool).await;
        let actual = post(
            address,
            &role_headers(fixture["role"].as_str().unwrap(), &fixture["session"]),
            &json!({ "query": fixture["query"] }),
        )
        .await;
        let expected = &fixture["response"];
        if let Some(errors) = expected.get("errors") {
            assert!(actual["data"].is_null(), "{name}: {actual}");
            let code = &errors[0]["extensions"]["code"];
            if code == "permission-error" {
                assert_eq!(actual["errors"].as_array().unwrap().len(), 1, "{name}");
                assert_eq!(
                    actual["errors"][0]["message"], errors[0]["message"],
                    "{name}"
                );
                assert_eq!(actual["errors"][0]["extensions"]["code"], *code, "{name}");
                violations += 1;
            } else {
                assert!(!actual["errors"].as_array().unwrap().is_empty(), "{name}");
            }
        } else {
            assert_eq!(canonical(actual), canonical(expected.clone()), "{name}");
        }
        if fixture["query"].as_str().unwrap().contains("second:") {
            multiple += 1;
        }
        assert_eq!(state(&pool).await, fixture["tables"], "{name}");
        compared += 1;
    }
    assert_eq!(compared, 64);
    assert_eq!(violations, 7);
    assert_eq!(multiple, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_later_root_field_rolls_back_the_earlier_ones() {
    let pool = database("http_rollback", &["mutations/schema.sql"]).await;
    let address = serve(schemas(&fixture("mutations/schema.graphql"), &pool, None)).await;
    reseed(&pool).await;
    let before = state(&pool).await;
    let actual = post(
        address,
        &[],
        &json!({ "query": "mutation { first: insert_mutation_item_one(object: {id: 10, title: \"new\"}) { id } second: update_mutation_item_by_pk(pk_columns: {id: 2}, _set: {title: \"changed\"}) { id } third: insert_mutation_item_one(object: {id: 1, title: \"duplicate\"}) { id } fourth: delete_mutation_item_by_pk(id: 3) { id } }" }),
    )
    .await;
    assert!(actual["data"].is_null(), "{actual}");
    assert_eq!(actual["errors"].as_array().unwrap().len(), 1, "{actual}");
    assert_eq!(
        actual["errors"][0]["extensions"]["code"],
        "constraint-violation"
    );
    assert_eq!(state(&pool).await, before);
    let committed = post(
        address,
        &[],
        &json!({ "query": "mutation { first: insert_mutation_item_one(object: {id: 10, title: \"new\"}) { id } second: delete_mutation_item_by_pk(id: 3) { id } }" }),
    )
    .await;
    assert_eq!(
        committed,
        json!({ "data": { "first": { "id": 10 }, "second": { "id": 3 } } })
    );
    let ids: Vec<i64> = state(&pool).await["mutation_item"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [1, 2, 10]);
}

#[tokio::test(flavor = "multi_thread")]
async fn enum_inputs_and_repeated_root_fields_follow_the_document() {
    let pool = database("http_enums", &["mutations/schema.sql"]).await;
    let address = serve(schemas(&fixture("mutations/schema.graphql"), &pool, None)).await;
    reseed(&pool).await;
    let upsert = post(
        address,
        &[],
        &json!({
            "query": "mutation Upsert($conflict: mutation_item_on_conflict, $order: [mutation_item_order_by!]) { insert_mutation_item_one(object: {id: 1, title: \"renamed\"}, on_conflict: $conflict) { id title owner { items(order_by: $order) { id } } } }",
            "variables": {
                "conflict": {"constraint": "mutation_item_pkey", "update_columns": ["title"]},
                "order": [{"id": "desc"}],
            },
        }),
    )
    .await;
    assert_eq!(
        upsert,
        json!({ "data": { "insert_mutation_item_one": { "id": 1, "title": "renamed", "owner": { "items": [{ "id": 3 }, { "id": 1 }] } } } })
    );
    reseed(&pool).await;
    let repeated = post(
        address,
        &[],
        &json!({ "query": "mutation { a: insert_mutation_item_one(object: {id: 10}) { id } a: insert_mutation_item_one(object: {id: 10}) { title } }" }),
    )
    .await;
    assert!(repeated["data"].is_null(), "{repeated}");
    assert_eq!(
        repeated["errors"].as_array().unwrap().len(),
        1,
        "{repeated}"
    );
    assert_eq!(
        repeated["errors"][0]["extensions"]["code"],
        "validation-failed"
    );
    let ids: Vec<i64> = state(&pool).await["mutation_item"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [1, 2, 3]);
}
