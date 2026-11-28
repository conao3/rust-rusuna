mod support;

use async_graphql::dynamic::Schema;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::{Json as AxumJson, Router};
use rusuna_async_graphql::{Builder, RestSource};
use rusuna_axum::{Endpoint, RestMethod};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{database, post, request, serve};
use tokio::net::TcpListener;

const POKEAPI: &str = include_str!("../../rusuna-axum/examples/pokeapi.graphql");
const API: &str = r#"
    scalar JSON
    enum Status { draft paid }
    input Options { flag: Boolean = true, order: Status, extra: [String!] }
    type Query {
      echo(status: Status = paid, input: Options, ids: [ID], omit: Int, q: String): JSON
        @rest(source: "api", method: GET, path: "/echo", query: {status: "{{status}}", input: "{{input}}", id: "{{ids}}", omit: "{{omit}}", q: "{{q}}"}, roles: ["user"])
      session(user_id: Int): JSON
        @rest(source: "api", path: "/scope/{{$user_id}}", query: {user: "{{user_id}}"}, roles: ["user"])
      encode(value: String): String
        @rest(source: "api", path: "/encode/{{value}}/tail", roles: ["user"])
      scalar: Int! @rest(source: "api", path: "/value", result: ["a.b", ""])
      nullable404: String @rest(source: "api", path: "/missing", not_found: null)
      handled404: String @rest(source: "api", path: "/missing")
      redirect: String @rest(source: "api", path: "/redirect")
      hiddenBody: String @rest(source: "api", path: "/failure")
      exposedBody: String @rest(source: "expose", path: "/failure")
      empty: String @rest(source: "api", path: "/empty")
      empty200: String @rest(source: "api", path: "/empty200")
      emptyBoolean: Boolean! @rest(source: "api", path: "/empty")
      emptyRequired: String! @rest(source: "api", path: "/empty")
      nonJson: String @rest(source: "api", path: "/non-json")
      badJson: String @rest(source: "api", path: "/bad-json")
      timeout: String @rest(source: "bounded", path: "/slow")
      tooLarge: String @rest(source: "bounded", path: "/big")
      disconnected: String @rest(source: "down", path: "/down")
      retried: Int @rest(source: "retry", path: "/retry")
      noRetry: Int @rest(source: "api", path: "/no-retry")
      typed: Typed @rest(source: "api", path: "/typed")
      typedList: [Typed]! @rest(source: "api", path: "/typed-list")
      requiredItems: [Typed!] @rest(source: "api", path: "/typed-list")
      scalarItems: [Int] @rest(source: "api", path: "/scalar-list")
      requiredScalarItems: [Int!] @rest(source: "api", path: "/scalar-list")
      nested: [[Int!]] @rest(source: "api", path: "/nested-list")
      missingResult: Int @rest(source: "api", path: "/value", result: ["absent"])
    }
    type Typed {
      id: ID!
      label: String!
      absent: String
      required: Int!
      integer: Int
      smallFloat: Float
      text: String
      date: DateTime
      stopped: Boolean
      status: Status
      metadata: JSON @column(name: "@meta")
    }
"#;

#[derive(Default)]
struct Counts {
    paths: BTreeMap<String, usize>,
    detail_in_flight: usize,
    max_detail_in_flight: usize,
    writes: Vec<(Method, String, Json)>,
}

type Observed = Arc<Mutex<Counts>>;

fn fixture(text: &str) -> Json {
    serde_json::from_str(text).unwrap()
}

