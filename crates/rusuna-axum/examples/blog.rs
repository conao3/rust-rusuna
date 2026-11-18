use async_graphql::Value;
use async_graphql::dynamic::{Field, FieldFuture, FieldValue, Object, Schema, TypeRef};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use rusuna_axum::{Endpoint, RestMethod};
use rusuna_events::{Cause, Event, EventTrigger, Events, Operation, Retry, WorkerOptions};
use std::collections::HashMap;
use std::sync::Arc;

fn schema(
    rusuna: &Arc<rusuna::Schema>,
    role: &str,
    pool: &sqlx::PgPool,
    subscriptions: &rusuna_sqlx::Subscriptions,
) -> Schema {
    let parts = rusuna_async_graphql::Builder::new(rusuna.clone(), role)
        .field(
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
                    Ok(Some(Value::from(words as i32)))
                })
            },
        )
        .build()
        .unwrap();
    let viewer = Object::new("Viewer")
        .field(Field::new(
            "role",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let session = ctx.data::<rusuna::Session>()?;
                    Ok(Some(Value::from(session.role.clone())))
                })
            },
        ))
        .field(parts.root_field("article").unwrap());
    parts
        .query_field(Field::new("viewer", TypeRef::named_nn("Viewer"), |_| {
            FieldFuture::new(async { Ok(Some(FieldValue::owned_any(()))) })
        }))
        .schema()
        .register(viewer)
        .data(pool.clone())
        .data(subscriptions.clone())
        .finish()
        .unwrap()
}

fn session<'a>(
    header: impl Fn(&str) -> Option<&'a str>,
) -> Result<rusuna::Session, (StatusCode, &'static str)> {
    let Some(role) = header("x-role") else {
        return Err((StatusCode::UNAUTHORIZED, "x-role is required"));
    };
    let mut session = rusuna::Session::new(role);
    if let Some(user) = header("x-user-id") {
        let user = user
            .parse()
            .map_err(|_| (StatusCode::BAD_REQUEST, "x-user-id must be an integer"))?;
        session = session.variable("user_id", rusuna::Value::Int(user));
    }
    Ok(session)
}

#[tokio::main]
async fn main() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let rusuna = Arc::new(rusuna::Schema::from_sdl(include_str!("blog.graphql")).unwrap());
    let subscriptions =
        rusuna_sqlx::Subscriptions::new(pool.clone(), rusuna_sqlx::SubscriptionOptions::default());
    let schemas: HashMap<String, Schema> = ["reader", "writer"]
        .into_iter()
        .map(|role| {
            let schema = schema(&rusuna, role, &pool, &subscriptions);
            (role.to_string(), schema)
        })
        .collect();
    let header_session = |parts: &Parts| {
        let result = session(|name| parts.headers.get(name).and_then(|v| v.to_str().ok()))
            .map_err(IntoResponse::into_response);
        async move { result }
    };
    let rest = rusuna_axum::rest(
        schemas.clone(),
        vec![Endpoint::new(
            "article",
            "/api/rest/articles/:id",
            [RestMethod::Get],
            "query Article($id: Int!) { article_by_pk(id: $id) { id title author { name } } }",
        )],
        header_session,
    )
    .await
    .unwrap();
    let app = axum::Router::new()
        .route(
            "/api/graphql",
            rusuna_axum::graphql(schemas.clone(), header_session).merge(rusuna_axum::graphql_ws(
                schemas,
                |_: &Parts, payload: serde_json::Value| {
                    let result = session(|name| payload["headers"][name].as_str())
                        .map_err(|(_, message)| message.to_string());
                    async move { result }
                },
            )),
        )
        .merge(rest);
    let events = Events::new()
        .trigger(EventTrigger::new("article_created", "article").operations([Operation::Insert]))
        .handler(
            "article_created",
            Retry::default(),
            |event: Event| async move {
                if let Cause::Row { new: Some(row), .. } = &event.cause {
                    let title = row["title"].as_str().unwrap_or_default();
                    println!("article {} created: {title}", row["id"]);
                }
                Ok(())
            },
        );
    events.migrate(&pool).await.unwrap();
    let worker = events
        .worker(pool.clone(), WorkerOptions::default())
        .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let worker = tokio::spawn(worker.run(async {
        let _ = stopped.await;
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    println!("listening on http://127.0.0.1:3000/api/graphql");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
    let _ = stop.send(());
    worker.await.unwrap();
}
