use super::*;
use async_graphql::extensions::{NextResolve, ResolveInfo};
use async_graphql::parser::types::{InputObjectType, OperationType};
use async_graphql::{PathSegment, QueryPathNode};
use async_graphql_value::Value as InputSyntax;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rusuna::{BodyTemplate, RestField, RestMethod, Template, TemplatePart};
use rusuna_http_client::http::{
    HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header::CONTENT_TYPE,
};
use rusuna_http_client::{Client, Limits, Uri};
use std::collections::HashMap;
use std::sync::Mutex as SyncMutex;
use std::time::Duration;
use tokio::sync::{OnceCell, Semaphore};

mod relation;

const ENCODE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Clone)]
pub struct RestSource {
    name: String,
    url: Uri,
    fixed_headers: HeaderMap,
    session_headers: BTreeMap<String, (HeaderName, Template)>,
    forward: Vec<HeaderName>,
    limits: Limits,
    permits: Arc<Semaphore>,
    retry: (usize, Duration),
    expose_error_body: bool,
    client: Client,
}

impl RestSource {
    pub fn new(name: impl Into<String>, url: &str) -> Result<Self, Error> {
        let name = name.into();
        let uri: Uri = url
            .parse()
            .map_err(|_| source_error(&name, "invalid base URL"))?;
        if name.is_empty()
            || !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.host().is_none_or(str::is_empty)
            || uri.query().is_some()
            || url.contains('#')
            || uri
                .authority()
                .is_some_and(|authority| authority.as_str().contains('@'))
        {
            return Err(source_error(
                &name,
                "expected a name and an HTTP(S) base URL",
            ));
        }
        Ok(Self {
            name,
            url: uri,
            fixed_headers: HeaderMap::new(),
            session_headers: BTreeMap::new(),
            forward: Vec::new(),
            limits: Limits {
                timeout: REMOTE_TIMEOUT,
                max_body_bytes: 16 * 1024 * 1024,
            },
            permits: Arc::new(Semaphore::new(8)),
            retry: (0, Duration::ZERO),
            expose_error_body: false,
            client: Client::new(),
        })
    }

