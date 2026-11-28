mod support;

use async_graphql::Value as GraphQLValue;
use async_graphql::dynamic::{
    Enum, Field, FieldFuture, FieldValue, InputObject, InputValue, Object, ResolverContext, Scalar,
    Schema, TypeRef,
};
use axum::http::HeaderMap;
use rusuna_async_graphql::{Builder, RemoteSchema, RestSource};
use serde_json::{Value as Json, json};
use sqlx::Row;
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use support::*;
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct Weather {
    celsius: Arc<Mutex<BTreeMap<String, i64>>>,
    documents: Arc<Mutex<Vec<String>>>,
}

fn parent(ctx: &ResolverContext<'_>) -> Json {
    ctx.parent_value.try_downcast_ref::<Json>().unwrap().clone()
}

fn city(weather: &Weather, name: &str, unit: &str) -> Option<Json> {
    let celsius = *weather.celsius.lock().unwrap().get(name)?;
    Some(json!({"city": name, "celsius": celsius, "unit": unit}))
}

fn weather_schema(weather: &Weather) -> Schema {
    let unit = Enum::new("Unit")
        .description("A temperature unit")
        .item("CELSIUS")
        .item("FAHRENHEIT");
    let station = Object::new("Station")
        .field(Field::new("id", TypeRef::named_nn(TypeRef::ID), |ctx| {
            FieldFuture::new(async move {
                Ok(Some(GraphQLValue::from(format!(
                    "st-{}",
                    parent(&ctx)["city"].as_str().unwrap()
                ))))
            })
        }))
        .field(Field::new(
            "name",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    Ok(Some(GraphQLValue::from(format!(
                        "{} central",
                        parent(&ctx)["city"].as_str().unwrap()
                    ))))
                })
            },
        ));
    let observation = Object::new("Weather")
        .description("The weather of a city")
        .field(Field::new(
            "city",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    Ok(Some(GraphQLValue::from(
                        parent(&ctx)["city"].as_str().unwrap().to_string(),
                    )))
                })
            },
        ))
        .field(
            Field::new("temperature", TypeRef::named_nn(TypeRef::INT), |ctx| {
                FieldFuture::new(async move {
                    let row = parent(&ctx);
                    let unit = match ctx.args.get("unit") {
                        Some(unit) => unit.enum_name()?.to_string(),
                        None => row["unit"].as_str().unwrap().to_string(),
                    };
                    let celsius = row["celsius"].as_i64().unwrap();
                    let value = if unit == "FAHRENHEIT" {
                        celsius * 9 / 5 + 32
                    } else {
                        celsius
                    };
                    Ok(Some(GraphQLValue::from(value)))
                })
            })
            .argument(
                InputValue::new("unit", TypeRef::named("Unit"))
                    .default_value(GraphQLValue::Enum(async_graphql::Name::new("CELSIUS"))),
            ),
        )
        .field(Field::new("unit", TypeRef::named_nn("Unit"), |ctx| {
            FieldFuture::new(async move {
                Ok(Some(GraphQLValue::Enum(async_graphql::Name::new(
                    parent(&ctx)["unit"].as_str().unwrap(),
                ))))
            })
        }))
        .field(Field::new("station", TypeRef::named("Station"), |ctx| {
            FieldFuture::new(async move { Ok(Some(FieldValue::owned_any(parent(&ctx)))) })
        }));
    let forecast = InputObject::new("Forecast")
        .field(InputValue::new(
            "cities",
            TypeRef::named_nn_list_nn(TypeRef::STRING),
        ))
        .field(
            InputValue::new("unit", TypeRef::named_nn("Unit"))
                .default_value(GraphQLValue::Enum(async_graphql::Name::new("CELSIUS"))),
        );
    let query = {
        let (one, many) = (weather.clone(), weather.clone());
        Object::new("Query")
            .field(
                Field::new("weather", TypeRef::named("Weather"), move |ctx| {
                    let weather = one.clone();
                    FieldFuture::new(async move {
                        let name = ctx.args.try_get("city")?.string()?.to_string();
                        Ok(city(&weather, &name, "CELSIUS").map(FieldValue::owned_any))
                    })
                })
                .argument(InputValue::new("city", TypeRef::named_nn(TypeRef::STRING))),
            )
            .field(
                Field::new(
                    "forecast",
                    TypeRef::named_nn_list_nn("Weather"),
                    move |ctx| {
                        let weather = many.clone();
                        FieldFuture::new(async move {
                            let filter = ctx.args.try_get("filter")?.object()?;
                            let unit = match filter.get("unit") {
                                Some(unit) => unit.enum_name()?.to_string(),
                                None => "CELSIUS".to_string(),
                            };
                            let mut rows = Vec::new();
                            for name in filter.try_get("cities")?.list()?.iter() {
                                if let Some(row) = city(&weather, name.string()?, &unit) {
                                    rows.push(FieldValue::owned_any(row));
                                }
                            }
                            Ok(Some(FieldValue::list(rows)))
                        })
                    },
                )
                .argument(InputValue::new("filter", TypeRef::named_nn("Forecast"))),
            )
            .field(
                Field::new("header", TypeRef::named(TypeRef::STRING), |ctx| {
                    FieldFuture::new(async move {
                        let name = ctx.args.try_get("name")?.string()?.to_string();
                        let headers = ctx.data::<HeaderMap>()?;
                        Ok(headers
                            .get(&name)
                            .map(|v| GraphQLValue::from(v.to_str().unwrap().to_string())))
                    })
                })
                .argument(InputValue::new("name", TypeRef::named_nn(TypeRef::STRING))),
            )
            .field(Field::new("defaultUnit", TypeRef::named_nn("Unit"), |_| {
                FieldFuture::new(async {
                    Ok(Some(GraphQLValue::Enum(async_graphql::Name::new(
                        "CELSIUS",
                    ))))
                })
            }))
            .field(Field::new("now", TypeRef::named_nn("Instant"), |_| {
                FieldFuture::new(async { Ok(Some(GraphQLValue::from("2026-01-01T00:00:00Z"))) })
            }))
            .field(Field::new(
                "failing",
                TypeRef::named(TypeRef::STRING),
                |_| {
                    FieldFuture::new(async {
                        Err::<Option<GraphQLValue>, _>(async_graphql::Error::new(
                            "the station is offline",
                        ))
                    })
                },
            ))
    };
    let mutation = {
        let weather = weather.clone();
        Object::new("Mutation").field(
            Field::new("setTemperature", TypeRef::named("Weather"), move |ctx| {
                let weather = weather.clone();
                FieldFuture::new(async move {
                    let name = ctx.args.try_get("city")?.string()?.to_string();
                    let celsius = ctx.args.try_get("celsius")?.i64()?;
                    let mut cities = weather.celsius.lock().unwrap();
                    let Some(value) = cities.get_mut(&name) else {
                        return Err(async_graphql::Error::new(format!("unknown city {name}")));
                    };
                    *value = celsius;
                    drop(cities);
                    Ok(city(&weather, &name, "CELSIUS").map(FieldValue::owned_any))
                })
            })
            .argument(InputValue::new("city", TypeRef::named_nn(TypeRef::STRING)))
            .argument(InputValue::new("celsius", TypeRef::named_nn(TypeRef::INT))),
        )
    };
    Schema::build("Query", Some("Mutation"), None)
        .register(unit)
        .register(station)
        .register(observation)
        .register(forecast)
        .register(Scalar::new("Instant"))
        .register(query)
        .register(mutation)
        .finish()
        .unwrap()
}

