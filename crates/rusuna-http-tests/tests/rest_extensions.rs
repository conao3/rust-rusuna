mod support;

use async_graphql::{Request, Variables, dynamic::Schema};
use axum::{
    Json as Response, Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use rusuna::Session;
use rusuna_async_graphql::{Builder, RestSource};
use serde_json::{Value as Json, json};
use sqlx::PgPool;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
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
    serve(router, "").await
}

async fn serve(router: Router, base_path: &str) -> RestSource {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    RestSource::new("api", &format!("http://{address}{base_path}")).unwrap()
}

fn schema(source: RestSource, role: &str, pool: Option<PgPool>) -> Schema {
    schema_from(SDL, source, role, pool)
}

fn schema_from(sdl: &str, source: RestSource, role: &str, pool: Option<PgPool>) -> Schema {
    let mut schema = Builder::new(Arc::new(rusuna::Schema::from_sdl(sdl).unwrap()), role)
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

#[tokio::test]
async fn rest_pagination_joins_all_modes_and_stops_at_max_pages() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let router = Router::new().route(
        "/api/{mode}",
        get({
            let calls = calls.clone();
            move |Path(mode): Path<String>, Query(query): Query<BTreeMap<String, String>>| {
                let calls = calls.clone();
                async move {
                    assert_eq!(query.get("fixed").map(String::as_str), Some("a&b"));
                    let page = match mode.as_str() {
                        "link" => query.get("page").map_or(0, |s| s.parse().unwrap()),
                        "cursor" => match query.get("after").map(String::as_str) {
                            Some("initial") => 0,
                            Some("a &+") => 1,
                            Some("b") => 2,
                            _ => panic!("{query:?}"),
                        },
                        "offset" => {
                            let offset = query["skip"].parse::<usize>().unwrap();
                            assert_eq!(offset % 2, 0);
                            offset / 2
                        }
                        _ => panic!("{mode}"),
                    };
                    calls.lock().unwrap().push((mode.clone(), page));
                    let items = (page * 2..(page * 2 + 2).min(5))
                        .map(|id| json!({"id": id}))
                        .collect::<Vec<_>>();
                    let mut body = json!({"data": {"items": items}});
                    if mode == "cursor" {
                        body["meta"] = match (page, query.get("end").map(String::as_str)) {
                            (0, _) => json!({"next": "a &+"}),
                            (1, _) => json!({"next": "b"}),
                            (2, Some("missing")) => json!({}),
                            (2, Some("null")) => json!({"next": null}),
                            (2, _) => json!({"next": ""}),
                            _ => panic!("{query:?}"),
                        };
                    }
                    let mut response = Response(body).into_response();
                    if mode == "link" && page < 2 {
                        let headers = response.headers_mut();
                        headers.append("link", "</api/link>; rel=first".parse().unwrap());
                        headers.append("link", format!(
                            r#"<?fixed=a%26b&page={}>; title="more, please; \"next\""; REL="alternate next""#,
                            page + 1
                        ).parse().unwrap());
                    }
                    response
                }
            }
        }),
    );
    let source = serve(router, "/api").await;
    let session = Session::new("admin");
    for (mode, pagination, query) in [
        ("link", "link: true", r#"fixed: "a&b""#),
        (
            "cursor",
            r#"cursor: {next: ["meta", "next"], query: "after"}"#,
            r#"fixed: "a&b", after: "initial""#,
        ),
        (
            "offset",
            r#"offset: {query: "skip", size: 2}"#,
            r#"fixed: "a&b", skip: "100""#,
        ),
    ] {
        let sdl = format!(
            r#"
            type Query {{
                all: [Item!]! @rest(source: "api", path: "/{mode}", query: {{{query}}},
                    result: ["data", "items"], paginate: {{{pagination}}})
                capped: [Item!]! @rest(source: "api", path: "/{mode}", query: {{{query}}},
                    result: ["data", "items"], paginate: {{{pagination}, max_pages: 2}})
                afterFilter: [Item!]! @rest(source: "api", path: "/{mode}", query: {{{query}}},
                    result: ["data", "items"], paginate: {{{pagination}}}, filter: true)
            }}
            type Item {{ id: Int! }}
        "#
        );
        let schema = schema_from(&sdl, source.clone(), "admin", None);
        for (field, count) in [("all", 5usize), ("capped", 4)] {
            let response = execute(
                &schema,
                &session,
                &format!("{{ {field} {{ id }} }}"),
                json!({}),
            )
            .await;
            let items: Vec<_> = (0..count).map(|id| json!({"id": id})).collect();
            assert_eq!(response, json!({"data": {field: items}}), "{mode} {field}");
            let requested: Vec<_> = calls.lock().unwrap().drain(..).collect();
            assert_eq!(
                requested,
                (0..count.div_ceil(2))
                    .map(|page| (mode.to_string(), page))
                    .collect::<Vec<_>>()
            );
        }
        let response = execute(
            &schema,
            &session,
            "{ afterFilter_aggregate(where: {id: {_gte: 2}}, order_by: {id: desc}, limit: 2) { aggregate { count } nodes { id } } }",
            json!({}),
        )
        .await;
        assert_eq!(
            response,
            json!({"data": {"afterFilter_aggregate": {"aggregate": {"count": 2}, "nodes": [{"id": 4}, {"id": 3}]}}}),
            "{mode}"
        );
        calls.lock().unwrap().clear();
    }

    for end in ["missing", "null"] {
        let sdl = format!(
            r#"
            type Query {{
                items: [Item!]! @rest(source: "api", path: "/cursor",
                    query: {{fixed: "a&b", after: "initial", end: "{end}"}},
                    result: ["data", "items"],
                    paginate: {{cursor: {{next: ["meta", "next"], query: "after"}}}})
            }}
            type Item {{ id: Int! }}
        "#
        );
        let schema = schema_from(&sdl, source.clone(), "admin", None);
        let response = execute(&schema, &session, "{ items { id } }", json!({})).await;
        assert_eq!(response["data"]["items"].as_array().unwrap().len(), 5);
        assert!(response.get("errors").is_none(), "{response}");
        assert_eq!(calls.lock().unwrap().drain(..).count(), 3);
    }
}

#[tokio::test]
async fn rest_link_pagination_rejects_out_of_scope_targets_and_failed_pages() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let router = Router::new()
        .route(
            "/api/{case}",
            get(|Path(case): Path<String>, headers: HeaderMap| async move {
                let host = headers["host"].to_str().unwrap();
                let (_, port) = host.split_once(':').unwrap();
                let target = match case.as_str() {
                    "host" => format!("http://localhost:{port}/api/target"),
                    "port" => "http://127.0.0.1:1/api/target".into(),
                    "scheme" => format!("https://{host}/api/target"),
                    "prefix" => "/api-outside".into(),
                    "traversal" => "/api/%2E%2E/api-outside".into(),
                    "encoded" => "/api/..%2Fapi-outside".into(),
                    "failure" => "/api/target".into(),
                    _ => panic!("{case}"),
                };
                (
                    [("link", format!("<{target}>; rel=next"))],
                    Response(json!([1])),
                )
            }),
        )
        .route(
            "/api/target",
            get({
                let calls = calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.lock().unwrap().push("target");
                        StatusCode::NOT_FOUND
                    }
                }
            }),
        )
        .route(
            "/api-outside",
            get({
                let calls = calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.lock().unwrap().push("outside");
                    }
                }
            }),
        );
    let source = serve(router, "/api").await;
    let session = Session::new("admin");
    let schema = schema_from(
        r#"
        type Query {
            items(case: String!): [Int!] @rest(source: "api", path: "/{{case}}",
                paginate: {link: true}, not_found: null)
        }
        "#,
        source,
        "admin",
        None,
    );
    for case in [
        "host",
        "port",
        "scheme",
        "prefix",
        "traversal",
        "encoded",
        "failure",
    ] {
        let response = execute(
            &schema,
            &session,
            "query($case: String!) { items(case: $case) }",
            json!({"case": case}),
        )
        .await;
        assert!(response["data"]["items"].is_null(), "{case}: {response}");
        let extensions = &response["errors"][0]["extensions"];
        assert_eq!(
            extensions["code"], "rest-source-error",
            "{case}: {response}"
        );
        if case == "failure" {
            assert_eq!(extensions["http"]["status"], 404, "{response}");
            assert_eq!(&*calls.lock().unwrap(), &["target"]);
        } else {
            assert_eq!(
                extensions["reason"], "unexpected-response",
                "{case}: {response}"
            );
            assert!(calls.lock().unwrap().is_empty(), "{case}");
        }
    }
}