    fn header_name(&self, name: &str) -> Result<HeaderName, Error> {
        HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| source_error(&self.name, "invalid header name"))
    }

    pub fn header(mut self, name: &str, value: &str) -> Result<Self, Error> {
        let name = self.header_name(name)?;
        let mut value = HeaderValue::from_str(value)
            .map_err(|_| source_error(&self.name, "invalid fixed header value"))?;
        value.set_sensitive(true);
        self.fixed_headers.insert(name, value);
        Ok(self)
    }

    pub fn session_header(mut self, name: &str, template: &str) -> Result<Self, Error> {
        let name = self.header_name(name)?;
        let invalid = |message| source_error(&self.name, message);
        HeaderValue::from_str(template).map_err(|_| invalid("invalid session header template"))?;
        let template = Template::parse(template).map_err(invalid)?;
        if template
            .0
            .iter()
            .any(|part| matches!(part, TemplatePart::Field(_)))
        {
            return Err(invalid(
                "session header templates use only session variables",
            ));
        }
        self.session_headers
            .insert(name.to_string(), (name, template));
        Ok(self)
    }

    pub fn forward_header(mut self, name: &str) -> Result<Self, Error> {
        let name = self.header_name(name)?;
        if !self.forward.contains(&name) {
            self.forward.push(name);
        }
        Ok(self)
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.limits.timeout = timeout;
        self
    }

    pub fn max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.limits.max_body_bytes = max_body_bytes;
        self
    }

    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.permits = Arc::new(Semaphore::new(concurrency.clamp(1, Semaphore::MAX_PERMITS)));
        self
    }

    pub fn retry(mut self, times: usize, delay: Duration) -> Self {
        self.retry = (times, delay);
        self
    }

    pub fn expose_error_body(mut self, expose: bool) -> Self {
        self.expose_error_body = expose;
        self
    }

    fn headers(
        &self,
        session: &rusuna::Session,
        client: Option<&ClientHeaders>,
    ) -> Result<HeaderMap, async_graphql::Error> {
        let mut headers = self.fixed_headers.clone();
        for (name, template) in self.session_headers.values() {
            if headers.contains_key(name) {
                continue;
            }
            let value = render(template, |_| Ok(Json::Null), session, Render::Header)?;
            let mut value = HeaderValue::from_str(&value[0])
                .map_err(|_| invalid("invalid session header value"))?;
            value.set_sensitive(true);
            headers.insert(name.clone(), value);
        }
        if let Some(client) = client {
            for name in &self.forward {
                if !headers.contains_key(name) {
                    for value in client.0.get_all(name) {
                        headers.append(name.clone(), value.clone());
                    }
                }
            }
        }
        Ok(headers)
    }

    fn uri(
        &self,
        field: &RestField,
        value: impl Fn(&str) -> Result<Json, async_graphql::Error>,
        session: &rusuna::Session,
    ) -> Result<Uri, async_graphql::Error> {
        let mut path = self.url.path().trim_end_matches('/').to_string();
        for segment in &field.path {
            path.push('/');
            path.push_str(&render(segment, &value, session, Render::Path)?[0]);
        }
        let mut query = '?';
        for (name, template) in &field.query {
            for value in render(template, &value, session, Render::Query)? {
                path.push(query);
                query = '&';
                path.push_str(name);
                path.push('=');
                path.push_str(&utf8_percent_encode(&value, ENCODE).to_string());
            }
        }
        let mut parts = self.url.clone().into_parts();
        parts.path_and_query = Some(
            path.parse()
                .map_err(|_| invalid("invalid REST request path"))?,
        );
        Uri::from_parts(parts).map_err(|_| invalid("invalid REST request URL"))
    }

    async fn request(
        &self,
        method: Method,
        uri: &Uri,
        headers: &HeaderMap,
        mut body: Option<Vec<u8>>,
    ) -> Upstream {
        use rusuna_http_client::Error as HttpError;
        let (times, delay) = if method == Method::GET {
            self.retry
        } else {
            (0, Duration::ZERO)
        };
        let mut retries = 0;
        loop {
            let permit = self
                .permits
                .acquire()
                .await
                .expect("REST source semaphore is open");
            let response = self
                .client
                .request(
                    method.clone(),
                    uri,
                    headers,
                    body.take().map(Into::into),
                    self.limits,
                )
                .await;
            drop(permit);
            let retryable = match &response {
                Ok(response) => matches!(response.status.as_u16(), 429 | 502 | 503 | 504),
                Err(error) => matches!(error, HttpError::Connection(_)),
            };
            if retryable && retries < times {
                retries += 1;
                tokio::time::sleep(delay).await;
                continue;
            }
            return match response {
                Err(HttpError::Timeout) => Upstream::Failed("timeout"),
                Err(HttpError::TooLarge) => Upstream::Failed("too-large"),
                Err(HttpError::InvalidHeader) => Upstream::InvalidHeader,
                Err(_) => Upstream::Failed("connection"),
                Ok(response) if !response.status.is_success() => Upstream::Http {
                    status: response.status,
                    body: {
                        let prefix = &response.body[..response.body.len().min(1024)];
                        let mut body = String::from_utf8_lossy(prefix).into_owned();
                        if body.len() > 1024 {
                            body.truncate(body.floor_char_boundary(1024));
                        }
                        body
                    },
                },
                Ok(response)
                    if response.status == StatusCode::NO_CONTENT || response.body.is_empty() =>
                {
                    Upstream::Empty
                }
                Ok(response) if is_json(&response.headers) => {
                    serde_json::from_slice(&response.body)
                        .map(Upstream::Json)
                        .unwrap_or(Upstream::Failed("unexpected-response"))
                }
                Ok(_) => Upstream::Failed("unexpected-response"),
            };
        }
    }
}

