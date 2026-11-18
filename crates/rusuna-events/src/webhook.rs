use rusuna_http_client::http::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value as Json;

use crate::Error;

/// An HTTP endpoint that receives events as `POST` requests with a JSON body.
///
/// A response with a 2xx status is a successful delivery; any other status, a connection
/// error or the trigger's timeout is a failed attempt.
#[derive(Debug, Clone)]
pub struct Webhook {
    pub(crate) url: rusuna_http_client::Uri,
    pub(crate) headers: Vec<(HeaderName, HeaderValue)>,
}

impl Webhook {
    /// A webhook at `url`, an `http` or `https` URL.
    pub fn new(url: &str) -> Result<Self, Error> {
        let uri: rusuna_http_client::Uri = url
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
pub(crate) struct Client(rusuna_http_client::Client);

pub(crate) struct Response {
    pub status: Option<u16>,
    pub error: Option<String>,
}

impl Client {
    pub fn new() -> Self {
        Self(rusuna_http_client::Client::new())
    }

    pub async fn post(&self, webhook: &Webhook, body: &Json) -> Response {
        let mut headers = HeaderMap::new();
        for (name, value) in &webhook.headers {
            headers.append(name.clone(), value.clone());
        }
        match self
            .0
            .post_json(&webhook.url, &headers, body.to_string())
            .await
        {
            Ok(response) if response.status.is_success() => Response {
                status: Some(response.status.as_u16()),
                error: None,
            },
            Ok(response) => Response {
                status: Some(response.status.as_u16()),
                error: Some(format!(
                    "the webhook answered {}: {}",
                    response.status,
                    String::from_utf8_lossy(&response.body)
                        .chars()
                        .take(1000)
                        .collect::<String>()
                )),
            },
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
