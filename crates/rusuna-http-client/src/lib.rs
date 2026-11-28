//! The HTTP client of rusuna: hyper 1 with rustls on the ring provider and the webpki root
//! certificates, so it builds without OpenSSL or CMake.
//!
//! `rusuna-events` sends webhooks with it and `rusuna-async-graphql` calls remote schemas
//! and REST data sources with it.

pub use hyper::Uri;
pub use hyper::http;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use std::{fmt, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Limits {
    pub timeout: Duration,
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    Timeout,
    TooLarge,
    InvalidHeader,
    Connection(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("the HTTP request timed out"),
            Self::TooLarge => f.write_str("the HTTP response exceeded the body limit"),
            Self::InvalidHeader => {
                f.write_str("an HTTP header contains a carriage return or newline")
            }
            Self::Connection(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for Error {}

/// The status, headers and body of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
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
        let mut request_headers = HeaderMap::new();
        request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (name, value) in headers {
            request_headers.append(name.clone(), value.clone());
        }
        self.send(
            Method::POST,
            uri,
            request_headers,
            Some(body.into()),
            usize::MAX,
        )
        .await
    }

    pub async fn request(
        &self,
        method: Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: Option<Bytes>,
        limits: Limits,
    ) -> Result<Response, Error> {
        tokio::time::timeout(
            limits.timeout,
            self.send(method, uri, headers.clone(), body, limits.max_body_bytes),
        )
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn send(
        &self,
        method: Method,
        uri: &Uri,
        headers: HeaderMap,
        body: Option<Bytes>,
        max_body_bytes: usize,
    ) -> Result<Response, Error> {
        if headers.values().any(|value| {
            value
                .as_bytes()
                .iter()
                .any(|byte| matches!(byte, b'\r' | b'\n'))
        }) {
            return Err(Error::InvalidHeader);
        }
        let mut request = Request::new(Full::new(body.unwrap_or_default()));
        *request.method_mut() = method;
        *request.uri_mut() = uri.clone();
        *request.headers_mut() = headers;
        let response = self
            .0
            .request(request)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        let (parts, body) = response.into_parts();
        let body = Limited::new(body, max_body_bytes)
            .collect()
            .await
            .map_err(|error| {
                if error.is::<LengthLimitError>() {
                    Error::TooLarge
                } else {
                    Error::Connection(error.to_string())
                }
            })?
            .to_bytes();
        Ok(Response {
            status: parts.status,
            headers: parts.headers,
            body,
        })
    }
}