fn source_error(name: &str, message: &str) -> Error {
    Error::Rest {
        name: name.into(),
        message: message.into(),
    }
}

fn invalid(message: impl Into<String>) -> async_graphql::Error {
    coded(message, "validation-failed")
}

#[derive(Clone, Copy)]
enum Render {
    Path,
    Query,
    Header,
}

fn render(
    template: &Template,
    field: impl Fn(&str) -> Result<Json, async_graphql::Error>,
    session: &rusuna::Session,
    mode: Render,
) -> Result<Vec<String>, async_graphql::Error> {
    let mut pieces = Vec::new();
    let mut list = None;
    let mut has_null = false;
    for part in &template.0 {
        let value = match part {
            TemplatePart::Literal(text) => {
                pieces.push(text.clone());
                continue;
            }
            TemplatePart::Field(name) => field(name)?,
            TemplatePart::Session(name) => session_value(session, name)?,
        };
        let text = match (mode, value) {
            (Render::Path, value) => {
                if value.is_null() || value.is_array() {
                    return Err(invalid("a REST path marker requires a single value"));
                }
                let text = as_text(value);
                if matches!(text.as_str(), "" | "." | "..") || text.contains('/') {
                    return Err(invalid("invalid REST path segment"));
                }
                utf8_percent_encode(&text, ENCODE).to_string()
            }
            (Render::Header, Json::Null) => return Err(invalid("a session header value is null")),
            (Render::Query, Json::Null) => {
                has_null = true;
                String::new()
            }
            (Render::Query, Json::Array(values)) => {
                if list.replace((pieces.len(), values)).is_some() {
                    return Err(invalid("a REST query template can repeat one list"));
                }
                String::new()
            }
            (_, value) => as_text(value),
        };
        pieces.push(text);
    }
    if has_null {
        return Ok(Vec::new());
    }
    if let Some((index, values)) = list {
        let prefix = pieces[..index].concat();
        let suffix = pieces[index + 1..].concat();
        Ok(values
            .into_iter()
            .filter(|value| !value.is_null())
            .map(|value| format!("{prefix}{}{suffix}", as_text(value)))
            .collect())
    } else {
        Ok(vec![pieces.concat()])
    }
}

fn session_value(session: &rusuna::Session, name: &str) -> Result<Json, async_graphql::Error> {
    match session.variables.get(name) {
        Some(rusuna::Value::Null) => Ok(Json::Null),
        Some(rusuna::Value::Text(value)) => Ok(Json::String(value.clone())),
        Some(rusuna::Value::Int(value)) => Ok(Json::from(*value)),
        Some(rusuna::Value::Float(value)) if value.is_finite() => Ok(Json::from(*value)),
        Some(rusuna::Value::Bool(value)) => Ok(Json::from(*value)),
        Some(_) => Err(invalid("invalid REST session value")),
        None => Err(invalid(format!("missing session variable `{name}`"))),
    }
}

fn body_value(
    template: &BodyTemplate,
    field: &impl Fn(&str) -> Result<Json, async_graphql::Error>,
    session: &rusuna::Session,
) -> Result<Json, async_graphql::Error> {
    match template {
        BodyTemplate::Literal(value) => Ok(value.clone()),
        BodyTemplate::Field(name) => field(name),
        BodyTemplate::Session(name) => session_value(session, name),
        BodyTemplate::Array(values) => values
            .iter()
            .map(|value| body_value(value, field, session))
            .collect::<Result<_, _>>()
            .map(Json::Array),
        BodyTemplate::Object(values) => body_object(values, field, session).map(Json::Object),
    }
}

fn body_object(
    template: &BTreeMap<String, BodyTemplate>,
    field: &impl Fn(&str) -> Result<Json, async_graphql::Error>,
    session: &rusuna::Session,
) -> Result<serde_json::Map<String, Json>, async_graphql::Error> {
    template
        .iter()
        .map(|(name, value)| Ok((name.clone(), body_value(value, field, session)?)))
        .collect()
}

