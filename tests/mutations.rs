use postgres::types::private::BytesMut;
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, NoTls};
use rusuna::{Schema, Session, Value};
use serde_json::{Value as Json, json};
use std::error::Error;
use std::path::Path;

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

#[test]
fn mutation_responses_and_state_match_hasura() {
    let mut client = Client::connect(
        &std::env::var("RUSUNA_POSTGRES").expect("run make check"),
        NoTls,
    )
    .unwrap();
    client
        .batch_execute(include_str!("fixtures/mutations/schema.sql"))
        .unwrap();
    let schema = Schema::from_sdl(include_str!("fixtures/mutations/schema.graphql")).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths: Vec<_> = std::fs::read_dir(root.join("tests/fixtures/mutations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|s| s == "json") && p.file_stem().unwrap() != "metadata"
        })
        .collect();
    paths.sort();
    for path in paths {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let fixture: Json = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let session =
            Session::from_json(fixture["role"].as_str().unwrap(), &fixture["session"]).unwrap();
        let compiled = schema.compile(
            fixture["query"].as_str().unwrap(),
            &json!({}),
            None,
            &session,
        );
        if name.starts_with("denied_") {
            assert!(fixture["response"]["errors"].is_array());
            assert!(compiled.is_err(), "{name}");
            continue;
        }
        let fields = compiled.unwrap_or_else(|e| panic!("{name}: {e}"));
        let expected_sql = fields
            .iter()
            .map(|f| format!("{}\n", f.compiled.sql))
            .collect::<String>();
        let snapshot = root.join(format!("tests/fixtures/expected/mutation_{name}.sql"));
        if std::env::var_os("RUSUNA_UPDATE_EXPECTED").is_some() {
            std::fs::write(&snapshot, &expected_sql).unwrap();
        }
        assert_eq!(
            expected_sql,
            std::fs::read_to_string(snapshot).unwrap(),
            "{name}"
        );
        if fixture["response"]["errors"].is_array() && fields.len() == 1 {
            client
                .batch_execute(include_str!("fixtures/mutations/seed.sql"))
                .unwrap();
            client
                .batch_execute("SET plan_cache_mode = force_generic_plan")
                .unwrap();
            let compiled = &fields[0].compiled;
            let params: Vec<_> = compiled.params.iter().map(Param).collect();
            let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
            let error = client.query_one(&compiled.sql, &params).unwrap_err();
            let db = error.as_db_error().unwrap();
            assert!(rusuna::is_permission_check_error(
                db.code().code(),
                db.message()
            ));
            let state: Json = client
                .query_one(include_str!("fixtures/mutations/state.sql"), &[])
                .unwrap()
                .get(0);
            assert_eq!(
                state, fixture["tables"],
                "{name} must fail without a caller rollback"
            );
        }
        for generic in [false, true] {
            client
                .batch_execute(include_str!("fixtures/mutations/seed.sql"))
                .unwrap();
            client
                .batch_execute(if generic {
                    "SET plan_cache_mode = force_generic_plan"
                } else {
                    "SET plan_cache_mode = force_custom_plan"
                })
                .unwrap();
            let mut transaction = client.transaction().unwrap();
            let mut response = serde_json::Map::new();
            let mut rejected = false;
            for field in &fields {
                let params: Vec<_> = field.compiled.params.iter().map(Param).collect();
                let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
                match transaction.query_one(&field.compiled.sql, &params) {
                    Ok(row) => {
                        response.insert(
                            field.response_key.clone(),
                            row.get::<_, Option<Json>>(0).unwrap_or(Json::Null),
                        );
                    }
                    Err(error) => {
                        assert_eq!(
                            fixture["response"]["errors"][0]["extensions"]["code"],
                            "permission-error",
                            "{name}: {error}"
                        );
                        let db = error.as_db_error().unwrap();
                        assert!(
                            rusuna::is_permission_check_error(db.code().code(), db.message()),
                            "{name}: {error}"
                        );
                        rejected = true;
                        break;
                    }
                }
            }
            if rejected {
                transaction.rollback().unwrap();
            } else {
                assert!(
                    fixture["response"].get("errors").is_none(),
                    "{name} should fail"
                );
                assert_eq!(
                    canonical(json!({"data": response})),
                    canonical(fixture["response"].clone()),
                    "{name}, generic={generic}"
                );
                transaction.commit().unwrap();
            }
            let state: Json = client
                .query_one(include_str!("fixtures/mutations/state.sql"), &[])
                .unwrap()
                .get(0);
            assert_eq!(state, fixture["tables"], "{name}, generic={generic}");
        }
    }
}

#[test]
fn mutation_expanded_schemas_are_committed() {
    let schema = Schema::from_sdl(include_str!("fixtures/mutations/schema.graphql")).unwrap();
    for role in ["admin", "user", "editor", "writer", "visitor"] {
        let expanded = schema.expanded_sdl(role).unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/mutations/{role}.expanded.graphql"));
        if std::env::var_os("RUSUNA_UPDATE_EXPECTED").is_some() {
            std::fs::write(&path, &expanded).unwrap();
        }
        assert_eq!(expanded, std::fs::read_to_string(path).unwrap());
        async_graphql_parser::parse_schema(&expanded).unwrap();
    }
}