async fn upstream(
    State(counts): State<Observed>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().strip_prefix("/api/v2").unwrap();
    if matches!(path, "/write" | "/write/fail") {
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        let body = serde_json::from_slice(&body).unwrap();
        if method == Method::POST {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        counts
            .lock()
            .unwrap()
            .writes
            .push((method, path.into(), body));
        return if path == "/write/fail" {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        } else {
            AxumJson(true).into_response()
        };
    }
    let is_detail = path.starts_with("/pokemon/") && path != "/pokemon/";
    let calls = {
        let mut counts = counts.lock().unwrap();
        let calls = counts.paths.entry(path.into()).or_default();
        *calls += 1;
        let calls = *calls;
        if is_detail {
            counts.detail_in_flight += 1;
            counts.max_detail_in_flight = counts.max_detail_in_flight.max(counts.detail_in_flight);
        }
        calls
    };
    let body = match path {
        "/pokemon/" => {
            assert_eq!(uri.query(), Some("limit=20&offset=0"));
            fixture(include_str!("fixtures/pokeapi/list.json"))
        }
        _ if is_detail => {
            tokio::time::sleep(Duration::from_millis(75)).await;
            counts.lock().unwrap().detail_in_flight -= 1;
            let mut pokemon = fixture(include_str!("fixtures/pokeapi/pokemon.json"));
            pokemon["name"] = json!(path.split('/').nth(2).unwrap());
            pokemon
        }
        "/pokemon-species/bulbasaur/" => fixture(include_str!("fixtures/pokeapi/species.json")),
        "/type/grass/" => fixture(include_str!("fixtures/pokeapi/grass.json")),
        "/type/poison/" => fixture(include_str!("fixtures/pokeapi/poison.json")),
        "/echo" => json!({"query": uri.query()}),
        _ if path.starts_with("/scope/") => {
            let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
            json!({"path": path, "query": uri.query(), "authorization": header("authorization"),
                "fixed": header("x-fixed"), "client": headers.get_all("x-client").iter().map(|v| v.to_str().unwrap()).collect::<Vec<_>>(),
                "other": header("x-other")})
        }
        _ if path.starts_with("/encode/") => json!(path),
        "/value" => {
            return (
                [(
                    header::CONTENT_TYPE,
                    "application/vnd.test+json; charset=utf-8",
                )],
                r#"{"a.b":{"":1.0}}"#,
            )
                .into_response();
        }
        "/missing" => return (StatusCode::NOT_FOUND, "missing").into_response(),
        "/redirect" => {
            return (
                StatusCode::FOUND,
                [(header::LOCATION, "/api/v2/redirected")],
            )
                .into_response();
        }
        "/failure" => return (StatusCode::IM_A_TEAPOT, "x".repeat(1200)).into_response(),
        "/non-json" => return r#""hello""#.into_response(),
        "/bad-json" => {
            return ([(header::CONTENT_TYPE, "application/json")], "malformed").into_response();
        }
        "/empty" => return StatusCode::NO_CONTENT.into_response(),
        "/empty200" => return StatusCode::OK.into_response(),
        "/slow" => {
            let mut response =
                Response::new(Body::from_stream(futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok::<_, std::convert::Infallible>("\"delayed\"")
                })));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
            return response;
        }
        "/big" => {
            return (
                [(header::CONTENT_TYPE, "application/json")],
                "x".repeat(128),
            )
                .into_response();
        }
        "/retry" if calls > 2 => json!(17),
        "/retry" | "/no-retry" => {
            return (StatusCode::TOO_MANY_REQUESTS, "try again").into_response();
        }
        "/typed" => {
            json!({"id": 18, "label": "kept", "required": null, "integer": "1", "smallFloat": 0.5, "text": 17,
            "date": "2026-11-28T00:00:00Z", "stopped": false, "status": "draft", "@meta": {"arbitrary": [null, true]}})
        }
        "/typed-list" => {
            json!([{"id": 1, "label": "good", "required": 2}, {"id": "B", "required": 4}, null])
        }
        "/scalar-list" => json!([1.0, "2", null, 3]),
        "/nested-list" => json!([[1, 2], ["bad"], null, [4]]),
        _ => return (StatusCode::NOT_FOUND, "unknown path").into_response(),
    };
    AxumJson(body).into_response()
}