fn as_text(value: Json) -> String {
    match value {
        Json::String(value) => value,
        value => value.to_string(),
    }
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let media = value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            media == "application/json"
                || media.rsplit_once('/').is_some_and(|(kind, subtype)| {
                    !kind.is_empty() && subtype.len() > 5 && subtype.ends_with("+json")
                })
        })
}

#[derive(PartialEq, Eq, Hash)]
struct Call {
    limits: Limits,
    retry: (usize, Duration),
    uri: Uri,
    headers: Vec<(HeaderName, Vec<HeaderValue>)>,
}

impl Call {
    fn new(source: &RestSource, uri: Uri, headers: &HeaderMap) -> Self {
        let mut keys: Vec<_> = headers.keys().collect();
        keys.sort_unstable_by_key(|name| name.as_str());
        Self {
            limits: source.limits,
            retry: source.retry,
            uri,
            headers: keys
                .into_iter()
                .map(|name| {
                    (
                        name.clone(),
                        headers.get_all(name).into_iter().cloned().collect(),
                    )
                })
                .collect(),
        }
    }
}

enum Upstream {
    Json(Json),
    Empty,
    Http { status: StatusCode, body: String },
    Failed(&'static str),
    InvalidHeader,
}

fn unexpected() -> async_graphql::Error {
    failure("unexpected-response")
}

fn failure(reason: &'static str) -> async_graphql::Error {
    coded(
        "the REST source response could not be used",
        "rest-source-error",
    )
    .extend_with(|_, e| e.set("reason", reason))
}

fn status_error(status: StatusCode, body: Option<&str>) -> async_graphql::Error {
    coded(
        "the REST source returned an unsuccessful status",
        "rest-source-error",
    )
    .extend_with(|_, e| {
        let mut http = BTreeMap::from([(Name::new("status"), Value::from(status.as_u16()))]);
        if let Some(body) = body {
            http.insert(Name::new("body"), Value::from(body));
        }
        e.set("http", Value::Object(http.into_iter().collect()));
    })
}

type Calls = HashMap<Call, Arc<OnceCell<Upstream>>>;

#[derive(Default)]
struct RestRequest {
    calls: Mutex<Calls>,
    null_paths: SyncMutex<BTreeSet<String>>,
    provided_variables: BTreeSet<Name>,
}

impl RestRequest {
    fn null_at(&self, path: String) {
        self.null_paths.lock().unwrap().insert(path);
    }

    fn is_null(&self, path: &str) -> bool {
        self.null_paths.lock().unwrap().contains(path)
    }
}

pub(super) struct RestExecution;

impl ExtensionFactory for RestExecution {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(Self)
    }
}

#[async_trait::async_trait]
impl Extension for RestExecution {
    async fn prepare_request(
        &self,
        ctx: &ExtensionContext<'_>,
        request: Request,
        next: NextPrepareRequest<'_>,
    ) -> ServerResult<Request> {
        let state = RestRequest {
            provided_variables: request.variables.keys().cloned().collect(),
            ..RestRequest::default()
        };
        next.run(ctx, request.data(state)).await
    }

    async fn resolve(
        &self,
        ctx: &ExtensionContext<'_>,
        info: ResolveInfo<'_>,
        next: NextResolve<'_>,
    ) -> ServerResult<Option<Value>> {
        let Some(state) = ctx.data_opt::<RestRequest>() else {
            return next.run(ctx, info).await;
        };
        let path = *info.path_node;
        let key = path.to_string();
        let required = info.return_type.ends_with('!');
        let value = if state.is_null(&key) {
            Ok(None)
        } else {
            next.run(ctx, info).await
        };
        if state.is_null(&key) {
            if required {
                state.null_at(path_key(path.parent));
            }
            return Ok(Some(Value::Null));
        }
        value
    }

