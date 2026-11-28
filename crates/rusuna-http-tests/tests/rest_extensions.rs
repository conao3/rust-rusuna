mod support;

use async_graphql::{Request, Variables, dynamic::Schema};
use axum::{
    Json as Response, Router,
    extract::Path,
    routing::{get, post},
};
use rusuna::Session;
use rusuna_async_graphql::{Builder, RestSource};
use serde_json::{Value as Json, json};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::net::TcpListener;

const SDL: &str = r#"
    scalar DateTime
    enum State { Zulu Alpha }
    type Query {
      restRows: [RestRow!]! @rest(source: "api", path: "/rows", filter: true, roles: ["user", "limited"])
      tableRows: [TableRow!]! @list
      tableRows_aggregate: TableRow_aggregate! @aggregate(target: "TableRow")
      parent: Parent! @rest(source: "api", path: "/parent", roles: ["user"])
      plain: [RestRow!]! @rest(source: "api", path: "/rows", roles: ["user"])
      one(id: Int!): RestRow! @rest(source: "api", path: "/row/{{id}}", roles: ["user"])
      invalid: [RestRow!] @rest(source: "api", path: "/invalid", filter: true)
    }
    type Mutation {
      update(scale: Int!): [RestRow!]!
        @rest(source: "api", path: "/write", method: POST, filter: true, roles: ["user"])
    }
    type Parent {
      id: Int!
      rows: [RestRow!]! @rest(source: "api", path: "/parent/{{id}}/rows", filter: true, roles: ["user"])
    }
    type TableRow
      @table(name: "rest_extension_rows", read_only: true)
      @permission(role: "user", columns: ["id"], where: {})
      @permission(role: "limited", columns: ["id", "label", "score"], where: {owner: {_eq: "$owner"}}, allow_aggregations: true, limit: 2) {
      id: Int!
      owner: Int!
      label: String
      score: Float
      moment: DateTime!
      rest: RestRow! @rest(source: "api", path: "/row/{{id}}", roles: ["user"])
    }
    type RestRow
      @permission(role: "user", filter: {owner: {_eq: "X-Hasura-User-Id"}}, columns: ["id", "label", "score", "detail", "state", "moment", "relation"], limit: 2)
      @permission(role: "limited", filter: {owner: {_eq: "$owner"}}, columns: ["id", "label", "score"], limit: 2) {
      id: Int!
      owner: Int! @column(name: "owner_id")
      label: String
      score: Float
      moment: DateTime!
      detail: Detail
      state: State!
      relation: TableRow @relation(columns: ["id"], references: ["id"])
      hooked: RestRow @rest(source: "api", path: "/row/{{id}}", roles: ["user"])
    }
    type Detail { title: String @column(name: "description") }
"#;

fn rows() -> Json {
    let mut rows = Vec::new();
    for (id, owner, label, score, moment, title, state) in [
        (
            5,
            9,
            Some("before"),
            Some(0.25),
            "2026-02-01T02:00:00Z",
            Some("skip"),
            "Alpha",
        ),
        (
            3,
            7,
            Some("AlphaX%"),
            Some(3.125),
            "2026-02-01T00:30:00-02:00",
            Some("visible"),
            "Zulu",
        ),
        (
            1,
            7,
            Some("alpha_%"),
            Some(1.25),
            "2026-02-01T02:00:00+03:00",
            None,
            "Alpha",
        ),
        (
            6,
            9,
            Some("after"),
            Some(4.0625),
            "2026-02-01T01:00:00Z",
            Some("skip"),
            "Zulu",
        ),
        (
            4,
            7,
            Some("alphaXY"),
            None,
            "2026-02-01T00:30:00Z",
            Some("visible"),
            "Zulu",
        ),
        (
            2,
            7,
            Some("Ä_"),
            Some(4.5),
            "2026-02-01T00:00:00Z",
            Some("visible"),
            "Alpha",
        ),
        (
            7,
            7,
            None,
            Some(5.25),
            "2026-02-01T05:00:00+02:00",
            None,
            "Alpha",
        ),
    ] {
        rows.push(json!({
            "id": id, "owner_id": owner, "label": label, "score": score,
            "moment": moment, "detail": title.map(|title| json!({"description":title})),
            "state": state
        }));
    }
    Json::Array(rows)
}

