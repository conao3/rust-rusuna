use async_graphql::Value;
use async_graphql::dynamic::{Field, FieldFuture, FieldValue, Object, Schema, TypeRef};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use std::collections::HashMap;
use std::sync::Arc;

fn schema(rusuna: &Arc<rusuna::Schema>, role: &str, pool: &sqlx::PgPool) -> Schema {
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
        .finish()
        .unwrap()
}

fn session(parts: &Parts) -> Result<rusuna::Session, (StatusCode, &'static str)> {
    let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok());
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
    let schemas: HashMap<String, Schema> = ["reader", "writer"]
        .into_iter()
        .map(|role| (role.to_string(), schema(&rusuna, role, &pool)))
        .collect();
    let app = axum::Router::new().route(
        "/api/graphql",
        rusuna_axum::graphql(schemas, |parts: &Parts| {
            let result = session(parts).map_err(IntoResponse::into_response);
            async move { result }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    println!("listening on http://127.0.0.1:3000/api/graphql");
    axum::serve(listener, app).await.unwrap();
}