    async fn execute(
        &self,
        ctx: &ExtensionContext<'_>,
        name: Option<&str>,
        next: NextExecute<'_>,
    ) -> Response {
        let mut response = next.run(ctx, name).await;
        if ctx
            .data_opt::<RestRequest>()
            .is_some_and(|state| state.is_null(""))
        {
            response.data = Value::Null;
        }
        response
    }
}

fn path_key(path: Option<&QueryPathNode<'_>>) -> String {
    path.map(ToString::to_string).unwrap_or_default()
}

#[derive(Default)]
pub(super) struct RestRuntime {
    sources: BTreeMap<String, RestSource>,
    inputs: BTreeMap<String, InputObjectType>,
    enums: BTreeMap<String, BTreeSet<String>>,
    multiplexed: relation::MultiplexedCache,
}

struct RestRow {
    value: Json,
    relations: Arc<relation::Outcomes>,
}

impl RestRuntime {
    pub(super) fn new(
        schema: &rusuna::Schema,
        sources: Vec<RestSource>,
        types: &BTreeMap<String, TypeKind>,
    ) -> Result<Self, Error> {
        let mut runtime = Self::default();
        for source in sources {
            let name = source.name.clone();
            if runtime.sources.insert(name.clone(), source).is_some() {
                return Err(source_error(&name, "source name is repeated"));
            }
        }
        for field in schema.rest_catalog().fields.values() {
            if !runtime.sources.contains_key(&field.source) {
                return Err(source_error(
                    &field.source,
                    "source is referenced in SDL and needs Builder::rest",
                ));
            }
        }
        if schema.rest_catalog().fields.is_empty() {
            return Ok(runtime);
        }
        for (name, kind) in types {
            match kind {
                TypeKind::InputObject(input) => {
                    runtime.inputs.insert(name.clone(), input.clone());
                }
                TypeKind::Enum(values) => {
                    runtime.enums.insert(
                        name.clone(),
                        values
                            .values
                            .iter()
                            .map(|value| value.node.value.node.to_string())
                            .collect(),
                    );
                }
                _ => {}
            }
        }
        Ok(runtime)
    }