#[tokio::test]
async fn rest_response_fields_read_result_and_typed_headers() {
    let router = Router::new().route(
        "/page",
        get(|| async {
            (
                [
                    ("x-total-count", "42"),
                    ("x-title", "invoice list"),
                    ("x-ratio", "1.25"),
                    ("x-more", "true"),
                    ("x-invalid", "forty two"),
                ],
                Response(json!({"data": {"items": [{"id": 1}, {"id": 2}]}})),
            )
        }),
    );
    let schema = schema_from(
        r#"
        scalar JSON
        type Query {
            page: Page! @rest(source: "api", path: "/page", result: ["data", "items"])
        }
        type Page {
            items: [Item!]! @body
            raw: JSON! @body
            total: Int @response_header(name: "X-Total-Count")
            title: String @response_header(name: "x-title")
            ratio: Float @response_header(name: "x-ratio")
            more: Boolean @response_header(name: "x-more")
            missing: String @response_header(name: "x-missing")
            invalid: Int @response_header(name: "x-invalid")
        }
        type Item { id: Int! }
        "#,
        serve(router, "").await,
        "admin",
        None,
    );
    let response = execute(
        &schema,
        &Session::new("admin"),
        "{ page { items { id } raw total title ratio more missing invalid } }",
        json!({}),
    )
    .await;
    assert_eq!(
        response["data"],
        json!({"page": {
            "items": [{"id": 1}, {"id": 2}],
            "raw": [{"id": 1}, {"id": 2}],
            "total": 42,
            "title": "invoice list",
            "ratio": 1.25,
            "more": true,
            "missing": null,
            "invalid": null
        }})
    );
    let errors = response["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{response}");
    assert_eq!(errors[0]["path"], json!(["page", "invalid"]));
    assert_eq!(errors[0]["extensions"]["code"], "rest-source-error");
    assert_eq!(errors[0]["extensions"]["reason"], "unexpected-response");
}