async fn serve_weather() -> (SocketAddr, Weather, Schema) {
    let weather = Weather::default();
    weather
        .celsius
        .lock()
        .unwrap()
        .extend([("Tokyo".to_string(), 20), ("Oslo".to_string(), -5)]);
    let schema = weather_schema(&weather);
    let address = listen(recorded_remote(schema.clone(), weather.documents.clone())).await;
    (address, weather, schema)
}

fn recorded_remote(schema: Schema, documents: Arc<Mutex<Vec<String>>>) -> axum::Router {
    axum::Router::new().route(
        "/graphql",
        axum::routing::post({
            move |headers: HeaderMap, request: async_graphql_axum::GraphQLRequest| async move {
                let request = request.into_inner();
                documents.lock().unwrap().push(request.query.clone());
                async_graphql_axum::GraphQLResponse::from(
                    schema.execute(request.data(headers)).await,
                )
            }
        }),
    )
}

async fn listen(app: axum::Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn role_schemas(
    pool: &sqlx::PgPool,
    remotes: &[RemoteSchema],
) -> Result<HashMap<String, Schema>, rusuna_async_graphql::Error> {
    let schema = Arc::new(rusuna::Schema::from_sdl(&fixture("permissions.graphql")).unwrap());
    schema
        .roles()
        .into_iter()
        .map(|role| {
            let mut builder = rusuna_async_graphql::Builder::new(schema.clone(), &role);
            for remote in remotes {
                builder = builder.remote(remote.clone());
            }
            let built = builder
                .build()?
                .schema()
                .data(pool.clone())
                .finish()
                .unwrap();
            Ok((role, built))
        })
        .collect()
}

async fn serve_rusuna(schemas: HashMap<String, Schema>) -> SocketAddr {
    let app = axum::Router::new().route(
        "/graphql",
        rusuna_axum::graphql(schemas, |parts: &axum::http::request::Parts| {
            let session = session(parts);
            async move { Ok(session) }
        }),
    );
    listen(app).await
}

fn remote(address: SocketAddr, weather: &Schema) -> RemoteSchema {
    RemoteSchema::new("weather", &format!("http://{address}/graphql"))
        .unwrap()
        .sdl(weather.sdl())
        .roles(["admin", "user"])
}

async fn direct(weather: &Schema, query: &str) -> Json {
    let response = weather
        .execute(async_graphql::Request::new(query).data(HeaderMap::new()))
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()
}

async fn title(pool: &sqlx::PgPool) -> String {
    sqlx::query("SELECT title FROM article WHERE id = 1")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_root_fields_are_served_beside_rusuna_root_fields() {
    let pool = database("http_remote_mixed", &["schema.sql", "seed.sql"]).await;
    let (address, weather, schema) = serve_weather().await;
    let rusuna = serve_rusuna(role_schemas(&pool, &[remote(address, &schema)]).unwrap()).await;

    let remote_part = "weather(city: \"Tokyo\") { city temperature f: temperature(unit: FAHRENHEIT) unit station { id name } } now defaultUnit";
    let response = post(
        rusuna,
        &[],
        &json!({
            "query": "query Q($city: String!, $unit: Unit) { article(order_by: {id: asc}, limit: 2) { id } here: weather(city: $city) { ...W __typename } now } fragment W on Weather { city temperature(unit: $unit) station { name } }",
            "variables": {"city": "Oslo", "unit": "FAHRENHEIT"},
        }),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {
            "article": [{"id": 1}, {"id": 2}],
            "here": {"city": "Oslo", "temperature": 23, "station": {"name": "Oslo central"}, "__typename": "Weather"},
            "now": "2026-01-01T00:00:00Z",
        }})
    );
    let response = post(
        rusuna,
        &[],
        &json!({ "query": format!("{{ {remote_part} }}") }),
    )
    .await;
    assert_eq!(
        response["data"],
        direct(&schema, &format!("{{ {remote_part} }}")).await
    );
    let omitted = post(
        rusuna,
        &[],
        &json!({"query": "query($unit: Unit) { weather(city: \"Oslo\") { temperature(unit: $unit) unit } }"}),
    )
    .await;
    assert_eq!(
        omitted["data"],
        direct(&schema, "{ weather(city: \"Oslo\") { temperature unit } }").await,
        "{omitted}"
    );
    assert_eq!(omitted["data"]["weather"]["temperature"], -5);
    let user = post(
        rusuna,
        &role_headers("user", &json!({"user_id": 1})),
        &json!({"query": "{ article(order_by: {id: asc}) { id } weather(city: \"Tokyo\") { temperature } }"}),
    )
    .await;
    assert_eq!(
        user,
        json!({"data": {"article": [{"id": 1}, {"id": 7}, {"id": 12}], "weather": {"temperature": 20}}})
    );

    let response = post(
        rusuna,
        &[],
        &json!({"query": "mutation { update_article(where: {id: {_eq: 1}}, _set: {title: \"changed\"}) { affected_rows } setTemperature(city: \"Tokyo\", celsius: 30) { temperature } }"}),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"update_article": {"affected_rows": 1}, "setTemperature": {"temperature": 30}}})
    );
    assert_eq!(title(&pool).await, "changed");
    assert_eq!(weather.celsius.lock().unwrap()["Tokyo"], 30);

    let response = post(
        rusuna,
        &[],
        &json!({"query": "mutation { update_article(where: {id: {_eq: 1}}, _set: {title: \"rolled back\"}) { affected_rows } setTemperature(city: \"Atlantis\", celsius: 1) { temperature } later: setTemperature(city: \"Oslo\", celsius: 9) { temperature } }"}),
    )
    .await;
    assert_eq!(response["data"], Json::Null, "{response}");
    assert_eq!(response["errors"][0]["message"], "unknown city Atlantis");
    assert_eq!(
        response["errors"][0]["extensions"]["code"],
        "remote-schema-error"
    );
    assert_eq!(title(&pool).await, "changed");
    assert_eq!(weather.celsius.lock().unwrap()["Oslo"], -5);

    let response = post(
        rusuna,
        &[],
        &json!({"query": "mutation { t: setTemperature(city: \"Oslo\", celsius: 1) { city } t: setTemperature(city: \"Oslo\", celsius: 1) { city } }"}),
    )
    .await;
    assert_eq!(
        response["errors"][0]["extensions"]["code"], "validation-failed",
        "{response}"
    );
    let forwarded = weather.documents.lock().unwrap().clone();
    assert!(
        forwarded.iter().all(|d| !d.contains("article")),
        "{forwarded:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_types_appear_in_introspection_for_the_roles_that_see_them() {
    let pool = database("http_remote_introspection", &["schema.sql", "seed.sql"]).await;
    let (address, _, schema) = serve_weather().await;
    let rusuna = serve_rusuna(role_schemas(&pool, &[remote(address, &schema)]).unwrap()).await;
    let query = "{ __type(name: \"Weather\") { description fields { name args { name defaultValue } } } unit: __type(name: \"Unit\") { enumValues { name } } forecast: __type(name: \"Forecast\") { inputFields { name defaultValue } } }";
    let expected = json!({
        "__type": {
            "description": "The weather of a city",
            "fields": [
                {"name": "city", "args": []},
                {"name": "temperature", "args": [{"name": "unit", "defaultValue": "CELSIUS"}]},
                {"name": "unit", "args": []},
                {"name": "station", "args": []},
            ],
        },
        "unit": {"enumValues": [{"name": "CELSIUS"}, {"name": "FAHRENHEIT"}]},
        "forecast": {"inputFields": [{"name": "cities", "defaultValue": null}, {"name": "unit", "defaultValue": "CELSIUS"}]},
    });
    for role in ["admin", "user"] {
        let response = post(
            rusuna,
            &role_headers(role, &json!({"user_id": 1})),
            &json!({ "query": query }),
        )
        .await;
        assert_eq!(response, json!({ "data": expected }), "{role}");
    }
    let reviewer = post(
        rusuna,
        &role_headers("reviewer", &json!({})),
        &json!({ "query": "{ __type(name: \"Weather\") { name } }" }),
    )
    .await;
    assert_eq!(reviewer, json!({"data": {"__type": null}}));
    let hidden = post(
        rusuna,
        &role_headers("reviewer", &json!({})),
        &json!({ "query": "{ weather(city: \"Tokyo\") { city } }" }),
    )
    .await;
    assert_eq!(hidden["data"], Json::Null, "{hidden}");
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_schemas_forward_headers_and_report_errors() {
    let pool = database("http_remote_errors", &["schema.sql", "seed.sql"]).await;
    let (address, _, schema) = serve_weather().await;
    let headed = remote(address, &schema)
        .header("x-api-key", "secret")
        .unwrap()
        .forward_header("x-request-id")
        .unwrap();
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let down_address = closed.local_addr().unwrap();
    drop(closed);
    let down = RemoteSchema::new("down", &format!("http://{down_address}/graphql"))
        .unwrap()
        .sdl("type Query { offline: String }");
    let schemas = role_schemas(&pool, &[headed, down]).unwrap();
    let rusuna = serve_rusuna(schemas.clone()).await;
    let rest = rusuna_axum::rest(
        schemas,
        vec![rusuna_axum::Endpoint::new(
            "offline",
            "/offline",
            [rusuna_axum::RestMethod::Get],
            "{ offline }",
        )],
        |parts: &axum::http::request::Parts| {
            let session = session(parts);
            async move { Ok(session) }
        },
    )
    .await
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rest_address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, rest).await.unwrap() });
    let (status, body) = request(rest_address, "GET", "/offline", &[], None).await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(
        body["errors"][0]["extensions"]["code"],
        "remote-schema-error"
    );

    let response = post(
        rusuna,
        &[
            ("x-request-id", "request-7".into()),
            ("x-other", "kept".into()),
        ],
        &json!({"query": "{ key: header(name: \"x-api-key\") id: header(name: \"x-request-id\") other: header(name: \"x-other\") }"}),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {"key": "secret", "id": "request-7", "other": null}})
    );

    let response = post(
        rusuna,
        &[],
        &json!({"query": "{ article(limit: 1) { id } failing }"}),
    )
    .await;
    assert_eq!(response["data"]["article"], json!([{"id": 1}]));
    assert_eq!(response["data"]["failing"], Json::Null);
    let error = &response["errors"][0];
    assert_eq!(error["message"], "the station is offline");
    assert_eq!(error["extensions"]["code"], "remote-schema-error");
    assert_eq!(
        error["extensions"]["remote_errors"][0]["message"],
        "the station is offline"
    );

    let response = post(rusuna, &[], &json!({"query": "{ offline }"})).await;
    assert_eq!(response["data"]["offline"], Json::Null);
    let message = response["errors"][0]["message"].as_str().unwrap();
    assert!(
        message.starts_with("the remote schema down is unreachable"),
        "{message}"
    );
    assert_eq!(
        response["errors"][0]["extensions"]["code"],
        "remote-schema-error"
    );
    let introspection = RemoteSchema::new("down", &format!("http://{down_address}/graphql"))
        .unwrap()
        .introspect()
        .await
        .unwrap_err();
    assert!(
        introspection.to_string().contains("failed"),
        "{introspection}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_names_collide_or_take_prefixes() {
    let pool = database("http_remote_names", &["schema.sql", "seed.sql"]).await;
    let (address, weather, schema) = serve_weather().await;
    let first = remote(address, &schema);
    let collision = |remotes: &[RemoteSchema]| match role_schemas(&pool, remotes) {
        Err(rusuna_async_graphql::Error::Remote { message, .. }) => message,
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("the schemas were built"),
    };
    assert!(collision(&[first.clone(), first.clone()]).contains("the type"));
    assert!(collision(&[first.clone(), first.clone().type_prefix("w_")]).contains("root field"));
    let article = RemoteSchema::new("articles", "http://127.0.0.1:1/graphql")
        .unwrap()
        .sdl("type Query { article: String }");
    assert!(collision(std::slice::from_ref(&article)).contains("root field article"));
    let unit = RemoteSchema::new("units", "http://127.0.0.1:1/graphql")
        .unwrap()
        .sdl("enum order_by { id } type Query { units: order_by }");
    assert!(collision(&[unit]).contains("type order_by"));
    for sdl in [
        "interface Node { id: ID! } type Query { node: Node }",
        "type A { id: ID! } type B { id: ID! } union U = A | B type Query { u: U }",
        "type Query { missing: Missing }",
        "type Query { a: Int",
    ] {
        let invalid = RemoteSchema::new("invalid", "http://127.0.0.1:1/graphql")
            .unwrap()
            .sdl(sdl);
        collision(&[invalid]);
    }
    assert!(RemoteSchema::new("bad", "ftp://example.com/").is_err());
    assert!(
        role_schemas(
            &pool,
            &[RemoteSchema::new("no sdl", "http://127.0.0.1:1/graphql").unwrap()]
        )
        .is_err()
    );

    let second = first.clone().type_prefix("w_").root_field_prefix("w_");
    let prefixed = article.root_field_prefix("remote_");
    let rusuna = serve_rusuna(role_schemas(&pool, &[first, second, prefixed]).unwrap()).await;
    weather.documents.lock().unwrap().clear();
    let response = post(
        rusuna,
        &[],
        &json!({
            "query": "query($filter: w_Forecast!) { w_forecast(filter: $filter) { city temperature unit station { name } __typename } forecast(filter: {cities: [\"Tokyo\"]}) { temperature } u: __type(name: \"w_Unit\") { name } }",
            "variables": {"filter": {"cities": ["Oslo", "Tokyo", "Nowhere"], "unit": "FAHRENHEIT"}},
        }),
    )
    .await;
    assert_eq!(
        response,
        json!({"data": {
            "w_forecast": [
                {"city": "Oslo", "temperature": -5, "unit": "FAHRENHEIT", "station": {"name": "Oslo central"}, "__typename": "w_Weather"},
                {"city": "Tokyo", "temperature": 20, "unit": "FAHRENHEIT", "station": {"name": "Tokyo central"}, "__typename": "w_Weather"},
            ],
            "forecast": [{"temperature": 20}],
            "u": {"name": "w_Unit"},
        }})
    );
    let documents = weather.documents.lock().unwrap().clone();
    assert_eq!(documents.len(), 2, "{documents:?}");
    assert!(
        documents.iter().any(
            |d| d.contains("w_forecast: forecast(filter: $v0)") && d.contains("$v0: Forecast!")
        ),
        "{documents:?}"
    );
    assert!(documents.iter().all(|d| !d.contains("w_Forecast")));
}