async fn source() -> RestSource {
    let router = Router::new()
        .route("/rows", get(|| async { Response(rows()) }))
        .route(
            "/write",
            post(|Response(body): Response<Json>| async move {
                assert_eq!(body, json!({"scale": 4}));
                Response(rows())
            }),
        )
        .route("/parent", get(|| async { Response(json!({"id": 1})) }))
        .route("/parent/1/rows", get(|| async { Response(rows()) }))
        .route(
            "/row/{id}",
            get(|Path(id): Path<i32>| async move {
                Response(
                    rows()
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|row| row["id"] == id)
                        .unwrap()
                        .clone(),
                )
            }),
        )
        .route(
            "/invalid",
            get(|| async {
                let mut rows = rows();
                rows[0]["score"] = json!("invalid");
                Response(rows)
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    RestSource::new("api", &format!("http://{address}")).unwrap()
}

fn schema(source: RestSource, role: &str, pool: Option<PgPool>) -> Schema {
    let mut schema = Builder::new(Arc::new(rusuna::Schema::from_sdl(SDL).unwrap()), role)
        .rest(source)
        .build()
        .unwrap()
        .schema();
    if let Some(pool) = pool {
        schema = schema.data(pool);
    }
    schema.finish().unwrap()
}

async fn execute(schema: &Schema, session: &Session, query: &str, variables: Json) -> Json {
    let response = schema
        .execute(
            Request::new(query)
                .data(session.clone())
                .variables(Variables::from_json(variables)),
        )
        .await;
    serde_json::to_value(response).unwrap()
}

#[tokio::test]
async fn rest_filter_order_pagination_and_aggregates_match_postgres() {
    let pool = support::database("http_rest_extensions", &[]).await;
    sqlx::query("CREATE TABLE rest_extension_rows (id int, owner int, label text COLLATE \"C\", score double precision, moment timestamptz)")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO rest_extension_rows SELECT id, owner_id, label, score, moment::timestamptz FROM jsonb_to_recordset($1) AS t(id int, owner_id int, label text, score double precision, moment text)")
        .bind(rows())
        .execute(&pool).await.unwrap();
    let source = source().await;
    let user = schema(source.clone(), "user", Some(pool.clone()));
    let limited = schema(source.clone(), "limited", Some(pool.clone()));
    let schema = schema(source, "admin", Some(pool));
    for (arguments, fields) in [
        (
            r#"(where: {_and: [{score: {_gt: 1.0, _lte: 5.25}}, {id: {_nin: [2, 6]}}], _or: [{label: {_is_null: true}}, {label: {_ilike: "alpha_%"}}]}, order_by: [{score: desc}, {id: asc}], offset: 1, limit: 2)"#,
            "id label score",
        ),
        (
            r#"(where: {_not: {label: {_like: "alpha\\_%"}}}, order_by: [{label: asc}, {id: desc}])"#,
            "id label",
        ),
        (
            r#"(where: {moment: {_eq: "2026-02-01T02:30:00Z"}, owner: {_neq: 9}}, order_by: {moment: asc})"#,
            "id",
        ),
        (
            r#"(where: {_or: [{score: {_gte: 3.0}}, {score: {_lt: 0.3}}, {id: {_in: [3, 4]}}]}, order_by: [{score: desc}, {id: asc}])"#,
            "id score",
        ),
        ("(where: {id: {_in: []}})", "id"),
        (
            "(where: {id: {_nin: []}}, order_by: [{score: asc}, {id: asc}], limit: 3)",
            "id",
        ),
        (
            "(order_by: [{score: asc_nulls_first}, {id: asc}], offset: 1, limit: 2)",
            "id",
        ),
        ("(order_by: {moment: desc}, limit: 0)", "id"),
        ("(order_by: {moment: asc}, offset: 1, limit: 3)", "id"),
    ] {
        let query = format!(
            r#"{{
            restRows{arguments} {{ {fields} }}
            tableRows{arguments} {{ {fields} }}
            restRows_aggregate{arguments} {{
                summary: aggregate {{ n: count sum {{ score }} avg {{ score }} min {{ label id }} max {{ label id }} }}
                page: nodes {{ {fields} relation {{ id }} }}
            }}
            tableRows_aggregate{arguments} {{
                summary: aggregate {{ n: count sum {{ score }} avg {{ score }} min {{ label id }} max {{ label id }} }}
                page: nodes {{ {fields} }}
            }}
        }}"#
        );
        let response = execute(&schema, &Session::new("admin"), &query, json!({})).await;
        assert!(response.get("errors").is_none(), "{query}: {response}");
        let data = &response["data"];
        assert_eq!(data["restRows"], data["tableRows"], "{query}");
        let mut aggregate = data["restRows_aggregate"].clone();
        for node in aggregate["page"].as_array_mut().unwrap() {
            assert_eq!(node["relation"]["id"], node["id"]);
            node.as_object_mut().unwrap().remove("relation");
        }
        assert_eq!(aggregate, data["tableRows_aggregate"], "{query}");
    }
    let limited_session = Session::from_json("limited", &json!({"owner": 7})).unwrap();
    let selection =
        "{ aggregate { count sum { score } avg { score } min { id } max { id } } nodes { id } }";
    for (arguments, count, ids) in [
        ("(order_by: {id: asc})", 5, json!([{"id": 1}, {"id": 2}])),
        (
            "(where: {id: {_gte: 2}}, order_by: {id: desc}, offset: 1, limit: 3)",
            3,
            json!([{"id": 4}, {"id": 3}]),
        ),
        ("(order_by: {id: asc}, limit: 1)", 1, json!([{"id": 1}])),
        ("(order_by: {id: asc}, limit: 0)", 0, json!([])),
    ] {
        let query = format!(
            "{{ restRows_aggregate{arguments} {selection} tableRows_aggregate{arguments} {selection} }}"
        );
        let response = execute(&limited, &limited_session, &query, json!({})).await;
        assert!(response.get("errors").is_none(), "{query}: {response}");
        let data = &response["data"];
        assert_eq!(
            data["restRows_aggregate"], data["tableRows_aggregate"],
            "{query}"
        );
        assert_eq!(data["restRows_aggregate"]["aggregate"]["count"], count);
        assert_eq!(data["restRows_aggregate"]["nodes"], ids);
    }

    let session = Session::from_json("user", &json!({"X-Hasura-User-Id": 7})).unwrap();
    let response = execute(
        &user,
        &session,
        "{ tableRows(where: {id: {_in: [1, 5]}}, order_by: {id: asc}) { id rest { id } } }",
        json!({}),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"tableRows": [
            {"id": 1, "rest": {"id": 1}},
            {"id": 5, "rest": null},
        ]}})
    );
}