async fn start(app: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

async fn server() -> (String, Observed) {
    let observed = Observed::default();
    let address = start(
        Router::new()
            .fallback(upstream)
            .with_state(observed.clone()),
    )
    .await;
    (format!("http://{address}/api/v2"), observed)
}

fn config(name: &str, url: &str) -> RestSource {
    RestSource::new(name, url).unwrap()
}

fn role_schema(document: &str, role: &str, sources: &[RestSource]) -> Schema {
    let schema = Arc::new(rusuna::Schema::from_sdl(document).unwrap());
    sources
        .iter()
        .cloned()
        .fold(Builder::new(schema, role), Builder::rest)
        .build()
        .unwrap()
        .schema()
        .finish()
        .unwrap()
}

async fn execute(schema: &Schema, query: &str, session: rusuna::Session) -> Json {
    let response = schema
        .execute(async_graphql::Request::new(query).data(session))
        .await;
    serde_json::to_value(response).unwrap()
}

fn extensions(response: &Json, path: Json) -> &Json {
    &response["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("{response}"))
        .iter()
        .find(|error| error["path"] == path)
        .unwrap_or_else(|| panic!("{path}: {response}"))["extensions"]
}

#[tokio::test]
async fn pokeapi_runs_without_a_pool_and_deduplicates_with_a_concurrency_limit() {
    let (base, observed) = server().await;
    let schema = role_schema(
        POKEAPI,
        "anonymous",
        &[config("pokeapi", &base).concurrency(2)],
    );
    let address = serve(HashMap::from([("anonymous".into(), schema)])).await;
    let headers = [("x-hasura-role", "anonymous".into())];
    let query = r#"{
      pokemons {
        name
        details { id baseExperience species { details { captureRate } } types { type { details { id name } } } }
        again: details { abilities { isHidden ability { name } } stats { baseStat } }
      }
      pokemon(name: "bulbasaur") { name height weight }
    }"#;
    let response = post(address, &headers, &json!({"query": query})).await;
    assert_eq!(response["errors"], Json::Null, "{response}");
    assert_eq!(
        response["data"]["pokemon"],
        json!({"name":"bulbasaur", "height":7, "weight":69})
    );
    let list = response["data"]["pokemons"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(
        list[0],
        json!({"name": "bulbasaur", "details": {"id": 1, "baseExperience": 64,
        "species": {"details": {"captureRate": 45}}, "types": [{"type":{"details":{"id":12,"name":"grass"}}}, {"type":{"details":{"id":4,"name":"poison"}}}]},
        "again": {"abilities": [{"isHidden":false, "ability":{"name":"overgrow"}}], "stats": [{"baseStat":45}]}})
    );
    {
        let counts = observed.lock().unwrap();
        for path in [
            "/pokemon/",
            "/pokemon/bulbasaur/",
            "/pokemon/ivysaur/",
            "/pokemon/venusaur/",
            "/pokemon-species/bulbasaur/",
            "/type/grass/",
            "/type/poison/",
        ] {
            assert_eq!(counts.paths.get(path), Some(&1), "{path}");
        }
        assert_eq!(counts.max_detail_in_flight, 2);
    }
    let response = post(
        address,
        &headers,
        &json!({"query": "{ pokemon(name: \"bulbasaur\") { name } }"}),
    )
    .await;
    assert_eq!(response["data"]["pokemon"]["name"], "bulbasaur");
    assert_eq!(observed.lock().unwrap().paths["/pokemon/bulbasaur/"], 2);
}

async fn api_sources(base: &str) -> Vec<RestSource> {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let down = format!("http://{}/api/v2", closed.local_addr().unwrap());
    drop(closed);
    vec![
        config("api", base),
        config("expose", base).expose_error_body(true),
        config("bounded", base)
            .timeout(Duration::from_millis(100))
            .max_body_bytes(32),
        config("down", &down),
        config("retry", base).retry(2, Duration::from_millis(1)),
    ]
}

#[tokio::test]
async fn rest_arguments_encode_enums_input_objects_defaults_and_repeated_keys() {
    let (base, _) = server().await;
    let schema = role_schema(API, "admin", &api_sources(&base).await);
    let response = execute(
        &schema,
        r#"{
      scalar
      defaults: echo
      echo(input: {order: draft, extra: "x"}, ids: ["a/b", null, 7], omit: null, q: "a?b#c&d\r\n")
      singleton: echo(ids: 5)
    }"#,
        rusuna::Session::new("admin"),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"scalar": 1,
            "defaults": {"query": "status=paid"},
            "echo": {"query": "status=paid&input=%7B%22flag%22%3Atrue%2C%22order%22%3A%22draft%22%2C%22extra%22%3A%5B%22x%22%5D%7D&id=a%2Fb&id=7&q=a%3Fb%23c%26d%0D%0A"},
            "singleton": {"query": "status=paid&id=5"}
        }})
    );
    let response = execute(&schema, "query($status: Status, $flag: Boolean, $declared: Status = null) { echo(status: $status, input: {flag: $flag}) nullStatus: echo(status: $declared) }", rusuna::Session::new("admin")).await;
    assert_eq!(
        response,
        json!({"data": {"echo": {"query": "status=paid&input=%7B%22flag%22%3Atrue%7D"}, "nullStatus": {"query": null}}})
    );
}