#[tokio::test(flavor = "multi_thread")]
async fn introspection_and_sdl_give_the_same_remote_schema() {
    let pool = database("http_remote_introspect", &["schema.sql", "seed.sql"]).await;
    let (address, _, schema) = serve_weather().await;
    let introspected = RemoteSchema::new("weather", &format!("http://{address}/graphql"))
        .unwrap()
        .roles(["admin"])
        .introspect()
        .await
        .unwrap();
    let from_sdl = serve_rusuna(role_schemas(&pool, &[remote(address, &schema)]).unwrap()).await;
    let from_introspection = serve_rusuna(role_schemas(&pool, &[introspected]).unwrap()).await;
    for query in [
        "{ weather(city: \"Oslo\") { city temperature(unit: FAHRENHEIT) station { id } } now forecast(filter: {cities: [\"Tokyo\"]}) { temperature } }",
        "{ __type(name: \"Weather\") { description fields { name type { name kind ofType { name } } args { name defaultValue } } } f: __type(name: \"Forecast\") { inputFields { name defaultValue } } }",
        "{ __schema { types { name kind } } }",
    ] {
        let expected = post(from_sdl, &[], &json!({ "query": query })).await;
        assert!(expected.get("errors").is_none(), "{expected}");
        assert_eq!(
            post(from_introspection, &[], &json!({ "query": query })).await,
            expected,
            "{query}"
        );
    }
}

