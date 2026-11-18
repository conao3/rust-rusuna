//! The HTTP client of rusuna: hyper 1 with rustls on the ring provider and the webpki root
//! certificates, so it builds without OpenSSL or CMake.
//!
//! `rusuna-events` sends webhooks with it and `rusuna-async-graphql` forwards remote
//! schema requests with it.

pub use hyper::Uri;
pub use hyper::http;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use hyper::{Request, StatusCode};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use std::fmt;

/// A request that did not produce a response: a connection, TLS or protocol error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The status and the whole body of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: StatusCode,
    pub body: Bytes,
}

/// A pooled client for `http` and `https` URLs. Clones share the pool.
#[derive(Clone)]
pub struct Client(HyperClient<HttpsConnector<HttpConnector>, Full<Bytes>>);

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        let connector = HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(rustls::crypto::ring::default_provider())
            .expect("the ring provider supports the default protocol versions")
            .https_or_http()
            .enable_http1()
            .build();
        Self(HyperClient::builder(TokioExecutor::new()).build(connector))
    }

    /// Sends `body` as `POST` with `content-type: application/json` and `headers`, and
    /// reads the whole response.
    pub async fn post_json(
        &self,
        uri: &Uri,
        headers: &HeaderMap,
        body: impl Into<Bytes>,
    ) -> Result<Response, Error> {
        let mut request = Request::post(uri.clone())
            .body(Full::new(body.into()))
            .expect("a POST request with a parsed URI is valid");
        request
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (name, value) in headers {
            request.headers_mut().append(name.clone(), value.clone());
        }
        let response = self
            .0
            .request(request)
            .await
            .map_err(|e| Error(e.to_string()))?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| Error(e.to_string()))?
            .to_bytes();
        Ok(Response { status, body })
    }
}
