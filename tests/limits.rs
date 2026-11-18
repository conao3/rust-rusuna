use postgres::types::private::BytesMut;
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, NoTls};
use rusuna::{Schema, Session, Value};
use serde_json::{Value as Json, json};
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
            (Value::Int(v), &Type::INT4) => i32::try_from(*v)?.to_sql(ty, out),
            (Value::Int(v), &Type::INT8) => v.to_sql(ty, out),
            (Value::Text(v), &Type::TEXT | &Type::VARCHAR) => v.to_sql(ty, out),
            (Value::Bool(v), &Type::BOOL) => v.to_sql(ty, out),
            (Value::Float(v), &Type::FLOAT8) => v.to_sql(ty, out),
            (Value::Null, _) => Ok(IsNull::Yes),
            (value, ty) => Err(format!("{value:?} is not a {ty}").into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/limits")
}

fn schema() -> Schema {
    Schema::from_sdl(include_str!("fixtures/limits.graphql")).unwrap()
}

fn client() -> Client {
    Client::connect(
        &std::env::var("RUSUNA_POSTGRES").expect("run the tests with make check"),
        NoTls,
    )
    .unwrap()
}

fn fetch(client: &mut Client, compiled: &rusuna::Compiled) -> Json {
    let params: Vec<_> = compiled.params.iter().map(Param).collect();
    let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
    client.query_one(&compiled.sql, &params).unwrap().get(0)
}

#[test]
fn limited_results_equal_hasura_for_each_case() {
    let mut client = client();
    let schema = schema();
    let fixtures = fixtures();
    let cases: Json = serde_json::from_str(include_str!("fixtures/limits/cases.json")).unwrap();
    assert_eq!(cases.as_array().unwrap().len(), 17);
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let role = case["role"].as_str().unwrap();
        client
            .query_one(
                "SELECT set_config('hasura.user', $1, false)",
                &[&json!({"x-hasura-role": role}).to_string()],
            )
            .unwrap();
        let hasura = std::fs::read_to_string(fixtures.join(format!("hasura/{name}.sql"))).unwrap();
        let expected: Json = client.query_one(&hasura, &[]).unwrap().get(0);
        let query =
            std::fs::read_to_string(fixtures.join(format!("queries/{name}.graphql"))).unwrap();
        let session = Session::from_json(role, &case["session"]).unwrap();
        let compiled = schema
            .compile(&query, &json!({}), None, &session)
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
        assert_eq!(fetch(&mut client, &compiled), expected, "{name}");
    }
}

#[test]
fn returning_caps_relationships_and_keeps_every_changed_row() {
    let mut client = client();
    let schema = schema();
    let cases: Json = serde_json::from_str(include_str!("fixtures/limits/mutations.json")).unwrap();
    assert_eq!(cases.as_array().unwrap().len(), 3);
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let session = Session::from_json(case["role"].as_str().unwrap(), &case["session"]).unwrap();
        let fields = schema
            .compile(case["query"].as_str().unwrap(), &json!({}), None, &session)
            .unwrap();
        let mut transaction = client.transaction().unwrap();
        let mut data = serde_json::Map::new();
        for field in fields {
            let params: Vec<_> = field.compiled.params.iter().map(Param).collect();
            let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
            let value: Json = transaction
                .query_one(&field.compiled.sql, &params)
                .unwrap()
                .get(0);
            data.insert(field.response_key, value);
        }
        transaction.rollback().unwrap();
        assert_eq!(json!({ "data": data }), case["response"], "{name}");
    }
}

#[test]
fn admin_reads_are_not_limited() {
    let mut client = client();
    let compiled = schema()
        .compile(
            "{ author(order_by: {id: asc}) { id } }",
            &json!({}),
            None,
            &Session::new("admin"),
        )
        .unwrap()
        .remove(0)
        .compiled;
    assert!(!compiled.sql.contains("LIMIT"));
    assert_eq!(fetch(&mut client, &compiled).as_array().unwrap().len(), 6);
}

#[test]
fn limits_do_not_change_the_parameters() {
    let schema = schema();
    let query = "{ article(where: {id: {_lt: 9}}, order_by: {id: asc}, limit: 7) { id comments_aggregate(where: {id: {_gt: 1}}) { nodes { id } } } }";
    let compiled = schema
        .compile(query, &json!({}), None, &Session::new("limited"))
        .unwrap()
        .remove(0)
        .compiled;
    assert_eq!(
        compiled.params,
        vec![Value::Int(9), Value::Int(2), Value::Int(1)]
    );
    assert!(compiled.sql.contains("LIMIT 3"));
    assert!(compiled.sql.contains("[1: 2 ]"));
}
