//! An axum route that serves rusuna async-graphql schemas.
//!
//! [`graphql`] takes one schema per role and a function that builds a
//! [`rusuna::Session`] from the request, and returns a `POST` route.

use async_graphql::dynamic::Schema;
use async_graphql::{ErrorExtensions, Response as GraphQLBody};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::{FromRequest, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, post};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

pub fn graphql<S, F, Fut>(schemas: HashMap<String, Schema>, session: F) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
    F: Fn(&Parts) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<rusuna::Session, Response>> + Send + 'static,
{
    let schemas = Arc::new(schemas);
    post(move |request: Request| {
        let schemas = schemas.clone();
        let session = session.clone();
        async move {
            let (parts, body) = request.into_parts();
            let session = match session(&parts).await {
                Ok(session) => session,
                Err(response) => return response,
            };
            let Some(schema) = schemas.get(&session.role) else {
                let error = async_graphql::Error::new(format!(
                    "no schema serves the role {}",
                    session.role
                ))
                .extend_with(|_, e| e.set("code", "access-denied"))
                .into_server_error(Default::default());
                return GraphQLResponse::from(GraphQLBody::from_errors(vec![error]))
                    .into_response();
            };
            let request =
                match GraphQLRequest::<async_graphql_axum::rejection::GraphQLRejection>::from_request(
                    Request::from_parts(parts, body),
                    &(),
                )
                .await
                {
                    Ok(request) => request.into_inner(),
                    Err(rejection) => return rejection.into_response(),
                };
            GraphQLResponse::from(schema.execute(request.data(session)).await).into_response()
        }
    })
}
