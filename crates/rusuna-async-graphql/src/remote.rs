use super::*;
use async_graphql::dynamic::EnumItem;
use async_graphql::parser::types::TypeDefinition;
use rusuna_http_client::http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

/// The time a remote schema has to answer, unless [`RemoteSchema::timeout`] sets another.
pub const REMOTE_TIMEOUT: Duration = Duration::from_secs(60);

const INTROSPECTION: &str = "query IntrospectionQuery { __schema { queryType { name } mutationType { name } subscriptionType { name } types { kind name description fields(includeDeprecated: true) { name description args { name description type { ...TypeRef } defaultValue } type { ...TypeRef } } inputFields { name description type { ...TypeRef } defaultValue } enumValues(includeDeprecated: true) { name description } possibleTypes { name } } } } fragment TypeRef on __Type { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } } }";

/// The HTTP headers of the client request, which a remote schema forwards by name.
///
/// `rusuna-axum` puts them in the request data of every operation. Callers that execute
/// requests themselves add them with `Request::data`.
#[derive(Debug, Clone, Default)]
pub struct ClientHeaders(pub HeaderMap);

/// Another GraphQL server whose query and mutation root fields join the schema.
///
/// Its types come from SDL ([`RemoteSchema::sdl`]) or from introspecting the server
/// ([`RemoteSchema::introspect`]). An operation on one of its root fields is sent to the
/// server as a document of that root field alone.
#[derive(Debug, Clone)]
pub struct RemoteSchema {
    name: String,
    url: rusuna_http_client::Uri,
    headers: HeaderMap,
    forward: Vec<HeaderName>,
    roles: BTreeSet<String>,
    type_prefix: String,
    root_prefix: String,
    timeout: Duration,
    sdl: Option<String>,
}

impl RemoteSchema {
    /// A remote schema served at `url`, an `http` or `https` URL, visible to `admin`.
    pub fn new(name: impl Into<String>, url: &str) -> Result<Self, Error> {
        let name = name.into();
        let invalid = |message: String| Error::Remote {
            name: name.clone(),
            message,
        };
        let uri: rusuna_http_client::Uri = url
            .parse()
            .map_err(|e| invalid(format!("invalid URL {url:?}: {e}")))?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
            return Err(invalid(format!(
                "invalid URL {url:?}: use an http or https URL"
            )));
        }
        Ok(Self {
            name,
            url: uri,
            headers: HeaderMap::new(),
            forward: Vec::new(),
            roles: BTreeSet::from(["admin".to_string()]),
            type_prefix: String::new(),
            root_prefix: String::new(),
            timeout: REMOTE_TIMEOUT,
            sdl: None,
        })
    }

    fn invalid(&self, message: String) -> Error {
        Error::Remote {
            name: self.name.clone(),
            message,
        }
    }

    /// Sends a fixed header with every request, including the introspection request.
    pub fn header(mut self, name: &str, value: &str) -> Result<Self, Error> {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| self.invalid(format!("invalid header {name}: {e}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|e| self.invalid(format!("invalid header {name}: {e}")))?;
        self.headers.append(header, value);
        Ok(self)
    }

    /// Forwards the client request's header `name`, when present, from [`ClientHeaders`].
    pub fn forward_header(mut self, name: &str) -> Result<Self, Error> {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| self.invalid(format!("invalid header {name}: {e}")))?;
        self.forward.push(header);
        Ok(self)
    }

    /// The roles whose schemas include the remote root fields; `admin` by default.
    pub fn roles<I, S>(mut self, roles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.roles = roles.into_iter().map(Into::into).collect();
        self
    }

    /// Prefixes the name of every remote type except the built-in scalars.
    pub fn type_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.type_prefix = prefix.into();
        self
    }

    /// Prefixes the name of every remote root field.
    pub fn root_field_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.root_prefix = prefix.into();
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Takes the remote types from SDL.
    pub fn sdl(mut self, sdl: impl Into<String>) -> Self {
        self.sdl = Some(sdl.into());
        self
    }

    /// Takes the remote types from the server's introspection, sent with the fixed headers.
    pub async fn introspect(self) -> Result<Self, Error> {
        let client = rusuna_http_client::Client::new();
        let body = serde_json::json!({ "query": INTROSPECTION }).to_string();
        let response = tokio::time::timeout(
            self.timeout,
            client.post_json(&self.url, &self.headers, body),
        )
        .await
        .map_err(|_| self.invalid("the introspection request timed out".into()))?
        .map_err(|e| self.invalid(format!("the introspection request failed: {e}")))?;
        let json: Json = serde_json::from_slice(&response.body).map_err(|e| {
            self.invalid(format!(
                "the introspection answer ({}) is not JSON: {e}",
                response.status
            ))
        })?;
        if let Some(error) = json["errors"].as_array().and_then(|e| e.first()) {
            return Err(self.invalid(format!("the introspection failed: {error}")));
        }
        let sdl = introspection_sdl(&json["data"]["__schema"]).map_err(|e| self.invalid(e))?;
        Ok(self.sdl(sdl))
    }

    pub(crate) fn serves(&self, role: &str) -> bool {
        self.roles.contains(role)
    }
}