#[tokio::test]
async fn rest_path_values_stay_in_one_segment() {
    let (base, observed) = server().await;
    let schema = role_schema(API, "user", &api_sources(&base).await);
    for value in [json!("/"), json!("."), json!(".."), json!(""), Json::Null] {
        let query = format!("{{ encode(value: {value}) }}");
        let response = execute(&schema, &query, rusuna::Session::new("user")).await;
        assert_eq!(response["data"], json!({"encode": null}));
        assert_eq!(
            extensions(&response, json!(["encode"]))["code"],
            "validation-failed"
        );
    }
    assert!(observed.lock().unwrap().paths.is_empty());
    let response = execute(
        &schema,
        r#"{ encode(value: "?x=#\r\n%2F") }"#,
        rusuna::Session::new("user"),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"encode": "/encode/%3Fx%3D%23%0D%0A%252F/tail"}})
    );
}

#[tokio::test]
async fn session_markers_and_headers_use_the_session_and_header_precedence() {
    let (base, observed) = server().await;
    let mut sources = api_sources(&base).await;
    sources[0] = config("api", &base)
        .header("x-fixed", "fixed")
        .unwrap()
        .session_header("X-Fixed", "{{$unused}}")
        .unwrap()
        .session_header("Authorization", "Bearer {{$user_id}}")
        .unwrap()
        .forward_header("x-fixed")
        .unwrap()
        .forward_header("authorization")
        .unwrap()
        .forward_header("x-client")
        .unwrap();
    let role_schema = role_schema(API, "user", &sources);
    let address = serve(HashMap::from([("user".into(), role_schema.clone())])).await;
    let headers = [
        ("x-hasura-role", "user".into()),
        ("x-hasura-user-id", "42".into()),
        ("authorization", "forwarded".into()),
        ("x-fixed", "spoofed".into()),
        ("x-client", "one".into()),
        ("x-client", "two".into()),
        ("x-other", "unlisted".into()),
    ];
    let response = post(
        address,
        &headers,
        &json!({"query": "{ session(user_id: 7) }"}),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"session": {"path":"/scope/42", "query":"user=7",
        "authorization":"Bearer 42", "fixed":"fixed", "client":["one", "two"], "other":null}}})
    );
    for session in [
        rusuna::Session::new("user"),
        rusuna::Session::new("user")
            .variable("user_id", rusuna::Value::Text("first\nsecond".into())),
        rusuna::Session::new("user")
            .variable("user_id", rusuna::Value::Text("first\rsecond".into())),
    ] {
        let response = execute(&role_schema, "{ session(user_id: 7) }", session).await;
        assert_eq!(
            extensions(&response, json!(["session"]))["code"],
            "validation-failed"
        );
    }
    let wrong_role = execute(&role_schema, "{ session }", rusuna::Session::new("admin")).await;
    assert_eq!(
        extensions(&wrong_role, json!(["session"]))["code"],
        "access-denied"
    );
    assert_eq!(observed.lock().unwrap().paths.values().sum::<usize>(), 1);
    for text in ["one\rtwo", "one\ntwo"] {
        assert!(config("api", &base).header("x-fixed", text).is_err());
        assert!(
            config("api", &base)
                .session_header("x-fixed", text)
                .is_err()
        );
    }
    assert!(
        config("api", &base)
            .session_header("x-fixed", "{{user_id}}")
            .is_err()
    );
}