    pub(super) fn field(
        shared: &Arc<Shared>,
        owner: &str,
        definition: &FieldDefinition,
    ) -> Option<Field> {
        let catalog = shared.schema.rest_catalog();
        let name = definition.name.node.to_string();
        let call = catalog.fields.get(&(owner.into(), name.clone()));
        let key = catalog.objects.get(owner).and_then(|keys| keys.get(&name));
        let relation = catalog
            .relations
            .get(&(owner.into(), name.clone()))
            .map(|relation| relation.list);
        if call.is_none() && key.is_none() && relation.is_none() {
            return None;
        }
        let mutation = call.is_some_and(|call| call.method != RestMethod::Get);
        let shared = shared.clone();
        let owner = owner.to_string();
        let spec = Arc::new(definition.clone());
        let key = key.cloned();
        let mut field = Field::new(name, type_ref(&definition.ty.node), move |ctx| {
            let shared = shared.clone();
            let spec = spec.clone();
            let key = key.clone();
            let owner = owner.clone();
            FieldFuture::new(async move {
                let state = ctx.data::<RestRequest>()?;
                if let Some(list) = relation {
                    let row = ctx.parent_value.try_downcast_ref::<RestRow>()?;
                    return match row.relations.get(&path_key(ctx.path_node.as_ref())) {
                        Some(Ok(value)) => Ok(borrowed(value, Shape::Object)),
                        Some(Err(error)) => {
                            report(&ctx, error.clone(), &[]);
                            state.null_at(path_key(ctx.path_node.as_ref()));
                            Ok(None)
                        }
                        None if list => Ok(Some(FieldValue::list([] as [FieldValue<'_>; 0]))),
                        None => Ok(None),
                    };
                }
                if let Some(key) = key {
                    let row = ctx.parent_value.try_downcast_ref::<RestRow>()?;
                    return Ok(shared.rest.output(
                        &shared,
                        &ctx,
                        state,
                        &spec.ty.node,
                        row.value.get(&key.key).unwrap_or(&Json::Null),
                        row.relations.clone(),
                    ));
                }
                let result = if mutation {
                    ctx.data::<Arc<Slot>>()?
                        .resolve(&ctx, async |slot| {
                            shared
                                .rest
                                .call(&shared, &owner, &spec, &ctx, state, Some(slot))
                                .await
                        })
                        .await
                } else {
                    shared
                        .rest
                        .call(&shared, &owner, &spec, &ctx, state, None)
                        .await
                };
                match result {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        report(&ctx, error, &[]);
                        state.null_at(path_key(ctx.path_node.as_ref()));
                        Ok(None)
                    }
                }
            })
        });
        for argument in &definition.arguments {
            field = field.argument(input_value(
                &argument.node,
                type_ref(&argument.node.ty.node),
            ));
        }
        Some(field)
    }

    async fn call(
        &self,
        shared: &Shared,
        owner: &str,
        definition: &FieldDefinition,
        ctx: &ResolverContext<'_>,
        state: &RestRequest,
        transaction_state: Option<&mut State>,
    ) -> async_graphql::Result<Option<FieldValue<'static>>> {
        let catalog = shared.schema.rest_catalog();
        let field = &catalog.fields[&(owner.into(), definition.name.node.to_string())];
        let source = &self.sources[&field.source];
        let session = session(shared, ctx)?;
        let omitted = |syntax: Option<&InputSyntax>| {
            matches!(syntax, Some(InputSyntax::Variable(name)) if !state.provided_variables.contains(name)
                && ctx.query_env.operation.node.variable_definitions.iter().any(|definition|
                    &definition.node.name.node == name && definition.node.default_value.is_none()))
        };
        let argument_value = |argument: &InputValueDefinition| {
            let name = argument.name.node.as_str();
            let syntax = ctx.item.node.get_argument(name).map(|value| &value.node);
            let supplied = if omitted(syntax) {
                argument.default_value.as_ref().map(|value| &value.node)
            } else {
                ctx.args.get(name).map(|value| value.as_value())
            };
            supplied
                .map(|value| self.argument(&argument.ty.node, value, syntax, &omitted, 0))
                .transpose()
        };
        let value = |name: &str| {
            if let Some(keys) = catalog.objects.get(owner) {
                let column = &keys[name];
                let value = ctx
                    .parent_value
                    .downcast_ref::<RestRow>()
                    .and_then(|parent| parent.value.get(&column.key))
                    .unwrap_or(&Json::Null);
                if value.is_null() {
                    return Ok(Json::Null);
                }
                return self
                    .scalar(named(&column.ty), value)
                    .ok_or_else(unexpected)?
                    .into_json()
                    .map_err(|_| unexpected());
            }
            let argument = definition
                .arguments
                .iter()
                .find(|arg| arg.node.name.node == name)
                .unwrap();
            Ok(argument_value(&argument.node)?.unwrap_or(Json::Null))
        };
        let uri = source.uri(field, value, session)?;
        let mut headers = source.headers(session, ctx.data_opt::<ClientHeaders>())?;
        let upstream;
        let cached;
        let response = if field.method == RestMethod::Get {
            let key = Call::new(source, uri.clone(), &headers);
            cached = state.calls.lock().await.entry(key).or_default().clone();
            cached
                .get_or_init(|| source.request(Method::GET, &uri, &headers, None))
                .await
        } else {
            let body = match &field.body {
                Some(template) => body_object(template, &value, session)?,
                None => definition
                    .arguments
                    .iter()
                    .filter_map(|argument| {
                        argument_value(&argument.node).transpose().map(|value| {
                            value.map(|value| (argument.node.name.node.to_string(), value))
                        })
                    })
                    .collect::<Result<_, _>>()?,
            };
            let body = serde_json::to_vec(&body).map_err(|_| invalid("invalid REST body"))?;
            headers
                .entry(CONTENT_TYPE)
                .or_insert(HeaderValue::from_static("application/json"));
            let method = match field.method {
                RestMethod::Post => Method::POST,
                RestMethod::Put => Method::PUT,
                RestMethod::Patch => Method::PATCH,
                RestMethod::Delete => Method::DELETE,
                RestMethod::Get => unreachable!(),
            };
            upstream = source.request(method, &uri, &headers, Some(body)).await;
            &upstream
        };
        let ty = &definition.ty.node;
        let value = match response {
            Upstream::Empty if ty.nullable => return Ok(None),
            Upstream::Empty if matches!(&ty.base, BaseType::Named(name) if name == "Boolean") => {
                return Ok(Some(FieldValue::value(true)));
            }
            Upstream::Empty => return Err(unexpected()),
            Upstream::Http {
                status: StatusCode::NOT_FOUND,
                ..
            } if field.not_found_null => return Ok(None),
            Upstream::Http { status, body } => {
                return Err(status_error(
                    *status,
                    source.expose_error_body.then_some(body.as_str()),
                ));
            }
            Upstream::Failed(reason) => return Err(failure(reason)),
            Upstream::InvalidHeader => return Err(invalid("invalid REST request header")),
            Upstream::Json(json) => field
                .result
                .iter()
                .try_fold(json, |value, key| value.get(key).ok_or_else(unexpected))?,
        };
        let mut slot;
        let connection = if let Some(transaction_state) = transaction_state {
            transaction_state.transaction.as_deref_mut()
        } else if ctx.query_env.operation.node.ty == OperationType::Mutation
            && !catalog.relations.is_empty()
        {
            slot = ctx.data::<Arc<Slot>>()?.state.lock().await;
            slot.transaction.as_deref_mut()
        } else {
            None
        };
        let relations = relation::collect(shared, ctx, ty, value, session, connection).await;
        Ok(self.output(shared, ctx, state, ty, value, Arc::new(relations)))
    }

    fn argument(
        &self,
        ty: &Type,
        value: &Value,
        syntax: Option<&InputSyntax>,
        omitted: &impl Fn(Option<&InputSyntax>) -> bool,
        depth: usize,
    ) -> Result<Json, async_graphql::Error> {
        if depth > rusuna::MAX_DOCUMENT_DEPTH {
            return Err(invalid("REST argument nesting limit exceeded"));
        }
        if *value == Value::Null {
            return Ok(Json::Null);
        }
        if let BaseType::List(inner) = &ty.base {
            let values = match value {
                Value::List(values) => values.as_slice(),
                value => std::slice::from_ref(value),
            };
            return values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let item = match syntax {
                        Some(InputSyntax::List(items)) => items.get(index),
                        syntax if index == 0 => syntax,
                        _ => None,
                    };
                    self.argument(inner, value, item, omitted, depth + 1)
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Json::Array);
        }
        if let Some(input) = self.inputs.get(named(ty))
            && let Value::Object(values) = value
        {
            return input
                .fields
                .iter()
                .filter_map(|field| {
                    let field = &field.node;
                    let name = &field.name.node;
                    let syntax = match syntax {
                        Some(InputSyntax::Object(fields)) => fields.get(name),
                        _ => None,
                    };
                    let provided = values.get(name).filter(|_| !omitted(syntax));
                    let value = provided
                        .or_else(|| field.default_value.as_ref().map(|default| &default.node));
                    value.map(|value| {
                        self.argument(&field.ty.node, value, syntax, omitted, depth + 1)
                            .map(|value| (name.to_string(), value))
                    })
                })
                .collect::<Result<serde_json::Map<_, _>, _>>()
                .map(Json::Object);
        }
        value
            .clone()
            .into_json()
            .map_err(|_| invalid("invalid REST argument value"))
    }

    fn scalar(&self, name: &str, value: &Json) -> Option<Value> {
        if let Some(values) = self.enums.get(name) {
            return value
                .as_str()
                .filter(|key| values.contains(*key))
                .map(|key| Value::Enum(Name::new(key)));
        }
        match name {
            "Int" => value
                .as_f64()
                .filter(|num| {
                    num.fract() == 0.0 && *num >= f64::from(i32::MIN) && *num <= f64::from(i32::MAX)
                })
                .map(|num| Value::from(num as i32)),
            "Float" if value.is_number() => Some(scalar_value(value)),
            "String" | "DateTime" => value.as_str().map(Value::from),
            "Boolean" => value.as_bool().map(Value::from),
            "ID" => value.as_str().map(Value::from).or_else(|| {
                let number = value.as_number()?;
                let id = if number.is_i64() || number.is_u64() {
                    number.to_string()
                } else {
                    format!("{:.0}", number.as_f64().filter(|num| num.fract() == 0.0)?)
                };
                Some(Value::from(id))
            }),
            "JSON" => Some(scalar_value(value)),
            _ => None,
        }
    }

    fn output(
        &self,
        shared: &Shared,
        ctx: &ResolverContext<'_>,
        state: &RestRequest,
        ty: &Type,
        value: &Json,
        relations: Arc<relation::Outcomes>,
    ) -> Option<FieldValue<'static>> {
        let mut output = Output {
            rest: self,
            catalog: shared.schema.rest_catalog(),
            ctx,
            state,
            indices: Vec::new(),
            relations,
        };
        match output.convert(ty, value) {
            Ok(value) => value,
            Err(()) => {
                state.null_at(path_key(ctx.path_node.as_ref()));
                None
            }
        }
    }
}