#[test]
fn primary_keys_and_mutation_arguments_are_validated() {
    let schema = Schema::from_sdl(include_str!("fixtures/mutations/schema.graphql")).unwrap();
    for query in [
        "mutation { insert_mutation_item_one { id } }",
        "mutation { update_mutation_item(_set: {score: 1}) { affected_rows } }",
        "mutation { update_mutation_item_by_pk(pk_columns: {}, _set: {score: 1}) { id } }",
        "mutation { delete_mutation_item_by_pk(id: null) { id } }",
        "mutation { insert_mutation_item_one(object: {owner: {data: {id: 1}}}) { id } }",
        "mutation { update_mutation_item(where: {}, _append: {title: \"x\"}) { affected_rows } }",
        "mutation { update_mutation_item(where: {}, _inc: {title: 1}) { affected_rows } }",
        "mutation { update_mutation_item(where: {}, _set: {score: 1}, _inc: {score: 1}) { affected_rows } }",
        "mutation { insert_mutation_item_one(object: {}, on_conflict: {constraint: mutation_item_pkey}) { id } }",
    ] {
        assert!(
            schema
                .compile(query, &json!({}), None, &Session::new("admin"))
                .is_err(),
            "{query}"
        );
    }
    let query = "mutation M($rows: [mutation_item_insert_input!]!) { first: insert_mutation_item(objects: $rows) { affected_rows returning { ...Row } } } fragment Row on mutation_item { id title }";
    assert!(
        schema
            .compile(
                query,
                &json!({"rows": [{"title": "a"}]}),
                Some("M"),
                &Session::new("admin")
            )
            .is_ok()
    );
    let schema = Schema::from_sdl("type pair @table(name: \"pair\") @primary_key(columns: [\"a\", \"b\"]) { a: Int! b: String! } type Query { pair: [pair!]! @list }").unwrap();
    assert!(
        schema
            .compile(
                "{ pair_by_pk(a: 1, b: \"x\") { a b } }",
                &json!({}),
                None,
                &Session::new("admin")
            )
            .is_ok()
    );
    assert!(
        schema
            .compile(
                "{ pair_by_pk(a: 1) { a } }",
                &json!({}),
                None,
                &Session::new("admin")
            )
            .is_err()
    );
}

#[test]
fn low_level_mutations_enforce_columns_presets_and_roles() {
    let schema = Schema::from_sdl(include_str!("fixtures/mutations/schema.graphql")).unwrap();
    let user = Session::new("user").variable("user_id", Value::Int(1));
    let fields = [rusuna::Selection::new("affected_rows")];
    for column in ["owner_id", "secret", "unknown"] {
        let mut input = rusuna::MutationArguments::new("mutation_item", rusuna::WriteKind::Insert);
        input.objects = vec![vec![(column.into(), Value::Int(2))]];
        assert!(schema.compile_mutation(&input, &fields, &user).is_err());
    }
    let input = rusuna::MutationArguments::new("mutation_item", rusuna::WriteKind::Delete);
    assert!(
        schema
            .compile_mutation(&input, &fields, &Session::new("visitor"))
            .is_err()
    );
    assert!(!rusuna::is_permission_check_error(
        "23505",
        rusuna::PERMISSION_CHECK_MARKER
    ));
    assert!(!rusuna::is_permission_check_error(
        "22P02",
        "unrelated cast error"
    ));
}

#[test]
fn conflict_updates_reject_primary_key_presets() {
    let schema = Schema::from_sdl(r#"
        type t @table(name: "t") @primary_key(columns: ["id"])
          @permission(role: "user", columns: ["id"], where: {})
          @insert_permission(role: "user", columns: ["id"], check: {})
          @update_permission(role: "user", columns: ["title"], where: {}, check: {}, set: {id: 42}) { id: Int! title: String }
        type Query { t: [t!]! @list }
    "#).unwrap();
    assert!(schema.compile("mutation { insert_t_one(object: {id: 1}, on_conflict: {constraint: t_pkey, update_columns: [title]}) { id } }", &json!({}), None, &Session::new("user")).is_err());
}

#[test]
fn write_permission_directives_reject_unknown_names_and_root_placements() {
    for sdl in [
        "type t @table(name: \"t\") @write_permission(role: \"user\") { id: Int! } type Query { t: [t!]! @list }",
        "type t @table(name: \"t\") { id: Int! } type Query @delete_permission(role: \"user\", where: {}) { t: [t!]! @list }",
    ] {
        assert!(Schema::from_sdl(sdl).is_err());
    }
}

#[test]
fn an_update_applies_to_a_row_changed_by_a_concurrent_transaction() {
    let url = std::env::var("RUSUNA_POSTGRES").expect("run make check");
    let mut first = Client::connect(&url, NoTls).unwrap();
    first
        .batch_execute(
            "CREATE TABLE concurrent_counter (id integer PRIMARY KEY, score integer NOT NULL);
             INSERT INTO concurrent_counter VALUES (1, 1);",
        )
        .unwrap();
    first
        .batch_execute("BEGIN; UPDATE concurrent_counter SET score = score + 1 WHERE id = 1;")
        .unwrap();
    let schema = Schema::from_sdl(
        "type counter @table(name: \"concurrent_counter\") @primary_key(columns: [\"id\"]) { id: Int! score: Int! } type Query { counter: [counter!]! @list }",
    )
    .unwrap();
    let fields = schema
        .compile(
            "mutation { update_counter(where: {id: {_eq: 1}}, _inc: {score: 10}) { affected_rows returning { score } } }",
            &json!({}),
            None,
            &Session::new("admin"),
        )
        .unwrap();
    let compiled = fields[0].compiled.clone();
    let second = std::thread::spawn(move || {
        let mut client = Client::connect(&url, NoTls).unwrap();
        let params: Vec<_> = compiled.params.iter().map(Param).collect();
        let refs: Vec<_> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
        let row = client.query_one(&compiled.sql, &refs).unwrap();
        row.get::<_, Json>(0)
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    first.batch_execute("COMMIT").unwrap();
    assert_eq!(
        second.join().unwrap(),
        json!({"affected_rows": 1, "returning": [{"score": 12}]})
    );
}
