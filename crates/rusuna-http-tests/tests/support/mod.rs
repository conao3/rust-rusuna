#![allow(dead_code)]

use async_graphql::Value as GraphQLValue;
use async_graphql::dynamic::{Field, FieldFuture, FieldValue, Object, TypeRef};
use serde_json::Value as Json;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

pub fn fixture(path: &str) -> String {
    std::fs::read_to_string(format!("{ROOT}/tests/fixtures/{path}")).unwrap()
}

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

pub async fn database(name: &str, scripts: &[&str]) -> PgPool {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options("postgres"))
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {name}"
    )))
    .execute(&admin)
    .await
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options(name))
        .await
        .unwrap();
    for script in scripts {
        sqlx::raw_sql(sqlx::AssertSqlSafe(fixture(script)))
            .execute(&pool)
            .await
            .unwrap();
    }
    pool
}

fn viewer(parts: &rusuna_async_graphql::Parts, role: &str, mount: Option<&str>) -> Object {
    let role = role.to_string();
    let mut viewer = Object::new("Viewer").field(Field::new(
        "role",
        TypeRef::named_nn(TypeRef::STRING),
        move |_| {
            let role = role.clone();
            FieldFuture::new(async move { Ok(Some(GraphQLValue::from(role))) })
        },
    ));
    if let Some(field) = mount.and_then(|name| parts.root_field(name)) {
        viewer = viewer.field(field);
    }
    viewer
}

pub fn schemas(
    sdl: &str,
    pool: &PgPool,
    mount: Option<&str>,
) -> HashMap<String, async_graphql::dynamic::Schema> {
    let schema = Arc::new(rusuna::Schema::from_sdl(sdl).unwrap());
    schema
        .roles()
        .into_iter()
        .chain(["visitor".to_string()])
        .map(|role| {
            let mut builder = rusuna_async_graphql::Builder::new(schema.clone(), &role);
            if mount.is_some() {
                builder = builder.field(
                    "article",
                    "wordCount",
                    TypeRef::named_nn(TypeRef::INT),
                    &["title"],
                    |ctx| {
                        FieldFuture::new(async move {
                            let title = rusuna_async_graphql::parent_column(&ctx, "title")?;
                            let words = title
                                .as_str()
                                .unwrap_or_default()
                                .split_whitespace()
                                .count();
                            Ok(Some(GraphQLValue::from(words as i32)))
                        })
                    },
                );
            }
            let parts = builder.build().unwrap();
            let viewer = viewer(&parts, &role, mount);
            let built = parts
                .query_field(Field::new("viewer", TypeRef::named_nn("Viewer"), |_| {
                    FieldFuture::new(async { Ok(Some(FieldValue::owned_any(()))) })
                }))
                .schema()
                .register(viewer)
                .data(pool.clone())
                .finish()
                .unwrap();
            (role, built)
        })
        .collect()
}

pub async fn serve(schemas: HashMap<String, async_graphql::dynamic::Schema>) -> SocketAddr {
    let app = axum::Router::new().route(
        "/graphql",
        rusuna_axum::graphql(schemas, |parts: &axum::http::request::Parts| {
            let header = |name: &str| {
                parts
                    .headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let role = header("x-hasura-role").unwrap_or_else(|| "admin".into());
            let user = header("x-hasura-user-id");
            async move {
                let mut session = serde_json::Map::new();
                if let Some(user) = user {
                    session.insert("user_id".into(), Json::from(user.parse::<i64>().unwrap()));
                }
                Ok(rusuna::Session::from_json(role, &Json::Object(session)).unwrap())
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

pub async fn post(address: SocketAddr, headers: &[(&str, String)], body: &Json) -> Json {
    let body = body.to_string();
    let mut request = format!(
        "POST /graphql HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8(response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    serde_json::from_str(body).unwrap()
}

pub fn role_headers(role: &str, session: &Json) -> Vec<(&'static str, String)> {
    let mut headers = vec![("x-hasura-role", role.to_string())];
    if let Some(user) = session.get("user_id") {
        headers.push(("x-hasura-user-id", user.to_string()));
    }
    headers
}

pub fn sorted(value: Json, unordered: &[&str]) -> Json {
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
                .map(|(key, value)| match sorted(value, unordered) {
                    Json::Array(mut items) if unordered.contains(&key.as_str()) => {
                        items.sort_by_key(|item| item.to_string());
                        (key, Json::Array(items))
                    }
                    value => (key, value),
                })
                .collect(),
        ),
        value => value,
    }
}

pub fn session(parts: &axum::http::request::Parts) -> rusuna::Session {
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let role = header("x-hasura-role").unwrap_or_else(|| "admin".into());
    let mut session = serde_json::Map::new();
    if let Some(user) = header("x-hasura-user-id") {
        session.insert("user_id".into(), Json::from(user.parse::<i64>().unwrap()));
    }
    rusuna::Session::from_json(role, &Json::Object(session)).unwrap()
}

pub async fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<&str>,
) -> (u16, Json) {
    let body = body.unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8(response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Json::Null))
}
