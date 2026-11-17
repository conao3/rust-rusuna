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
        let compiled = schema.compile(&case.query).unwrap();
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
