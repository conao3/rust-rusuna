use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde_json::Value as Json;

use crate::Error;

/// An HTTP endpoint that receives events as `POST` requests with a JSON body.
///
/// A response with a 2xx status is a successful delivery; any other status, a connection
/// error or the trigger's timeout is a failed attempt.
#[derive(Debug, Clone)]
pub struct Webhook {
    pub(crate) url: hyper::Uri,
    pub(crate) headers: Vec<(HeaderName, HeaderValue)>,
}

impl Webhook {
    /// A webhook at `url`, an `http` or `https` URL.
    pub fn new(url: &str) -> Result<Self, Error> {
        let uri: hyper::Uri = url
            .parse()
            .map_err(|e| Error::Definition(format!("invalid webhook URL {url:?}: {e}")))?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
            return Err(Error::Definition(format!(
                "invalid webhook URL {url:?}: use an http or https URL"
            )));
        }
        Ok(Self {
            url: uri,
            headers: Vec::new(),
        })
    }

    /// Adds a header to every request.
    pub fn header(mut self, name: &str, value: &str) -> Result<Self, Error> {
        let invalid = |e: String| Error::Definition(format!("invalid webhook header {name}: {e}"));
        self.headers.push((
            HeaderName::from_bytes(name.as_bytes()).map_err(|e| invalid(e.to_string()))?,
            HeaderValue::from_str(value).map_err(|e| invalid(e.to_string()))?,
        ));
        Ok(self)
    }
}

#[derive(Clone)]
pub(crate) struct Client(HyperClient<HttpsConnector<HttpConnector>, Full<Bytes>>);

pub(crate) struct Response {
    pub status: Option<u16>,
    pub error: Option<String>,
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

    pub async fn post(&self, webhook: &Webhook, body: &Json) -> Response {
        let mut request = Request::post(webhook.url.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("a POST request with a parsed URI is valid");
        for (name, value) in &webhook.headers {
            request.headers_mut().append(name.clone(), value.clone());
        }
        match self.0.request(request).await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return Response {
                        status: Some(status.as_u16()),
                        error: None,
                    };
                }
                let body = match response.into_body().collect().await {
                    Ok(body) => String::from_utf8_lossy(&body.to_bytes()).into_owned(),
                    Err(e) => e.to_string(),
                };
                Response {
                    status: Some(status.as_u16()),
                    error: Some(format!(
                        "the webhook answered {status}: {}",
                        body.chars().take(1000).collect::<String>()
                    )),
                }
            }
            Err(e) => Response {
                status: None,
                error: Some(format!("the webhook request failed: {e}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhooks_require_http_urls_and_valid_headers() {
        assert!(Webhook::new("https://example.com/hook").is_ok());
        assert!(Webhook::new("http://127.0.0.1:8080/").is_ok());
        for url in ["ftp://example.com/", "/relative", "not a url"] {
            assert!(Webhook::new(url).is_err(), "{url}");
        }
        let webhook = Webhook::new("http://127.0.0.1/").unwrap();
        assert!(webhook.clone().header("x-secret", "value").is_ok());
        assert!(webhook.clone().header("bad name", "value").is_err());
        assert!(webhook.header("x-secret", "line\nbreak").is_err());
    }
}
