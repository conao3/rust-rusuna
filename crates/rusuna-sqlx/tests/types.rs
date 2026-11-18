use rusuna::{Schema, Session};
use serde_json::{Value as Json, json};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

const SDL: &str = r#"
scalar DateTime

type typed @table(name: "typed") @primary_key(columns: ["id"])
  @insert_permission(role: "user", columns: ["id", "small"], check: {small: {_gte: 0}}) {
  id: Int!
  small: Int
  big: Int
  single: Float
  double: Float
  label: String
  code: String
  flag: Boolean
  at: DateTime
  local: DateTime
  day: DateTime
}

type Query { typed: [typed!]! @list }
"#;

const TABLE: &str = "CREATE TABLE typed (id int4 PRIMARY KEY, small int2, big int8, single float4, double float8, label varchar(20), code char(3), flag bool, at timestamptz, local timestamp, day date);
INSERT INTO typed VALUES
  (1, 1, 10, 1.5, 2.25, 'one', 'abc', true, '2020-01-01 00:00:00+00', '2020-01-01 00:00:00', '2020-01-01'),
  (2, 2, 20, 2.5, 3.25, 'two', 'def', false, '2020-01-02 12:30:00+09', '2020-01-02 12:30:00', '2020-01-02');";

fn options(database: &str) -> PgConnectOptions {
    let config = std::env::var("RUSUNA_POSTGRES").expect("run the tests with make check");
    let field = |name: &str| {
        config
            .split(' ')
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("RUSUNA_POSTGRES has no `{name}`"))
            .to_string()
    };
    PgConnectOptions::new()
        .socket(field("host"))
        .username(&field("user"))
        .database(database)
}

async fn pool(name: &str) -> PgPool {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options("postgres"))
        .await
        .unwrap();
    for statement in [
        format!("DROP DATABASE IF EXISTS {name}"),
        format!("CREATE DATABASE {name}"),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&admin)
            .await
            .unwrap();
    }
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options(name))
        .await
        .unwrap();
    sqlx::raw_sql(TABLE).execute(&pool).await.unwrap();
    pool
}

async fn run(pool: &PgPool, session: &Session, query: &str) -> Result<Json, rusuna_sqlx::Error> {
    let schema = Schema::from_sdl(SDL).unwrap();
    let fields = schema.compile(query, &json!({}), None, session).unwrap();
    rusuna_sqlx::execute(pool, &fields).await.map(Json::Object)
}

