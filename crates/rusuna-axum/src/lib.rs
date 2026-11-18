//! axum routes that serve rusuna async-graphql schemas.
//!
//! [`graphql`] takes one schema per role and a function that builds a
//! [`rusuna::Session`] from the request, and returns a `POST` route. [`graphql_ws`] serves
//! the same schemas over WebSocket with the `graphql-transport-ws` protocol, for
//! subscriptions, queries and mutations. [`rest`] serves named GraphQL documents as REST
//! endpoints on the same schemas.

mod rest;

pub use rest::{Endpoint, EndpointError, MAX_BODY_BYTES, RestMethod, rest};

use async_graphql::dynamic::Schema;
use async_graphql::http::ALL_WEBSOCKET_PROTOCOLS;
use async_graphql::{Data, ErrorExtensions, Response as GraphQLBody};
use async_graphql_axum::{GraphQLProtocol, GraphQLRequest, GraphQLResponse, GraphQLWebSocket};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use futures_util::{StreamExt, stream};
use rusuna_async_graphql::ClientHeaders;
use serde_json::Value as Json;
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
            let headers = ClientHeaders(parts.headers.clone());
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
            GraphQLResponse::from(schema.execute(request.data(session).data(headers)).await)
                .into_response()
        }
    })
}

/// A `GET` route that upgrades to a WebSocket and speaks `graphql-transport-ws`.
///
/// The session function receives the upgrade request's head and the payload of the
/// client's `connection_init` message, which must be the first message, and returns the
/// connection's [`rusuna::Session`] or an error message. The connection uses the schema of
/// the session's role for every operation, with the session in the connection data. An
/// error, a role without a schema or another first message closes the connection with code
/// 4403 or 4400.
pub fn graphql_ws<S, F, Fut>(schemas: HashMap<String, Schema>, session: F) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
    F: Fn(&Parts, Json) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<rusuna::Session, String>> + Send + 'static,
{
    let schemas = Arc::new(schemas);
    get(move |request: Request| {
        let schemas = schemas.clone();
        let session = session.clone();
        async move {
            let (mut parts, _) = request.into_parts();
            let protocol = match GraphQLProtocol::from_request_parts(&mut parts, &()).await {
                Ok(protocol) => protocol,
                Err(rejection) => return rejection.into_response(),
            };
            let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
                Ok(upgrade) => upgrade,
                Err(rejection) => return rejection.into_response(),
            };
            upgrade
                .protocols(ALL_WEBSOCKET_PROTOCOLS)
                .on_upgrade(move |socket| serve(socket, protocol, parts, schemas, session))
        }
    })
}

async fn close(mut socket: WebSocket, code: u16, reason: String) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

async fn serve<F, Fut>(
    mut socket: WebSocket,
    protocol: GraphQLProtocol,
    parts: Parts,
    schemas: Arc<HashMap<String, Schema>>,
    session: F,
) where
    F: Fn(&Parts, Json) -> Fut,
    Fut: Future<Output = Result<rusuna::Session, String>>,
{
    let first = loop {
        match socket.recv().await {
            Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => break message,
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
            Some(Ok(_)) => {}
        }
    };
    let init = serde_json::from_slice::<Json>(&first.clone().into_data())
        .ok()
        .filter(|message| message["type"] == "connection_init");
    let Some(init) = init else {
        return close(socket, 4400, "expected connection_init".into()).await;
    };
    let payload = init.get("payload").cloned().unwrap_or(Json::Null);
    let session = match session(&parts, payload).await {
        Ok(session) => session,
        Err(message) => return close(socket, 4403, message).await,
    };
    let Some(schema) = schemas.get(&session.role).cloned() else {
        return close(
            socket,
            4403,
            format!("no schema serves the role {}", session.role),
        )
        .await;
    };
    let headers = ClientHeaders(parts.headers.clone());
    let (sink, rest) = socket.split();
    let messages = stream::once(async move { Ok(first) }).chain(rest);
    GraphQLWebSocket::new_with_pair(sink, messages, schema, protocol)
        .on_connection_init(move |_| async move {
            let mut data = Data::default();
            data.insert(session);
            data.insert(headers);
            Ok(data)
        })
        .serve()
        .await
}