#[tokio::test]
async fn rest_permissions_filter_every_call_and_hide_columns() {
    let schema = schema(source().await, "user", None);
    let session = Session::from_json("user", &json!({"x-hasura-user-id": 7})).unwrap();
    let query = r#"
        query($condition: RestRow_bool_exp, $sort: [RestRow_order_by!]) {
          filtered: restRows(where: $condition, order_by: $sort, offset: 1, limit: 4) { id }
          restRows_aggregate(order_by: $sort, limit: 4) { aggregate { count min { id } } nodes { id } }
          parent { rows(order_by: $sort, offset: 2) { id } }
          plain { id }
          blocked: one(id: 5) { id }
          allowed: one(id: 7) { id }
        }
    "#;
    let response = execute(
        &schema,
        &session,
        query,
        json!({
            "condition": {"id": {"_gt": 1}},
            "sort": [{"id": "asc"}]
        }),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {
            "filtered": [{"id": 3}, {"id": 4}],
            "restRows_aggregate": {"aggregate": {"count": 4, "min": {"id": 1}}, "nodes": [{"id": 1}, {"id": 2}]},
            "parent": {"rows": [{"id": 3}, {"id": 4}]},
            "plain": [{"id": 3}, {"id": 1}],
            "blocked": null,
            "allowed": {"id": 7}
        }})
    );
    for query in [
        "{ plain { owner } }",
        "{ plain { hooked { id } } }",
        "{ restRows(where: {owner: {_eq: 7}}) { id } }",
        "{ restRows(order_by: {owner: asc}) { id } }",
        "{ restRows_aggregate { aggregate { max { owner } } } }",
    ] {
        let response = execute(&schema, &session, query, json!({})).await;
        assert!(response.get("errors").is_some(), "{query}: {response}");
    }
    for session in [
        Session::new("user"),
        Session::from_json("user", &json!({"X-Hasura-User-Id": false})).unwrap(),
    ] {
        let response = execute(&schema, &session, "{ one(id: 1) { id } }", json!({})).await;
        assert_eq!(
            response["errors"][0]["extensions"]["code"],
            "validation-failed"
        );
        assert_eq!(response["data"]["one"], Json::Null);
    }
}

#[tokio::test]
async fn rest_filter_handles_nested_objects_enums_and_unicode() {
    let schema = schema(source().await, "admin", None);
    let query = r#"
        {
          nested: restRows(where: {detail: {title: {_eq: "visible"}}, state: {_gte: Alpha}}, order_by: [{state: asc}, {id: desc}]) { id state }
          like: restRows(where: {label: {_ilike: "ä\\_"} }) { id }
          wildcard: restRows(where: {label: {_like: "_\\_"}}) { id }
          invalid(where: {id: {_eq: 1}}) { id }
        }
    "#;
    let response = execute(&schema, &Session::new("admin"), query, json!({})).await;
    assert_eq!(
        response["data"],
        json!({
            "nested": [{"id": 2, "state": "Alpha"}, {"id": 4, "state": "Zulu"}, {"id": 3, "state": "Zulu"}],
            "like": [{"id": 2}],
            "wildcard": [{"id": 2}],
            "invalid": null
        })
    );
    assert_eq!(response["errors"].as_array().unwrap().len(), 1);
    assert_eq!(
        response["errors"][0]["extensions"]["reason"],
        "unexpected-response"
    );
    let response = execute(
        &schema,
        &Session::new("admin"),
        "{ restRows(where: {hooked: {id: {_eq: 1}}}) { id } }",
        json!({}),
    )
    .await;
    assert!(response.get("errors").is_some(), "{response}");
}

#[tokio::test]
async fn rest_mutation_filters_the_result_after_sending_declared_arguments() {
    let schema = schema(source().await, "user", None);
    let session = Session::from_json("user", &json!({"x-hasura-user-id": 7})).unwrap();
    let response = execute(
        &schema,
        &session,
        "mutation {
            update(scale: 4, where: {id: {_gte: 2}}, order_by: {id: asc}, limit: 1) { id }
            update_aggregate(scale: 4, order_by: {id: desc}, offset: 1) {
                aggregate { count }
                nodes { id }
            }
        }",
        json!({}),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {
            "update": [{"id": 2}],
            "update_aggregate": {
                "aggregate": {"count": 4},
                "nodes": [{"id": 4}, {"id": 3}]
            }
        }})
    );
}
