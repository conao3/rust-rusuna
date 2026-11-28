use rusuna_async_graphql::{Builder, RestSource};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:3000".into());
    let source = RestSource::new("pokeapi", "https://pokeapi.co/api/v2")?
        .header("user-agent", "rusuna-example/0.x")?;
    let schema = Arc::new(rusuna::Schema::from_sdl(include_str!("pokeapi.graphql"))?);
    let anonymous = Builder::new(schema, "anonymous")
        .rest(source)
        .build()?
        .schema()
        .finish()?;
    let app = axum::Router::new().route(
        "/graphql",
        rusuna_axum::graphql(
            HashMap::from([("anonymous".into(), anonymous)]),
            |_| async { Ok(rusuna::Session::new("anonymous")) },
        ),
    );
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!(
        "GraphQL endpoint at http://{}/graphql",
        listener.local_addr()?
    );
    axum::serve(listener, app).await?;
    Ok(())
}