const RELATION_SDL: &str = r#"
    type Query {
      invoices(count: Int!): [Invoice!]!
        @rest(source: "api", path: "/invoices/{{count}}", roles: ["user", "limited", "reviewer"])
    }

    type Invoice
      @permission(role: "user", columns: ["label", "customer", "customers"], filter: {})
      @permission(role: "limited", columns: ["label"], filter: {})
      @permission(role: "reviewer", columns: ["label", "customer", "customers"], filter: {}) {
      id: Int! @column(name: "customer_id")
      preferred: Boolean!
      label: String!
      customer: CrmCustomer
        @remote_relation(schema: "crm", field: "customer", arguments: {id: "{{id}}", preferred: "{{preferred}}"})
      customers: [CrmCustomer!]!
        @remote_relation(schema: "crm", field: "customers", arguments: {label: "customer-{{id}}"})
    }
"#;

fn customers() -> Schema {
    let customer = Object::new("Customer")
        .field(Field::new("id", TypeRef::named_nn(TypeRef::INT), |ctx| {
            FieldFuture::Value(Some(FieldValue::value(
                parent(&ctx)["id"].as_i64().unwrap(),
            )))
        }))
        .field(Field::new(
            "preferred",
            TypeRef::named_nn(TypeRef::BOOLEAN),
            |ctx| {
                FieldFuture::Value(Some(FieldValue::value(
                    parent(&ctx)["preferred"].as_bool().unwrap(),
                )))
            },
        ))
        .field(
            Field::new("label", TypeRef::named_nn(TypeRef::STRING), |ctx| {
                FieldFuture::new(async move {
                    let prefix = ctx.args.try_get("prefix")?.string()?;
                    let value = parent(&ctx);
                    Ok(Some(GraphQLValue::from(format!(
                        "{prefix}{}",
                        value["label"].as_str().unwrap()
                    ))))
                })
            })
            .argument(InputValue::new(
                "prefix",
                TypeRef::named_nn(TypeRef::STRING),
            )),
        );
    let query = Object::new("Query")
        .field(
            Field::new("customer", TypeRef::named("Customer"), |ctx| {
                FieldFuture::new(async move {
                    let id = ctx.args.try_get("id")?.i64()?;
                    let preferred = ctx.args.try_get("preferred")?.boolean()?;
                    if id < 0 {
                        ctx.add_error(
                            ctx.set_error_path(
                                async_graphql::Error::new("customer is unavailable")
                                    .into_server_error(ctx.item.pos),
                            ),
                        );
                        return Ok(None);
                    }
                    Ok(Some(FieldValue::owned_any(
                        json!({"id": id, "preferred": preferred, "label": "customer"}),
                    )))
                })
            })
            .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::INT)))
            .argument(InputValue::new(
                "preferred",
                TypeRef::named_nn(TypeRef::BOOLEAN),
            )),
        )
        .field(
            Field::new("customers", TypeRef::named_nn_list_nn("Customer"), |ctx| {
                FieldFuture::new(async move {
                    let label = ctx.args.try_get("label")?.string()?;
                    Ok(Some(FieldValue::list([FieldValue::owned_any(
                        json!({"label": label}),
                    )])))
                })
            })
            .argument(InputValue::new("label", TypeRef::named_nn(TypeRef::STRING))),
        );
    Schema::build("Query", None, None)
        .register(customer)
        .register(query)
        .finish()
        .unwrap()
}