#[test]
fn rest_pagination_and_response_field_sdl_require_valid_placements_and_options() {
    for query in [
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {}) }"#,
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {link: false}) }"#,
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {link: true, offset: {query: "skip", size: 1}}) }"#,
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {link: true, max_pages: 0}) }"#,
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {link: true, max_pages: 1001}) }"#,
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {cursor: {query: "after", next: ["cursor", 1]}}) }"#,
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {offset: {query: "", size: 1}}) }"#,
        r#"type Query { items: [Int] @rest(source: "api", path: "/", paginate: {offset: {query: "skip", size: 0}}) }"#,
        r#"type Query { item: Int @rest(source: "api", path: "/", paginate: {link: true}) }"#,
        r#"type Query { item: Int @rest(source: "api", path: "/") } type Mutation { items: [Int] @rest(source: "api", path: "/", method: POST, paginate: {link: true}) }"#,
        r#"type Query { items: [Page] @rest(source: "api", path: "/") } type Page { raw: Int @body }"#,
        r#"type Query { item: Wrapper @rest(source: "api", path: "/") } type Wrapper { page: Page } type Page { header: String @response_header(name: "etag") }"#,
        r#"type Query { page: Page @rest(source: "api", path: "/") } type Page { header: [Int] @response_header(name: "etag") }"#,
        r#"type Query { page: Page @rest(source: "api", path: "/") } type Page { header: ID @response_header(name: "etag") }"#,
        r#"type Query { page: Page @rest(source: "api", path: "/") } type Page { header: String @response_header(name: "bad name") }"#,
        r#"type Query { page: Page @rest(source: "api", path: "/") } type Page { raw: Int @body @column(name: "other") }"#,
    ] {
        assert!(rusuna::Schema::from_sdl(query).is_err(), "{query}");
    }
}

