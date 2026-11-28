use super::*;
use percent_encoding::percent_decode_str;
use rusuna::{RestPagination, RestPaginationMode};
use rusuna_http_client::http::header::{LINK, RETRY_AFTER};
use std::time::SystemTime;
use url::{Url, form_urlencoded};

pub(super) fn retry_delay(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|date| date.duration_since(now).unwrap_or_default())
}

pub(super) async fn fetch(
    state: &RestRequest,
    source: &RestSource,
    field: &RestField,
    original: Uri,
    headers: &HeaderMap,
    table: Option<TableCall>,
) -> async_graphql::Result<Option<RestValue<'static>>> {
    let RestPagination { mode, max_pages } = field.paginate.as_ref().unwrap();
    let mut uri = match mode {
        RestPaginationMode::Offset { query, .. } => with_query(&original, query, "0")?,
        _ => original.clone(),
    };
    let mut items = Vec::new();
    for page in 1..=*max_pages {
        let response = cached_get(state, source, uri.clone(), headers, table.clone()).await;
        let reply = match response_json(source, field, &response, page == 1)? {
            Some(reply) => reply,
            None if page == 1 => return Ok(None),
            None => return Err(unexpected()),
        };
        let rows = reply.value.as_array().ok_or_else(unexpected)?;
        items.extend(rows.iter().cloned());
        if page == *max_pages {
            break;
        }
        let next = match mode {
            RestPaginationMode::Link => next_link(reply.headers)
                .map(|target| scoped_link(&source.url, &uri, target))
                .transpose()?,
            RestPaginationMode::Cursor { next, query } => next
                .iter()
                .try_fold(reply.body, |value, key| value.get(key))
                .filter(|value| !value.is_null() && value.as_str() != Some(""))
                .map(|value| with_query(&original, query, &as_text(value.clone())))
                .transpose()?,
            RestPaginationMode::Offset { query, size } if rows.len() >= *size => {
                let offset = page.checked_mul(*size).ok_or_else(unexpected)?;
                Some(with_query(&original, query, &offset.to_string())?)
            }
            RestPaginationMode::Offset { .. } => None,
        };
        let Some(next) = next else { break };
        uri = next;
    }
    Ok(Some(RestValue::from(Json::Array(items))))
}

fn with_query(uri: &Uri, name: &str, value: &str) -> async_graphql::Result<Uri> {
    let mut path = uri.path().to_owned();
    path.push('?');
    for pair in uri
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        if form_urlencoded::parse(pair.as_bytes()).any(|(key, _)| key == name) {
            continue;
        }
        path.push_str(pair);
        path.push('&');
    }
    path.push_str(&utf8_percent_encode(name, ENCODE).to_string());
    path.push('=');
    path.push_str(&utf8_percent_encode(value, ENCODE).to_string());
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path.parse().map_err(|_| unexpected())?);
    Uri::from_parts(parts).map_err(|_| unexpected())
}

fn next_link(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(LINK)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| split_link(value, ','))
        .find_map(|link| {
            let (target, parameters) = link.trim_start().strip_prefix('<')?.split_once('>')?;
            split_link(parameters, ';')
                .filter_map(|parameter| parameter.split_once('='))
                .any(|(name, value)| {
                    name.trim().eq_ignore_ascii_case("rel")
                        && value
                            .trim()
                            .trim_matches('"')
                            .split_ascii_whitespace()
                            .any(|relation| relation.eq_ignore_ascii_case("next"))
                })
                .then_some(target)
        })
}

fn split_link(value: &str, delimiter: char) -> impl Iterator<Item = &str> {
    let mut angle = false;
    let mut quoted = false;
    let mut escaped = false;
    value.split(move |ch| {
        if escaped {
            escaped = false;
        } else if quoted {
            match ch {
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
        } else if angle {
            if ch == '>' {
                angle = false;
            }
        } else {
            match ch {
                '<' => angle = true,
                '"' => quoted = true,
                _ if ch == delimiter => return true,
                _ => {}
            }
        }
        false
    })
}

fn scoped_link(base: &Uri, current: &Uri, target: &str) -> async_graphql::Result<Uri> {
    let parse = |uri: &Uri| Url::parse(&uri.to_string()).map_err(|_| unexpected());
    let base = parse(base)?;
    let current = parse(current)?;
    let mut next = current.join(target).map_err(|_| unexpected())?;
    if next.scheme() != base.scheme()
        || next.origin() != base.origin()
        || !next.username().is_empty()
        || next.password().is_some()
        || !within_path(base.path(), next.path())
    {
        return Err(unexpected());
    }
    next.set_fragment(None);
    next.as_str().parse().map_err(|_| unexpected())
}

fn within_path(base: &str, next: &str) -> bool {
    let base = base.trim_end_matches('/');
    let segments = || next.split('/').map(percent_decode_str);
    let mut prefix = segments();
    (base.is_empty()
        || base
            .split('/')
            .map(percent_decode_str)
            .all(|base| prefix.next().is_some_and(|segment| segment.eq(base))))
        && segments().all(|segment| {
            let decoded = Cow::<[u8]>::from(segment);
            !matches!(decoded.as_ref(), b"." | b"..")
                && !decoded
                    .iter()
                    .any(|&byte| byte == b'/' || byte == b'\\' || byte.is_ascii_control())
        })
}