async fn serve_relationships() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let documents = Arc::new(Mutex::new(Vec::new()));
    let remote = customers();
    let upstream = listen(recorded_remote(remote.clone(), documents.clone()).route(
        "/invoices/{count}",
        axum::routing::get(|axum::extract::Path(count): axum::extract::Path<i32>| async move {
            let row = |id, preferred| json!({"customer_id": id, "preferred": preferred, "label": "invoice"});
            let rows: Vec<_> = if count < 0 {
                vec![row(7, true), row(-1, false), row(-1, false)]
            } else {
                let distinct = (0..count).map(|i| row(i / 2, i % 2 == 0));
                distinct.clone().chain(distinct.rev()).collect()
            };
            axum::Json(rows)
        }),
    )).await;
    let source = RestSource::new("api", &format!("http://{upstream}")).unwrap();
    let remote = RemoteSchema::new("crm", &format!("http://{upstream}/graphql"))
        .unwrap()
        .sdl(remote.sdl())
        .roles(["admin", "user", "limited"])
        .type_prefix("Crm")
        .root_field_prefix("crm_");
    let schema = Arc::new(rusuna::Schema::from_sdl(RELATION_SDL).unwrap());
    let roles = schema
        .roles()
        .into_iter()
        .map(|role| {
            let built = Builder::new(schema.clone(), &role)
                .rest(source.clone())
                .remote(remote.clone())
                .build()
                .unwrap()
                .schema()
                .finish()
                .unwrap();
            (role, built)
        })
        .collect();
    (serve_rusuna(roles).await, documents)
}