#[tokio::test]
async fn rest_reports_status_transport_body_and_retry_outcomes() {
    let (base, observed) = server().await;
    let schema = role_schema(API, "admin", &api_sources(&base).await);
    let response = execute(&schema, "{ nullable404 handled404 redirect hiddenBody exposedBody empty empty200 emptyBoolean nonJson badJson timeout tooLarge disconnected retried noRetry missingResult }", rusuna::Session::new("admin")).await;
    assert_eq!(
        response["data"],
        json!({"nullable404":null, "handled404":null, "redirect":null, "hiddenBody":null,
        "exposedBody":null, "empty":null, "empty200":null, "emptyBoolean":true, "nonJson":null, "badJson":null,
        "timeout":null, "tooLarge":null, "disconnected":null, "retried":17, "noRetry":null, "missingResult":null})
    );
    let errors = response["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 11, "{response}");
    for error in errors {
        assert_eq!(error["extensions"]["code"], "rest-source-error");
    }
    for (field, status) in [
        ("handled404", 404),
        ("redirect", 302),
        ("hiddenBody", 418),
        ("noRetry", 429),
    ] {
        assert_eq!(
            extensions(&response, json!([field]))["http"],
            json!({"status": status})
        );
    }
    assert_eq!(
        extensions(&response, json!(["exposedBody"]))["http"],
        json!({"status":418, "body":"x".repeat(1024)})
    );
    for (field, reason) in [
        ("nonJson", "unexpected-response"),
        ("badJson", "unexpected-response"),
        ("missingResult", "unexpected-response"),
        ("timeout", "timeout"),
        ("tooLarge", "too-large"),
        ("disconnected", "connection"),
    ] {
        assert_eq!(extensions(&response, json!([field]))["reason"], reason);
    }
    let response = execute(
        &schema,
        "{ emptyRequired scalar }",
        rusuna::Session::new("admin"),
    )
    .await;
    assert_eq!(response["data"], Json::Null, "{response}");
    assert_eq!(
        extensions(&response, json!(["emptyRequired"]))["reason"],
        "unexpected-response"
    );
    let counts = observed.lock().unwrap();
    assert_eq!(counts.paths["/missing"], 1);
    assert_eq!(counts.paths["/failure"], 1);
    assert_eq!(counts.paths["/retry"], 3);
    assert_eq!(counts.paths["/no-retry"], 1);
    assert!(!counts.paths.contains_key("/redirected"));
}

#[tokio::test]
async fn rest_response_type_errors_follow_graphql_paths_and_nullability() {
    let (base, _) = server().await;
    let schema = role_schema(API, "admin", &api_sources(&base).await);
    let response = execute(
        &schema,
        r#"{
        typed { id label date stopped status metadata absent badInt: integer text smallFloat }
        bubbled: typed { required label }
        typedList { id alias: label required }
        requiredItems { id label }
        scalarItems requiredScalarItems nested
    }"#,
        rusuna::Session::new("admin"),
    )
    .await;
    assert_eq!(
        response["data"],
        json!({"typed": {"id":"18", "label":"kept", "date":"2026-11-28T00:00:00Z", "stopped":false,
        "status":"draft", "metadata":{"arbitrary":[null,true]}, "absent":null, "badInt":null, "text":null, "smallFloat":0.5},
        "bubbled":null, "typedList":[{"id":"1", "alias":"good", "required":2},null,null], "requiredItems":null,
        "scalarItems":[1,null,null,3], "requiredScalarItems":null, "nested":[[1,2],null,null,[4]]}),
        "{response}"
    );
    for path in [
        json!(["typed", "badInt"]),
        json!(["typed", "text"]),
        json!(["bubbled", "required"]),
        json!(["typedList", 1, "alias"]),
        json!(["requiredItems", 2]),
        json!(["scalarItems", 1]),
        json!(["requiredScalarItems", 1]),
        json!(["nested", 1, 0]),
    ] {
        assert_eq!(
            extensions(&response, path),
            &json!({"code":"rest-source-error", "reason":"unexpected-response"})
        );
    }
    assert_eq!(
        response["errors"].as_array().unwrap().len(),
        8,
        "{response}"
    );
    let response = execute(
        &schema,
        "{ requiredItems { id label } }",
        rusuna::Session::new("admin"),
    )
    .await;
    assert_eq!(response["data"], json!({"requiredItems":null}));
}

#[tokio::test]
async fn rest_endpoints_run_rest_sources_and_map_errors_to_bad_gateway() {
    let (base, observed) = server().await;
    let schema = role_schema(API, "admin", &api_sources(&base).await);
    let app = rusuna_axum::rest(
        HashMap::from([("admin".into(), schema)]),
        [
            Endpoint::new("value", "/value", [RestMethod::Get], "{ scalar }"),
            Endpoint::new("upstream", "/upstream", [RestMethod::Get], "{ handled404 }"),
        ]
        .to_vec(),
        |_| async { Ok(rusuna::Session::new("admin")) },
    )
    .await
    .unwrap();
    assert!(observed.lock().unwrap().paths.is_empty());
    let address = start(app).await;
    let (status, body) = request(address, "GET", "/value", &[], None).await;
    assert_eq!((status, body), (200, json!({"scalar":1})));
    let (status, body) = request(address, "GET", "/upstream", &[], None).await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(
        extensions(&body, json!(["handled404"]))["http"]["status"],
        404
    );
}

#[tokio::test]
async fn the_role_schema_excludes_hidden_rests_and_subscriptions() {
    let (base, observed) = server().await;
    let schema = role_schema(API, "user", &api_sources(&base).await);
    let response = execute(&schema, "{ hiddenBody }", rusuna::Session::new("user")).await;
    assert_eq!(response["data"], Json::Null);
    assert!(
        response["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("hiddenBody")
    );
    let response = execute(
        &schema,
        "{ __schema { subscriptionType { name } } typed: __type(name: \"Typed\") { name } }",
        rusuna::Session::new("user"),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"__schema":{"subscriptionType":null},"typed":null}})
    );
    assert!(observed.lock().unwrap().paths.is_empty());
    let schema = Arc::new(rusuna::Schema::from_sdl(API).unwrap());
    assert!(matches!(
        Builder::new(schema, "user").build(),
        Err(rusuna_async_graphql::Error::Rest { .. })
    ));
}