#[tokio::test]
async fn rest_retry_after_honors_seconds_and_http_date_and_caps_waits() {
    let seconds = Arc::new(Mutex::new(Vec::new()));
    let dates = Arc::new(Mutex::new(Vec::new()));
    let exceeded = Arc::new(Mutex::new(0));
    let router = Router::new()
        .route(
            "/seconds",
            get({
                let seconds = seconds.clone();
                move || {
                    let seconds = seconds.clone();
                    async move {
                        let mut times = seconds.lock().unwrap();
                        times.push(Instant::now());
                        if times.len() == 1 {
                            (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")]).into_response()
                        } else {
                            Response(json!(1)).into_response()
                        }
                    }
                }
            }),
        )
        .route(
            "/date",
            get({
                let dates = dates.clone();
                move || {
                    let dates = dates.clone();
                    async move {
                        let mut times = dates.lock().unwrap();
                        let now = SystemTime::now();
                        if times.is_empty() {
                            let date = httpdate::fmt_http_date(now + Duration::from_secs(2));
                            times.push(httpdate::parse_http_date(&date).unwrap());
                            (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", date)])
                                .into_response()
                        } else {
                            times.push(now);
                            Response(json!(2)).into_response()
                        }
                    }
                }
            }),
        )
        .route(
            "/exceeded",
            get({
                let exceeded = exceeded.clone();
                move || {
                    let exceeded = exceeded.clone();
                    async move {
                        *exceeded.lock().unwrap() += 1;
                        (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "61")])
                    }
                }
            }),
        );
    let source = serve(router, "").await;
    let sdl = r#"
        type Query {
            seconds: Int @rest(source: "api", path: "/seconds")
            date: Int @rest(source: "api", path: "/date")
            exceeded: Int @rest(source: "api", path: "/exceeded")
        }
    "#;
    let session = Session::new("admin");
    let schema = schema_from(sdl, source.clone().retry(1, Duration::ZERO), "admin", None);
    let response = execute(&schema, &session, "{ seconds date exceeded }", json!({})).await;
    assert_eq!(
        response["data"],
        json!({"seconds": 1, "date": 2, "exceeded": null})
    );
    assert_eq!(
        response["errors"].as_array().unwrap().len(),
        1,
        "{response}"
    );
    assert_eq!(response["errors"][0]["extensions"]["http"]["status"], 429);
    {
        let seconds = seconds.lock().unwrap();
        assert_eq!(seconds.len(), 2);
        assert!(seconds[1].duration_since(seconds[0]) >= Duration::from_secs(1));
        let dates = dates.lock().unwrap();
        assert_eq!(dates.len(), 2);
        assert!(dates[1] >= dates[0]);
    }
    assert_eq!(*exceeded.lock().unwrap(), 1);

    let schema = schema_from(
        sdl,
        source
            .retry(1, Duration::ZERO)
            .max_retry_delay(Duration::ZERO),
        "admin",
        None,
    );
    seconds.lock().unwrap().clear();
    let response = execute(&schema, &session, "{ seconds }", json!({})).await;
    assert_eq!(response["data"], json!({"seconds": null}));
    assert_eq!(response["errors"][0]["extensions"]["http"]["status"], 429);
    assert_eq!(seconds.lock().unwrap().len(), 1);
}