#[tokio::test]
async fn rest_remote_relationships_batch_distinct_argument_tuples() {
    let (address, documents) = serve_relationships().await;
    for count in [0usize, 1, 100, 101, 205] {
        let response = post(
            address,
            &[],
            &json!({
                "query": "query($count: Int!, $prefix: String!) { rows: invoices(count: $count) { buyer: customer { id } ...Buyer } } fragment Buyer on Invoice { buyer: customer { preferred name: label(prefix: $prefix) } }",
                "variables": {"count": count, "prefix": "preferred "},
            }),
        )
        .await;
        let expected = (0..count).chain((0..count).rev()).map(|i| {
            json!({"buyer": {"id": i / 2, "preferred": i % 2 == 0, "name": "preferred customer"}})
        }).collect::<Vec<_>>();
        assert_eq!(response, json!({"data": {"rows": expected}}));

        let batches = std::mem::take(&mut *documents.lock().unwrap());
        assert_eq!(batches.len(), count.div_ceil(100));
        for (batch, start) in batches.iter().zip((0..count).step_by(100)) {
            let document = async_graphql::parser::parse_query(batch).unwrap();
            let operation = document.operations.iter().next().unwrap().1;
            let fields = &operation.node.selection_set.node.items;
            assert_eq!(fields.len(), (count - start).min(100), "{batch}");
            for (index, field) in fields.iter().enumerate() {
                let async_graphql::parser::types::Selection::Field(field) = &field.node else {
                    panic!("expected a remote query field");
                };
                assert_eq!(field.node.response_key().node.as_str(), format!("r{index}"));
                assert_eq!(field.node.name.node.as_str(), "customer");
            }
        }
    }
}

