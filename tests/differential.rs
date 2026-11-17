mod common;

use postgres::types::private::BytesMut;
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, NoTls};
use rusuna::Value;
use serde_json::Value as Json;
use std::error::Error;
use std::path::PathBuf;

#[derive(Debug)]
struct Param<'a>(&'a Value);

impl ToSql for Param<'_> {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match (self.0, ty) {
            (Value::Int(v), &Type::INT2) => i16::try_from(*v)?.to_sql(ty, out),
            (Value::Int(v), &Type::INT4) => i32::try_from(*v)?.to_sql(ty, out),
            (Value::Int(v), &Type::INT8) => v.to_sql(ty, out),
            (Value::Text(v), &Type::TEXT | &Type::VARCHAR) => v.to_sql(ty, out),
            (Value::Bool(v), &Type::BOOL) => v.to_sql(ty, out),
            (Value::Float(v), &Type::FLOAT8) => v.to_sql(ty, out),
            (Value::Float(v), &Type::FLOAT4) => (*v as f32).to_sql(ty, out),
            (Value::Null, _) => Ok(IsNull::Yes),
            (value, ty) => Err(format!("{value:?} is not a {ty}").into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

fn sorted(value: Json, unordered: &[&str]) -> Json {
    match value {
        Json::Array(items) => Json::Array(
            items
                .into_iter()
                .map(|item| sorted(item, unordered))
                .collect(),
        ),
        Json::Object(fields) => Json::Object(
            fields
                .into_iter()
                .map(|(key, value)| {
                    let value = sorted(value, unordered);
                    match value {
                        Json::Array(mut items) if unordered.contains(&key.as_str()) => {
                            items.sort_by_key(|item| item.to_string());
                            (key, Json::Array(items))
                        }
                        value => (key, value),
                    }
                })
                .collect(),
        ),
        value => value,
    }
}

#[test]
fn results_equal_the_results_of_the_statements_hasura_generates() {
    let config = std::env::var("RUSUNA_POSTGRES")
        .expect("RUSUNA_POSTGRES is not set; run the tests with `make check`");
    let mut client = Client::connect(&config, NoTls).unwrap();
    let schema = common::schema();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hasura");
    for case in common::cases() {
        let hasura = std::fs::read_to_string(fixtures.join(format!("{}.sql", case.name))).unwrap();
        let expected: Json = client.query_one(hasura.as_str(), &[]).unwrap().get(0);
        if case.name != "in_empty"
            && let Json::Array(rows) = &expected
        {
            assert!(!rows.is_empty(), "query {} selects no seed rows", case.name);
        }
        let query = std::fs::read_to_string(
            fixtures
                .parent()
                .unwrap()
                .join(format!("queries/{}.graphql", case.name)),
        )
        .unwrap();
        let compiled = schema
            .compile(
                &query,
                &serde_json::json!({}),
                None,
                &rusuna::Session::new("admin"),
            )
            .unwrap()
            .remove(0)
            .compiled;
        let params: Vec<Param> = compiled.params.iter().map(Param).collect();
        let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
        let actual: Json = client
            .query_one(compiled.sql.as_str(), &params)
            .unwrap()
            .get(0);
        assert_eq!(
            sorted(actual, case.unordered),
            sorted(expected, case.unordered),
            "query {}",
            case.name
        );
    }
}

#[test]
fn renamed_sdl_and_expanded_queries_match_hasura() {
    let config = std::env::var("RUSUNA_POSTGRES").expect("run the tests with make check");
    let mut client = Client::connect(&config, NoTls).unwrap();
    let schema = rusuna::Schema::from_sdl(include_str!("fixtures/renamed-schema.graphql")).unwrap();
    let expected: Json = client
        .query_one(include_str!("fixtures/hasura/a.sql"), &[])
        .unwrap()
        .get(0);
    let literal = "{ articles(limit: 20, order_by: {publishedAt: desc}) { id title author { name } comments(limit: 5, order_by: {id: asc}) { body } } }";
    let expanded = "query A($limit: Int = 20, $show: Boolean! = true) { articles(limit: $limit, order_by: {publishedAt: desc}) { ...Fields comments(limit: 5, order_by: {id: asc}) @include(if: $show) { ... on Comment { body } } ignored: id @skip(if: true) } } fragment Fields on Article { id title author { name } }";
    for (query, variables) in [
        (literal, serde_json::json!({})),
        (expanded, serde_json::json!({})),
        (expanded, serde_json::json!({"limit":20,"show":true})),
    ] {
        let compiled = schema
            .compile(query, &variables, None, &rusuna::Session::new("admin"))
            .unwrap()
            .remove(0)
            .compiled;
        let actual: Json = client.query_one(&compiled.sql, &[]).unwrap().get(0);
        assert_eq!(actual, expected);
    }
    let compiled = schema
        .compile(
            "{ articles(limit: 1, order_by: {id: asc}) { __typename publishedAt } }",
            &serde_json::json!({}),
            None,
            &rusuna::Session::new("admin"),
        )
        .unwrap()
        .remove(0)
        .compiled;
    let actual: Json = client.query_one(&compiled.sql, &[]).unwrap().get(0);
    let expected: Json = client.query_one("SELECT json_agg(row_to_json(x)) FROM (SELECT 'Article' AS \"__typename\", published_at AS \"publishedAt\" FROM article ORDER BY id LIMIT 1) x", &[]).unwrap().get(0);
    assert_eq!(actual, expected);
}

#[test]
fn permission_results_equal_hasura_for_each_role_and_session() {
    let config = std::env::var("RUSUNA_POSTGRES").expect("run the tests with make check");
    let mut client = Client::connect(&config, NoTls).unwrap();
    let schema = rusuna::Schema::from_sdl(include_str!("fixtures/permissions.graphql")).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/permissions");
    let cases: Json =
        serde_json::from_str(include_str!("fixtures/permissions/cases.json")).unwrap();
    assert_eq!(cases.as_array().unwrap().len(), 19);
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let role = case["role"].as_str().unwrap();
        let session = rusuna::Session::from_json(role, &case["session"]).unwrap();
        let mut hasura_user = serde_json::json!({"x-hasura-role": role});
        if let Some(id) = case["session"].get("user_id") {
            hasura_user["x-hasura-user-id"] = Json::String(id.to_string());
        }
        client
            .query_one(
                "SELECT set_config('hasura.user', $1, false)",
                &[&hasura_user.to_string()],
            )
            .unwrap();
        let hasura = std::fs::read_to_string(fixtures.join(format!("hasura/{name}.sql"))).unwrap();
        let expected: Json = client.query_one(&hasura, &[]).unwrap().get(0);
        let query =
            std::fs::read_to_string(fixtures.join(format!("queries/{name}.graphql"))).unwrap();
        let compiled = schema
            .compile(&query, &serde_json::json!({}), None, &session)
            .unwrap()
            .remove(0)
            .compiled;
        let path = fixtures.join(format!("expected/{name}.sql"));
        if std::env::var_os("RUSUNA_UPDATE_EXPECTED").is_some() {
            std::fs::write(&path, format!("{}\n", compiled.sql)).unwrap();
        }
        assert_eq!(
            format!("{}\n", compiled.sql),
            std::fs::read_to_string(&path).unwrap(),
            "{name}"
        );
        let params: Vec<_> = compiled.params.iter().map(Param).collect();
        let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
        let actual: Json = client.query_one(&compiled.sql, &params).unwrap().get(0);
        let unordered: Vec<_> = case["unordered"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            sorted(actual, &unordered),
            sorted(expected, &unordered),
            "{name}"
        );
    }
    let rejected: Vec<(String, String)> =
        serde_json::from_str(include_str!("fixtures/permissions/rejections.json")).unwrap();
    for (role, query) in rejected {
        let session = rusuna::Session::new(role).variable("user_id", Value::Int(3));
        assert!(
            schema
                .compile(&query, &serde_json::json!({}), None, &session)
                .is_err(),
            "{query}"
        );
    }
}