struct Output<'a, 'ctx> {
    rest: &'a RestRuntime,
    catalog: &'a rusuna::RestCatalog,
    ctx: &'a ResolverContext<'ctx>,
    state: &'a RestRequest,
    indices: Vec<usize>,
    relations: Arc<relation::Outcomes>,
}

impl Output<'_, '_> {
    fn convert(&mut self, ty: &Type, value: &Json) -> Result<Option<FieldValue<'static>>, ()> {
        let converted = if value.is_null() {
            if ty.nullable {
                return Ok(None);
            }
            Err(unexpected())
        } else {
            match &ty.base {
                BaseType::Named(name) => self.named(name, value),
                BaseType::List(inner) => {
                    if let Some(items) = value.as_array() {
                        let mut values = Vec::with_capacity(items.len());
                        for (index, item) in items.iter().enumerate() {
                            self.indices.push(index);
                            let result = self.convert(inner, item);
                            let value = match result {
                                Ok(Some(value)) => value,
                                Ok(None) => {
                                    let mut path = path_key(self.ctx.path_node.as_ref());
                                    for index in &self.indices {
                                        write!(path, ".{index}").unwrap();
                                    }
                                    self.state.null_at(path);
                                    FieldValue::NULL
                                }
                                Err(()) => {
                                    self.indices.pop();
                                    return if ty.nullable { Ok(None) } else { Err(()) };
                                }
                            };
                            self.indices.pop();
                            values.push(value);
                        }
                        return Ok(Some(FieldValue::list(values)));
                    }
                    Err(unexpected())
                }
            }
        };
        match converted {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                report(self.ctx, error, &self.indices);
                if ty.nullable { Ok(None) } else { Err(()) }
            }
        }
    }

    fn named(&self, name: &str, value: &Json) -> Result<FieldValue<'static>, async_graphql::Error> {
        if self.catalog.objects.contains_key(name) {
            return value
                .is_object()
                .then(|| {
                    FieldValue::owned_any(RestRow {
                        value: value.clone(),
                        relations: self.relations.clone(),
                    })
                })
                .ok_or_else(unexpected);
        }
        self.rest
            .scalar(name, value)
            .map(FieldValue::value)
            .ok_or_else(unexpected)
    }
}

fn report(ctx: &ResolverContext<'_>, error: async_graphql::Error, indices: &[usize]) {
    let mut error = ctx.set_error_path(error.into_server_error(ctx.item.pos));
    error
        .path
        .extend(indices.iter().copied().map(PathSegment::Index));
    ctx.add_error(error);
}