async fn ids(pool: &PgPool, filter: &str) -> Json {
    run(
        pool,
        &Session::new("admin"),
        &format!("{{ typed(where: {filter}, order_by: {{id: asc}}) {{ id }} }}"),
    )
    .await
    .unwrap()["typed"]
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn parameters_take_the_type_postgresql_infers() {
    let pool = pool("sqlx_parameters").await;
    let second = json!([{"id": 2}]);
    for filter in [
        "{small: {_eq: 2}}",
        "{big: {_gt: 15}}",
        "{single: {_eq: 2.5}}",
        "{double: {_gt: 3}}",
        "{label: {_eq: \"two\"}}",
        "{code: {_eq: \"def\"}}",
        "{flag: {_eq: false}}",
        "{at: {_gt: \"2020-01-01T12:00:00Z\"}}",
        "{at: {_gt: \"2020-01-01 12:00:00+00\"}}",
        "{at: {_eq: \"2020-01-02 03:30:00+00:00\"}}",
        "{local: {_gt: \"2020-01-01 12:00\"}}",
        "{day: {_eq: \"2020-01-02\"}}",
        "{id: {_in: [2, 3]}}",
    ] {
        assert_eq!(ids(&pool, filter).await, second, "{filter}");
    }
    let error = run(
        &pool,
        &Session::new("admin"),
        "{ typed(where: {at: {_gt: \"yesterday\"}}) { id } }",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, rusuna_sqlx::Error::Parameter { index: 0, .. }),
        "{error}"
    );
    let overflow = run(
        &pool,
        &Session::new("admin"),
        "{ typed(where: {small: {_eq: 70000}}) { id } }",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(overflow, rusuna_sqlx::Error::Parameter { .. }),
        "{overflow}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mutations_run_in_one_transaction_and_report_permission_checks() {
    let pool = pool("sqlx_mutations").await;
    let inserted = run(
        &pool,
        &Session::new("admin"),
        "mutation { insert_typed_one(object: {id: 3, small: null, label: \"three\"}) { id small label } }",
    )
    .await
    .unwrap();
    assert_eq!(
        inserted,
        json!({"insert_typed_one": {"id": 3, "small": null, "label": "three"}})
    );
    let failed = run(
        &pool,
        &Session::new("admin"),
        "mutation { first: insert_typed_one(object: {id: 4}) { id } second: insert_typed_one(object: {id: 3}) { id } }",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(failed, rusuna_sqlx::Error::Database(_)),
        "{failed}"
    );
    assert_eq!(ids(&pool, "{id: {_gte: 3}}").await, json!([{"id": 3}]));
    let denied = run(
        &pool,
        &Session::new("user"),
        "mutation { first: insert_typed(objects: [{id: 5, small: 1}]) { affected_rows } second: insert_typed(objects: [{id: 6, small: -1}]) { affected_rows } }",
    )
    .await
    .unwrap_err();
    assert!(denied.is_permission_check(), "{denied}");
    assert_eq!(denied.to_string(), rusuna_sqlx::PERMISSION_CHECK_MESSAGE);
    assert_eq!(ids(&pool, "{id: {_gte: 3}}").await, json!([{"id": 3}]));
}

#[tokio::test(flavor = "multi_thread")]
async fn multiplexed_parameters_keep_the_inferred_types() {
    let pool = pool("sqlx_multiplexed").await;
    let schema = Schema::from_sdl(SDL).unwrap();
    let mut conn = pool.acquire().await.unwrap();
    for filters in [
        [
            "{small: {_eq: 1}}",
            "{small: {_eq: 2}}",
            "{small: {_eq: 3}}",
        ],
        [
            "{big: {_gt: 15}}",
            "{big: {_gt: 5}}",
            "{big: {_gt: 2147483647}}",
        ],
        [
            "{single: {_eq: 2.5}}",
            "{single: {_eq: 1.5}}",
            "{single: {_eq: 0.1}}",
        ],
        [
            "{single: {_lt: 2}}",
            "{single: {_lt: 3}}",
            "{single: {_lt: 1}}",
        ],
        [
            "{double: {_gt: 3}}",
            "{double: {_gt: 2.2499999999999996}}",
            "{double: {_gt: 1e300}}",
        ],
        [
            "{label: {_eq: \"two\"}}",
            "{label: {_eq: \"one\"}}",
            "{label: {_like: \"%o%\"}}",
        ],
        [
            "{code: {_eq: \"def\"}}",
            "{code: {_eq: \"abc \"}}",
            "{code: {_eq: \"x'y\"}}",
        ],
        [
            "{flag: {_eq: false}}",
            "{flag: {_eq: true}}",
            "{flag: {_neq: true}}",
        ],
        [
            "{at: {_gt: \"2020-01-01T12:00:00Z\"}}",
            "{at: {_gt: \"2020-01-01 12:00:00+00\"}}",
            "{at: {_gt: \"2020-01-02 03:30:00.5+09:30\"}}",
        ],
        [
            "{local: {_gt: \"2020-01-01 12:00\"}}",
            "{local: {_gt: \"2020-01-01T00:00:00.000001\"}}",
            "{local: {_gt: \"2020-01-02 12:30:00\"}}",
        ],
        [
            "{day: {_eq: \"2020-01-02\"}}",
            "{day: {_eq: \"2020-01-01\"}}",
            "{day: {_eq: \"2019-12-31\"}}",
        ],
        [
            "{id: {_in: [2, 3]}}",
            "{id: {_in: [1, 2]}}",
            "{id: {_nin: [1, 2]}}",
        ],
    ] {
        let compiled: Vec<rusuna::Compiled> = filters
            .iter()
            .map(|filter| {
                schema
                    .compile(
                        &format!("{{ typed(where: {filter}, order_by: {{id: asc}}) {{ id }} }}"),
                        &json!({}),
                        None,
                        &Session::new("admin"),
                    )
                    .unwrap()
                    .remove(0)
                    .compiled
            })
            .collect();
        let multiplexed = rusuna_sqlx::Multiplexed::prepare(&mut conn, &compiled[0])
            .await
            .unwrap();
        let mut arguments = Vec::new();
        let mut expected = Vec::new();
        for c in compiled.iter().filter(|c| c.sql == compiled[0].sql) {
            arguments.push(multiplexed.arguments(&c.params).unwrap());
            expected.push(rusuna_sqlx::fetch(&mut conn, c).await.unwrap());
        }
        assert!(arguments.len() >= 2, "{filters:?}");
        assert_eq!(
            multiplexed.fetch(&mut conn, &arguments).await.unwrap(),
            expected,
            "{filters:?}"
        );
    }
    let compiled = Schema::from_sdl(SDL)
        .unwrap()
        .compile(
            "{ typed(where: {small: {_eq: 1}}) { id } }",
            &json!({}),
            None,
            &Session::new("admin"),
        )
        .unwrap()
        .remove(0)
        .compiled;
    let multiplexed = rusuna_sqlx::Multiplexed::prepare(&mut conn, &compiled)
        .await
        .unwrap();
    for invalid in [
        rusuna::Value::Int(70000),
        rusuna::Value::Text("1".into()),
        rusuna::Value::Float(1.5),
    ] {
        assert!(matches!(
            multiplexed.arguments(&[invalid]),
            Err(rusuna_sqlx::Error::Parameter { index: 0, .. })
        ));
    }
    assert!(multiplexed.arguments(&[]).is_err());
    assert_eq!(
        multiplexed
            .fetch(&mut conn, &[json!(["1"]), json!([null])])
            .await
            .unwrap(),
        vec![json!([{"id": 1}]), json!([])]
    );
}
