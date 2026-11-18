use async_graphql::dynamic::Schema;
use async_graphql::parser::types::{BaseType, DocumentOperations, OperationType, Type};
use async_graphql::{Request as GraphQLRequest, Response as GraphQLBody, ServerError, Variables};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{FromRequestParts, RawPathParams, Request};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter};
use rusuna_async_graphql::ClientHeaders;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::sync::Arc;

/// The largest request body a REST endpoint reads.
pub const MAX_BODY_BYTES: usize = 1_048_576;

/// An HTTP method of a REST endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RestMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl RestMethod {
    fn filter(self) -> MethodFilter {
        match self {
            Self::Get => MethodFilter::GET,
            Self::Post => MethodFilter::POST,
            Self::Put => MethodFilter::PUT,
            Self::Patch => MethodFilter::PATCH,
            Self::Delete => MethodFilter::DELETE,
        }
    }
}

/// A REST endpoint: a GraphQL query or mutation served at a URL pattern for some methods.
///
/// `url` is a path such as `/api/rest/articles/:id`; a segment `:name` binds the GraphQL
/// variable `name`. The JSON form has the keys `Name`, `Url`, `Methods` and `Query`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct Endpoint {
    pub name: String,
    pub url: String,
    pub methods: Vec<RestMethod>,
    pub query: String,
}

impl Endpoint {
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        methods: impl IntoIterator<Item = RestMethod>,
        query: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            methods: methods.into_iter().collect(),
            query: query.into(),
        }
    }
}

/// A REST endpoint that cannot be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointError {
    pub endpoint: String,
    pub message: String,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "REST endpoint {}: {}", self.endpoint, self.message)
    }
}

impl std::error::Error for EndpointError {}

struct Prepared {
    name: String,
    query: String,
    operation_name: Option<String>,
    variables: BTreeMap<String, Type>,
    roles: BTreeSet<String>,
}

/// Builds the routes of `endpoints`, each executed on the schema of the session's role.
///
/// Every endpoint is validated against the schema of every role; a role whose schema
/// rejects the document cannot call it. An endpoint that no role can call, an invalid URL
/// pattern, a subscription or a document with other than one operation is an error.
pub async fn rest<S, F, Fut>(
    schemas: HashMap<String, Schema>,
    endpoints: Vec<Endpoint>,
    session: F,
) -> Result<Router<S>, EndpointError>
where
    S: Clone + Send + Sync + 'static,
    F: Fn(&Parts) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<rusuna::Session, Response>> + Send + 'static,
{
    let schemas = Arc::new(schemas);
    let mut names = BTreeSet::new();
    let mut routes: BTreeMap<String, (String, BTreeMap<RestMethod, Arc<Prepared>>)> =
        BTreeMap::new();
    for endpoint in endpoints {
        let fail = |message: String| EndpointError {
            endpoint: endpoint.name.clone(),
            message,
        };
        if endpoint.name.is_empty() || !names.insert(endpoint.name.clone()) {
            return Err(fail("endpoint names must be nonempty and distinct".into()));
        }
        let methods: BTreeSet<_> = endpoint.methods.iter().copied().collect();
        if methods.is_empty() || methods.len() != endpoint.methods.len() {
            return Err(fail("methods must be nonempty and distinct".into()));
        }
        let (path, shape, parameters) = pattern(&endpoint.url).map_err(fail)?;
        let prepared = Arc::new(prepare(&schemas, &endpoint, &parameters).await?);
        let (existing, served) = routes
            .entry(shape)
            .or_insert_with(|| (path.clone(), BTreeMap::new()));
        if *existing != path {
            return Err(fail(format!(
                "the URL overlaps {existing} with other variable names"
            )));
        }
        for method in methods {
            if served.insert(method, prepared.clone()).is_some() {
                return Err(fail(format!(
                    "another endpoint serves {method:?} at {}",
                    endpoint.url
                )));
            }
        }
    }
    let mut app = Router::new();
    for (path, served) in routes.into_values() {
        let mut router = MethodRouter::new();
        for (method, prepared) in served {
            let schemas = schemas.clone();
            let session = session.clone();
            router = router.on(method.filter(), move |request: Request| async move {
                serve(request, method, &schemas, &prepared, session).await
            });
        }
        app = app.route(&path, router);
    }
    Ok(app)
}