#[tokio::test]
async fn rest_mutations_send_json_in_document_order_without_deduplication() {
    let (base, observed) = server().await;
    let schema = role_schema(
        r#"
        schema { query: Query mutation: Mutation }
        type Query { ready: Boolean @rest(source: "api", path: "/ready") }
        input Payload { count: Int!, enabled: Boolean = true }
        type Mutation {
          create(input: Payload!, count: Int!, note: String): Boolean!
            @rest(source: "api", method: POST, path: "/write", body: {
              count: "{{count}}", payload: "{{input}}", note: "{{note}}",
              actor: "{{$actor}}", unknown: "{{$unknown}}",
              list: ["{{count}}", {actor: "{{$actor}}"}, "prefix {{note}}", "{{count}}/{{note}}", "{{ not a marker }}"]
            })
          put(input: Payload!, note: String, count: Int = 3, omitted: Boolean): Boolean!
            @rest(source: "api", method: PUT, path: "/write")
          patch: Boolean! @rest(source: "api", method: PATCH, path: "/write", body: {})
          delete: Boolean! @rest(source: "api", method: DELETE, path: "/write")
        }
        "#,
        "admin",
        &[config("api", &base)],
    );
    let response = execute(
        &schema,
        r#"mutation {
          first: create(input: {count: 2}, count: 7, note: null)
          second: put(input: {count: 4, enabled: false}, note: null)
          third: patch
          fourth: delete
          fifth: delete
        }"#,
        rusuna::Session::new("admin")
            .variable("actor", rusuna::Value::Int(42))
            .variable("unknown", rusuna::Value::Null),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"first": true, "second": true, "third": true, "fourth": true, "fifth": true}})
    );
    assert_eq!(
        observed.lock().unwrap().writes,
        [
            (
                Method::POST,
                "/write".into(),
                json!({
                    "count": 7, "payload": {"count": 2, "enabled": true}, "note": null,
                    "actor": 42, "unknown": null, "list": [7, {"actor": 42}, "prefix {{note}}", "{{count}}/{{note}}", "{{ not a marker }}"]
                })
            ),
            (
                Method::PUT,
                "/write".into(),
                json!({"input": {"count": 4, "enabled": false}, "note": null, "count": 3})
            ),
            (Method::PATCH, "/write".into(), json!({})),
            (Method::DELETE, "/write".into(), json!({})),
            (Method::DELETE, "/write".into(), json!({})),
        ]
    );
}

#[tokio::test]
async fn a_failed_rest_mutation_rolls_back_postgres_and_skips_later_fields() {
    let (base, observed) = server().await;
    let pool = database("http_rest_mutation", &[]).await;
    sqlx::raw_sql("CREATE TABLE item (id integer PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    let schema = Arc::new(
        rusuna::Schema::from_sdl(
            r#"
            type Query { items: [item!]! @list }
            type item @table(name: "item") { id: Int! }
            type Mutation {
              send: Boolean @rest(source: "api", method: POST, path: "/write")
              fail: Boolean @rest(source: "api", method: POST, path: "/write/fail")
            }
            "#,
        )
        .unwrap(),
    );
    let schema = Builder::new(schema, "admin")
        .rest(config("api", &base).retry(2, Duration::ZERO))
        .build()
        .unwrap()
        .schema()
        .data(pool.clone())
        .finish()
        .unwrap();
    let response = execute(
        &schema,
        r#"mutation {
          insert_item(objects: [{id: 1}]) { affected_rows }
          send
          fail
          later: send
        }"#,
        rusuna::Session::new("admin"),
    )
    .await;
    assert_eq!(response["data"], Json::Null);
    assert_eq!(
        extensions(&response, json!(["fail"])),
        &json!({"code": "rest-source-error", "http": {"status": 503}})
    );
    assert_eq!(
        observed.lock().unwrap().writes,
        [
            (Method::POST, "/write".into(), json!({})),
            (Method::POST, "/write/fail".into(), json!({})),
        ]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM item")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let response = execute(
        &schema,
        "mutation { insert_item(objects: [{id: 1}]) { affected_rows } send }",
        rusuna::Session::new("admin"),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"insert_item": {"affected_rows": 1}, "send": true}})
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM item")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