fn type_text(ty: &Json) -> Result<String, String> {
    match ty["kind"].as_str() {
        Some("NON_NULL") => Ok(format!("{}!", type_text(&ty["ofType"])?)),
        Some("LIST") => Ok(format!("[{}]", type_text(&ty["ofType"])?)),
        Some(_) => ty["name"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "a named type has no name".to_string()),
        None => Err("the introspection has an incomplete type reference".into()),
    }
}

fn description(value: &Json) -> String {
    value
        .as_str()
        .map(|d| format!("{} ", Json::String(d.to_string())))
        .unwrap_or_default()
}

fn input_values(values: &Json) -> Result<Vec<String>, String> {
    values
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|value| {
            let default = value["defaultValue"]
                .as_str()
                .map(|d| format!(" = {d}"))
                .unwrap_or_default();
            Ok(format!(
                "{}{}: {}{default}",
                description(&value["description"]),
                value["name"].as_str().unwrap_or_default(),
                type_text(&value["type"])?
            ))
        })
        .collect()
}

fn introspection_sdl(schema: &Json) -> Result<String, String> {
    let root = |key: &str| schema[key]["name"].as_str().map(str::to_string);
    let query = root("queryType").ok_or("the introspection has no query type")?;
    let mut sdl = format!("schema {{ query: {query}");
    if let Some(mutation) = root("mutationType") {
        sdl.push_str(&format!(" mutation: {mutation}"));
    }
    if let Some(subscription) = root("subscriptionType") {
        sdl.push_str(&format!(" subscription: {subscription}"));
    }
    sdl.push_str(" }\n");
    for ty in schema["types"]
        .as_array()
        .ok_or("the introspection has no types")?
    {
        let name = ty["name"].as_str().unwrap_or_default();
        if name.starts_with("__") || BUILTIN_SCALARS.contains(&name) {
            continue;
        }
        let described = description(&ty["description"]);
        let body = |entries: Vec<String>| {
            if entries.is_empty() {
                String::new()
            } else {
                format!(" {{ {} }}", entries.join(" "))
            }
        };
        let definition = match ty["kind"].as_str() {
            Some("SCALAR") => format!("scalar {name}"),
            Some(kind @ ("OBJECT" | "INTERFACE")) => {
                let fields = ty["fields"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|field| {
                        let args = input_values(&field["args"])?;
                        let args = if args.is_empty() {
                            String::new()
                        } else {
                            format!("({})", args.join(", "))
                        };
                        Ok(format!(
                            "{}{}{args}: {}",
                            description(&field["description"]),
                            field["name"].as_str().unwrap_or_default(),
                            type_text(&field["type"])?
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let keyword = if kind == "OBJECT" {
                    "type"
                } else {
                    "interface"
                };
                format!("{keyword} {name}{}", body(fields))
            }
            Some("UNION") => format!(
                "union {name} = {}",
                ty["possibleTypes"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|t| t["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            Some("ENUM") => format!(
                "enum {name}{}",
                body(
                    ty["enumValues"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .map(|v| format!(
                            "{}{}",
                            description(&v["description"]),
                            v["name"].as_str().unwrap_or_default()
                        ))
                        .collect()
                )
            ),
            Some("INPUT_OBJECT") => {
                format!("input {name}{}", body(input_values(&ty["inputFields"])?))
            }
            _ => return Err(format!("the type {name} has an unknown kind")),
        };
        sdl.push_str(&format!("{described}{definition}\n"));
    }
    Ok(sdl)
}

struct Remote {
    name: String,
    url: rusuna_http_client::Uri,
    headers: HeaderMap,
    forward: Vec<HeaderName>,
    timeout: Duration,
    catalog: Catalog,
    client: rusuna_http_client::Client,
}

pub(crate) struct Attached {
    pub query: Vec<Field>,
    pub mutation: Vec<Field>,
    pub types: Vec<async_graphql::dynamic::Type>,
}

pub(crate) struct Taken {
    pub types: BTreeSet<String>,
    pub query: BTreeSet<String>,
    pub mutation: BTreeSet<String>,
}

fn local_type(prefix: &str, name: &str) -> String {
    if BUILTIN_SCALARS.contains(&name) {
        name.to_string()
    } else {
        format!("{prefix}{name}")
    }
}

fn local_ref(prefix: &str, ty: &Type) -> TypeRef {
    let base = match &ty.base {
        BaseType::Named(name) => TypeRef::Named(local_type(prefix, name).into()),
        BaseType::List(inner) => TypeRef::List(Box::new(local_ref(prefix, inner))),
    };
    if ty.nullable {
        base
    } else {
        TypeRef::NonNull(Box::new(base))
    }
}

pub(crate) fn attach(
    remotes: Vec<RemoteSchema>,
    shared: &Arc<Shared>,
    taken: &mut Taken,
) -> Result<Attached, Error> {
    let mut attached = Attached {
        query: Vec::new(),
        mutation: Vec::new(),
        types: Vec::new(),
    };
    for remote in remotes {
        let fail = |message: String| Error::Remote {
            name: remote.name.clone(),
            message,
        };
        let sdl = remote.sdl.as_deref().ok_or_else(|| {
            fail("the remote schema has no SDL; call sdl or introspect first".into())
        })?;
        let document = async_graphql::parser::parse_schema(sdl).map_err(|e| fail(e.to_string()))?;
        let mut roots = (None, None);
        let mut definitions: BTreeMap<String, TypeDefinition> = BTreeMap::new();
        for definition in document.definitions {
            match definition {
                TypeSystemDefinition::Schema(schema) => {
                    roots = (
                        schema.node.query.map(|q| q.node.to_string()),
                        schema.node.mutation.map(|m| m.node.to_string()),
                    );
                }
                TypeSystemDefinition::Type(ty) if !ty.node.extend => {
                    definitions.insert(ty.node.name.node.to_string(), ty.node);
                }
                TypeSystemDefinition::Type(ty) => {
                    return Err(fail(format!(
                        "type extensions are unsupported ({})",
                        ty.node.name.node
                    )));
                }
                TypeSystemDefinition::Directive(_) => {}
            }
        }
        let query_root = roots.0.unwrap_or_else(|| "Query".into());
        let mutation_root = roots.1.unwrap_or_else(|| "Mutation".into());
        let prefix = remote.type_prefix.as_str();
        let mut reachable = BTreeSet::new();
        let mut pending: Vec<String> = [&query_root, &mutation_root]
            .into_iter()
            .filter(|root| definitions.contains_key(*root))
            .cloned()
            .collect();
        if pending.is_empty() {
            return Err(fail(format!(
                "the remote schema has no root type {query_root}"
            )));
        }
        while let Some(name) = pending.pop() {
            if BUILTIN_SCALARS.contains(&name.as_str()) || !reachable.insert(name.clone()) {
                continue;
            }
            let definition = definitions
                .get(&name)
                .ok_or_else(|| fail(format!("the type {name} is not defined")))?;
            match &definition.kind {
                TypeKind::Object(object) => {
                    for field in &object.fields {
                        pending.push(named(&field.node.ty.node).to_string());
                        pending.extend(
                            field
                                .node
                                .arguments
                                .iter()
                                .map(|a| named(&a.node.ty.node).to_string()),
                        );
                    }
                }
                TypeKind::InputObject(input) => pending.extend(
                    input
                        .fields
                        .iter()
                        .map(|f| named(&f.node.ty.node).to_string()),
                ),
                TypeKind::Interface(_) | TypeKind::Union(_) => {
                    return Err(fail(format!(
                        "the type {name} is an interface or a union, which remote schemas do not support"
                    )));
                }
                TypeKind::Scalar | TypeKind::Enum(_) => {}
            }
        }
        let is_root = |name: &str| name == query_root || name == mutation_root;
        let mut catalog = Catalog::default();
        let local_parent = |name: &str| {
            if name == query_root {
                shared.query.clone()
            } else if name == mutation_root {
                shared.mutation.clone()
            } else {
                local_type(prefix, name)
            }
        };
        for name in &reachable {
            if is_root(name) {
                continue;
            }
            let local = local_type(prefix, name);
            if !taken.types.insert(local.clone()) {
                return Err(fail(format!(
                    "the type {local} collides with another type; set a type prefix"
                )));
            }
        }
        for name in &reachable {
            let TypeKind::Object(object) = &definitions[name].kind else {
                continue;
            };
            for field in &object.fields {
                let field = &field.node;
                let field_name = if is_root(name) {
                    format!("{}{}", remote.root_prefix, field.name.node)
                } else {
                    field.name.node.to_string()
                };
                let key = (local_parent(name), field_name);
                catalog.arguments.insert(
                    key.clone(),
                    field
                        .arguments
                        .iter()
                        .map(|a| (a.node.name.node.to_string(), a.node.ty.node.to_string()))
                        .collect(),
                );
                let ty = named(&field.ty.node);
                if matches!(
                    definitions.get(ty).map(|d| &d.kind),
                    Some(TypeKind::Object(_))
                ) {
                    catalog.children.insert(key, local_type(prefix, ty));
                }
            }
        }
        let state = Arc::new(Remote {
            name: remote.name.clone(),
            url: remote.url.clone(),
            headers: remote.headers.clone(),
            forward: remote.forward.clone(),
            timeout: remote.timeout,
            catalog,
            client: rusuna_http_client::Client::new(),
        });
        for name in &reachable {
            let definition = &definitions[name];
            let local = local_type(prefix, name);
            let described = definition.description.as_ref().map(|d| d.node.clone());
            match &definition.kind {
                TypeKind::Object(object) if is_root(name) => {
                    let mutation = name == &mutation_root;
                    for field in &object.fields {
                        let field = &field.node;
                        let local_name = format!("{}{}", remote.root_prefix, field.name.node);
                        let roots = if mutation {
                            &mut taken.mutation
                        } else {
                            &mut taken.query
                        };
                        if !roots.insert(local_name.clone()) {
                            return Err(fail(format!(
                                "the root field {local_name} collides with another root field; set a root field prefix"
                            )));
                        }
                        let operation = if mutation {
                            Operation::Mutation
                        } else {
                            Operation::Query
                        };
                        let target = field.name.node.to_string();
                        let mut out = Field::new(local_name, local_ref(prefix, &field.ty.node), {
                            let state = state.clone();
                            let shared = shared.clone();
                            move |ctx| {
                                let state = state.clone();
                                let shared = shared.clone();
                                let target = target.clone();
                                FieldFuture::new(async move {
                                    forward(&state, &shared, operation, &target, &ctx).await
                                })
                            }
                        });
                        for argument in &field.arguments {
                            out = out.argument(input_value(
                                &argument.node,
                                local_ref(prefix, &argument.node.ty.node),
                            ));
                        }
                        if let Some(description) = &field.description {
                            out = out.description(description.node.clone());
                        }
                        if mutation {
                            attached.mutation.push(out);
                        } else {
                            attached.query.push(out);
                        }
                    }
                }
                TypeKind::Object(object) => {
                    let mut out = Object::new(&local);
                    for field in &object.fields {
                        let field = &field.node;
                        let shape = if definitions
                            .get(named(&field.ty.node))
                            .is_some_and(|d| matches!(d.kind, TypeKind::Object(_)))
                        {
                            Shape::Object
                        } else {
                            Shape::Scalar
                        };
                        let mut child = Field::new(
                            field.name.node.to_string(),
                            local_ref(prefix, &field.ty.node),
                            move |ctx| FieldFuture::Value(child_value(&ctx, shape)),
                        );
                        for argument in &field.arguments {
                            child = child.argument(input_value(
                                &argument.node,
                                local_ref(prefix, &argument.node.ty.node),
                            ));
                        }
                        if let Some(description) = &field.description {
                            child = child.description(description.node.clone());
                        }
                        out = out.field(child);
                    }
                    if let Some(description) = described {
                        out = out.description(description);
                    }
                    attached.types.push(out.into());
                }
                TypeKind::InputObject(input) => {
                    let mut out = InputObject::new(&local);
                    for field in &input.fields {
                        out = out.field(input_value(
                            &field.node,
                            local_ref(prefix, &field.node.ty.node),
                        ));
                    }
                    if let Some(description) = described {
                        out = out.description(description);
                    }
                    attached.types.push(out.into());
                }
                TypeKind::Enum(enumeration) => {
                    let mut out = Enum::new(&local);
                    for value in &enumeration.values {
                        let mut item = EnumItem::new(value.node.value.node.to_string());
                        if let Some(description) = &value.node.description {
                            item = item.description(description.node.clone());
                        }
                        out = out.item(item);
                    }
                    if let Some(description) = described {
                        out = out.description(description);
                    }
                    attached.types.push(out.into());
                }
                TypeKind::Scalar => {
                    let mut out = Scalar::new(&local);
                    if let Some(description) = described {
                        out = out.description(description);
                    }
                    attached.types.push(out.into());
                }
                TypeKind::Interface(_) | TypeKind::Union(_) => unreachable!(),
            }
        }
    }
    Ok(attached)
}

fn remote_error(remote: &Remote, message: String) -> async_graphql::Error {
    coded(
        format!("the remote schema {} {message}", remote.name),
        "remote-schema-error",
    )
}

async fn request(
    remote: &Remote,
    shared: &Shared,
    operation: Operation,
    target: &str,
    ctx: &ResolverContext<'_>,
) -> async_graphql::Result<Option<FieldValue<'static>>> {
    let (parent, keyword) = match operation {
        Operation::Mutation => (shared.mutation.as_str(), "mutation"),
        _ => (shared.query.as_str(), "query"),
    };
    let field = ctx.ctx.field();
    let key = field.alias().unwrap_or(field.name()).to_string();
    let child = remote
        .catalog
        .children
        .get(&(parent.to_string(), field.name().to_string()))
        .map(String::as_str);
    let mut document = Document {
        catalog: &remote.catalog,
        text: String::new(),
        definitions: Vec::new(),
        variables: serde_json::Map::new(),
        omit_null: true,
        has_table_rest: false,
    };
    document.field(parent, &field, child, Some(target))?;
    let definitions = if document.definitions.is_empty() {
        String::new()
    } else {
        format!("({})", document.definitions.join(", "))
    };
    let body = serde_json::json!({
        "query": format!("{keyword}{definitions} {{ {} }}", document.text),
        "variables": Json::Object(document.variables),
    });
    let mut headers = remote.headers.clone();
    if let Ok(ClientHeaders(client)) = ctx.data::<ClientHeaders>() {
        for name in &remote.forward {
            for value in client.get_all(name) {
                headers.append(name.clone(), value.clone());
            }
        }
    }
    let response = tokio::time::timeout(
        remote.timeout,
        remote
            .client
            .post_json(&remote.url, &headers, body.to_string()),
    )
    .await
    .map_err(|_| remote_error(remote, format!("did not answer in {:?}", remote.timeout)))?
    .map_err(|e| remote_error(remote, format!("is unreachable: {e}")))?;
    let json: Json = serde_json::from_slice(&response.body).map_err(|_| {
        remote_error(
            remote,
            format!("answered {} without a GraphQL response", response.status),
        )
    })?;
    if let Some(errors) = json
        .get("errors")
        .and_then(Json::as_array)
        .filter(|errors| !errors.is_empty())
    {
        let message = errors[0]["message"]
            .as_str()
            .unwrap_or("returned an error")
            .to_string();
        let remote_errors = scalar_value(&Json::Array(errors.clone()));
        return Err(async_graphql::Error::new(message).extend_with(|_, e| {
            e.set("code", "remote-schema-error");
            e.set("remote_errors", remote_errors.clone());
        }));
    }
    if !response.status.is_success() {
        return Err(remote_error(
            remote,
            format!("answered {}", response.status),
        ));
    }
    match json {
        Json::Object(mut object) => match object.remove("data") {
            Some(Json::Object(mut data)) => {
                let value = data.remove(&key).unwrap_or(Json::Null);
                Ok(match child {
                    Some(_) => owned(value),
                    None => (!value.is_null()).then(|| FieldValue::value(scalar_value(&value))),
                })
            }
            _ => Err(remote_error(remote, "answered without data".into())),
        },
        _ => Err(remote_error(remote, "answered without data".into())),
    }
}

async fn forward(
    remote: &Remote,
    shared: &Shared,
    operation: Operation,
    target: &str,
    ctx: &ResolverContext<'_>,
) -> async_graphql::Result<Option<FieldValue<'static>>> {
    session(shared, ctx)?;
    if operation != Operation::Mutation {
        return request(remote, shared, operation, target, ctx).await;
    }
    ctx.data::<Arc<Slot>>()?
        .resolve(ctx, async |_| {
            request(remote, shared, operation, target, ctx).await
        })
        .await
}