fn pattern(url: &str) -> Result<(String, String, Vec<String>), String> {
    let invalid = || format!("invalid URL pattern {url:?}");
    let rest = url.strip_prefix('/').ok_or_else(invalid)?;
    let mut path = String::new();
    let mut shape = String::new();
    let mut parameters = Vec::new();
    for segment in rest.split('/') {
        if segment.is_empty() || segment.contains(['{', '}', '*', '?', '#']) {
            return Err(invalid());
        }
        if let Some(name) = segment.strip_prefix(':') {
            if !graphql_name(name) || parameters.iter().any(|p| p == name) {
                return Err(invalid());
            }
            path.push_str(&format!("/{{{name}}}"));
            shape.push_str("/{}");
            parameters.push(name.to_string());
        } else {
            if segment.contains(':') {
                return Err(invalid());
            }
            path.push('/');
            path.push_str(segment);
            shape.push('/');
            shape.push_str(segment);
        }
    }
    Ok((path, shape, parameters))
}

fn graphql_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

async fn prepare(
    schemas: &HashMap<String, Schema>,
    endpoint: &Endpoint,
    parameters: &[String],
) -> Result<Prepared, EndpointError> {
    let fail = |message: String| EndpointError {
        endpoint: endpoint.name.clone(),
        message,
    };
    let document =
        async_graphql::parser::parse_query(&endpoint.query).map_err(|e| fail(e.to_string()))?;
    let (operation_name, operation) = match &document.operations {
        DocumentOperations::Single(operation) => (None, operation),
        DocumentOperations::Multiple(operations) if operations.len() == 1 => {
            let (name, operation) = operations.iter().next().unwrap();
            (Some(name.to_string()), operation)
        }
        DocumentOperations::Multiple(_) => {
            return Err(fail("the document must have exactly one operation".into()));
        }
    };
    if operation.node.ty == OperationType::Subscription {
        return Err(fail("subscriptions cannot be REST endpoints".into()));
    }
    let variables: BTreeMap<String, Type> = operation
        .node
        .variable_definitions
        .iter()
        .map(|d| (d.node.name.node.to_string(), d.node.var_type.node.clone()))
        .collect();
    for parameter in parameters {
        match variables.get(parameter) {
            None => {
                return Err(fail(format!(
                    "the URL variable {parameter} is not a variable of the operation"
                )));
            }
            Some(ty) if matches!(ty.base, BaseType::List(_)) => {
                return Err(fail(format!(
                    "the URL variable {parameter} has a list type"
                )));
            }
            Some(_) => {}
        }
    }
    let mut roles = BTreeSet::new();
    let mut rejections = BTreeMap::new();
    for (role, schema) in schemas {
        let mut request = GraphQLRequest::new(endpoint.query.clone()).only_introspection();
        request.operation_name = operation_name.clone();
        let response = schema.execute(request).await;
        if response.errors.is_empty() {
            roles.insert(role.clone());
        } else {
            rejections.insert(role.clone(), response.errors[0].message.clone());
        }
    }
    if roles.is_empty() {
        return Err(fail(format!(
            "no role can execute the document: {}",
            rejections
                .iter()
                .map(|(role, message)| format!("{role}: {message}"))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    Ok(Prepared {
        name: endpoint.name.clone(),
        query: endpoint.query.clone(),
        operation_name,
        variables,
        roles,
    })
}

fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    let body = json!({
        "data": null,
        "errors": [{"message": message.into(), "extensions": {"code": code}}],
    });
    (status, axum::Json(body)).into_response()
}

fn bad_request(message: impl Into<String>) -> Response {
    error(StatusCode::BAD_REQUEST, "bad-request", message)
}

fn convert(name: &str, ty: &Type, text: &str) -> Result<Json, String> {
    let BaseType::Named(base) = &ty.base else {
        return Err(format!(
            "the variable {name} has a list type and cannot be given in the URL or the query string"
        ));
    };
    match base.as_str() {
        "Boolean" if text.is_empty() => return Ok(Json::Bool(true)),
        _ if text.is_empty() && ty.nullable => return Ok(Json::Null),
        _ => {}
    }
    match base.as_str() {
        "Boolean" => match text {
            "true" => Ok(Json::Bool(true)),
            "false" => Ok(Json::Bool(false)),
            _ => Err(format!(
                "the variable {name} expects a Boolean, not {text:?}"
            )),
        },
        "Int" | "Float" => match serde_json::from_str::<Json>(text) {
            Ok(Json::Number(number)) => Ok(Json::Number(number)),
            _ => Err(format!(
                "the variable {name} expects a number, not {text:?}"
            )),
        },
        _ => Ok(Json::String(text.to_string())),
    }
}

async fn serve<F, Fut>(
    request: Request,
    method: RestMethod,
    schemas: &HashMap<String, Schema>,
    prepared: &Prepared,
    session: F,
) -> Response
where
    F: Fn(&Parts) -> Fut,
    Fut: Future<Output = Result<rusuna::Session, Response>>,
{
    let (mut parts, body) = request.into_parts();
    let session = match session(&parts).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let schema = match schemas.get(&session.role) {
        Some(schema) if prepared.roles.contains(&session.role) => schema,
        _ => {
            return error(
                StatusCode::FORBIDDEN,
                "access-denied",
                format!(
                    "the role {} cannot call the endpoint {}",
                    session.role, prepared.name
                ),
            );
        }
    };
    let path = match RawPathParams::from_request_parts(&mut parts, &()).await {
        Ok(path) => path,
        Err(rejection) => return bad_request(rejection.body_text()),
    };
    let mut variables = Map::new();
    if method != RestMethod::Get {
        let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
            Ok(bytes) => bytes,
            Err(e) => return bad_request(format!("the request body cannot be read: {e}")),
        };
        match body_variables(&bytes) {
            Ok(values) => variables.extend(values),
            Err(message) => return bad_request(message),
        }
    }
    let query = parts.uri.query().unwrap_or_default();
    let mut texts: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    texts.extend(path.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    for (name, text) in texts {
        let Some(ty) = prepared.variables.get(&name) else {
            return bad_request(format!("unexpected variable {name}"));
        };
        match convert(&name, ty, &text) {
            Ok(value) => {
                variables.insert(name, value);
            }
            Err(message) => return bad_request(message),
        }
    }
    if let Some(name) = variables
        .keys()
        .find(|name| !prepared.variables.contains_key(*name))
    {
        return bad_request(format!("unexpected variable {name}"));
    }
    let mut request = GraphQLRequest::new(prepared.query.clone())
        .variables(Variables::from_json(Json::Object(variables)))
        .data(ClientHeaders(parts.headers.clone()))
        .data(session);
    request.operation_name = prepared.operation_name.clone();
    respond(schema.execute(request).await)
}

fn body_variables(bytes: &Bytes) -> Result<Map<String, Json>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Json>(bytes) {
        Ok(Json::Object(values)) => Ok(values),
        Ok(_) => Err("the request body must be a JSON object".into()),
        Err(e) => Err(format!("the request body is not JSON: {e}")),
    }
}

fn status(error: &ServerError) -> StatusCode {
    let code = error
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("code"))
        .and_then(|code| match code {
            async_graphql::Value::String(code) => Some(code.as_str()),
            _ => None,
        });
    match code {
        Some("access-denied") => StatusCode::FORBIDDEN,
        Some("unexpected") => StatusCode::INTERNAL_SERVER_ERROR,
        Some("remote-schema-error") => StatusCode::BAD_GATEWAY,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn respond(response: GraphQLBody) -> Response {
    let mut out = if let Some(first) = response.errors.first() {
        let status = status(first);
        let body = serde_json::to_value(&response).unwrap_or(Json::Null);
        (status, axum::Json(body)).into_response()
    } else {
        let data = response.data.into_json().unwrap_or(Json::Null);
        (StatusCode::OK, axum::Json(data)).into_response()
    };
    out.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    out
}