#[tokio::test]
async fn rest_remote_relationship_lists_respect_both_role_visibility_rules() {
    let (address, documents) = serve_relationships().await;
    let visible = post(
        address,
        &role_headers("user", &json!({})),
        &json!({"query": "{ invoices(count: 1) { customers { display: label(prefix: \"\") __typename } } }"}),
    )
    .await;
    let row = json!({"customers": [{"display": "customer-0", "__typename": "CrmCustomer"}]});
    assert_eq!(visible, json!({"data": {"invoices": [row, row]}}));
    assert_eq!(documents.lock().unwrap().len(), 1);

    for (role, remote_root) in [("limited", true), ("reviewer", false)] {
        let hidden = post(
            address,
            &role_headers(role, &json!({})),
            &json!({"query": "{ invoice: __type(name: \"Invoice\") { fields { name } } __schema { queryType { fields { name } } } }"}),
        )
        .await;
        assert_eq!(
            hidden["data"]["invoice"]["fields"],
            json!([{"name": "label"}]),
            "{hidden}"
        );
        let roots = hidden["data"]["__schema"]["queryType"]["fields"]
            .as_array()
            .unwrap();
        assert_eq!(
            roots.iter().any(|field| field["name"] == "crm_customer"),
            remote_root
        );
    }
    assert_eq!(documents.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn rest_remote_relationship_errors_use_each_parent_path() {
    let (address, documents) = serve_relationships().await;
    let response = post(
        address,
        &[],
        &json!({"query": "{ rows: invoices(count: -1) { buyer: customer { id } } }"}),
    )
    .await;
    assert_eq!(
        response["data"],
        json!({"rows": [{"buyer": {"id": 7}}, {"buyer": null}, {"buyer": null}]}),
        "{response}"
    );
    let errors = response["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2, "{response}");
    for index in [1, 2] {
        let path = json!(["rows", index, "buyer"]);
        let error = errors.iter().find(|error| error["path"] == path).unwrap();
        assert_eq!(error["message"], "customer is unavailable");
        assert_eq!(error["extensions"]["code"], "remote-schema-error");
        assert_eq!(
            error["extensions"]["remote_errors"][0]["path"],
            json!(["r1"])
        );
        assert_eq!(error["locations"], json!([{"line": 1, "column": 31}]));
    }
    assert_eq!(documents.lock().unwrap().len(), 1);
}

#[test]
fn rest_remote_relationship_declarations_require_scalar_parent_fields_and_query_targets() {
    let declaration = |ty: &str, schema: &str, field: &str, args: &str| {
        format!(
            r#"type Query {{ invoices: [Invoice!]! @rest(source: "api", path: "/invoices", roles: ["user"]) }}
               type Invoice {{
                 id: Int
                 list: [Int]
                 object: Details
                 customer: {ty} @remote_relation(schema: "{schema}", field: "{field}", arguments: {{{args}}})
               }}
               type Details {{ name: String }}"#
        )
    };
    for parent in ["missing", "list", "object", "customer", "$X-Hasura-User-Id"] {
        let args = format!(r#"id: "{{{{{parent}}}}}""#);
        let sdl = declaration("Customer", "crm", "customer", &args);
        assert!(rusuna::Schema::from_sdl(&sdl).is_err(), "{sdl}");
    }

    let remote = RemoteSchema::new("crm", "http://127.0.0.1:1/graphql")
        .unwrap()
        .sdl("type Query { customer(id: Int!): Customer } type Mutation { update: Customer } type Customer { id: Int }");
    for (ty, name, field, args, message) in [
        (
            "Customer",
            "missing",
            "customer",
            r#"id: "{{id}}""#,
            "needs Builder::remote",
        ),
        (
            "Customer",
            "crm",
            "unknown",
            r#"id: "{{id}}""#,
            "unknown query field",
        ),
        (
            "Customer",
            "crm",
            "update",
            r#"id: "{{id}}""#,
            "unknown query field",
        ),
        (
            "Other",
            "crm",
            "customer",
            r#"id: "{{id}}""#,
            "requires the imported type",
        ),
        (
            "[Customer]",
            "crm",
            "customer",
            r#"id: "{{id}}""#,
            "requires the imported type",
        ),
        ("Customer", "crm", "customer", "", "required argument id"),
        (
            "Customer",
            "crm",
            "customer",
            r#"id: "{{id}}", extra: "extra""#,
            "unknown argument customer.extra",
        ),
    ] {
        let sdl = declaration(ty, name, field, args);
        let schema = Arc::new(rusuna::Schema::from_sdl(&sdl).unwrap());
        let error = match Builder::new(schema, "user")
            .rest(RestSource::new("api", "http://127.0.0.1:1").unwrap())
            .remote(remote.clone())
            .build()
        {
            Err(error) => error.to_string(),
            Ok(_) => panic!("accepted {sdl}"),
        };
        assert!(error.contains(message), "{error}");
    }
}
