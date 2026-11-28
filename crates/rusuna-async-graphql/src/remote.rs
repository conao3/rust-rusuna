use super::*;
use async_graphql::dynamic::EnumItem;
use async_graphql::parser::types::{FieldDefinition, TypeDefinition};
use rusuna::RestRemoteRelation;
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

    pub(crate) fn name(&self) -> &str {
        &self.name
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
    pub relations: BTreeMap<(String, String), Relation>,
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
    relations: &BTreeMap<(String, String), RestRemoteRelation>,
    role: &str,
    (query, mutation): (&str, &str),
    taken: &mut Taken,
) -> Result<Attached, Error> {
    let mut attached = Attached {
        query: Vec::new(),
        mutation: Vec::new(),
        types: Vec::new(),
        relations: BTreeMap::new(),
    };
    let mut pending_relations = BTreeMap::<_, Vec<_>>::new();
    for (key, relation) in relations {
        pending_relations
            .entry(relation.schema.as_str())
            .or_default()
            .push((key, relation));
    }
    let mut matched = BTreeSet::new();
    let role: Arc<str> = role.into();
    for remote in remotes {
        let fail = |message: String| Error::Remote {
            name: remote.name.clone(),
            message,
        };
        let serves = remote.serves(&role);
        let referenced = pending_relations.remove(remote.name.as_str());
        if matched.contains(&remote.name) {
            return Err(fail(
                "the name used by @remote_relation is ambiguous".into(),
            ));
        }
        if referenced.is_some() {
            matched.insert(remote.name.clone());
        } else if !serves {
            continue;
        }
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
                query.to_string()
            } else if name == mutation_root {
                mutation.to_string()
            } else {
                local_type(prefix, name)
            }
        };
        for name in reachable.iter().filter(|_| serves) {
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
        if let Some(referenced) = referenced {
            let query_fields = match definitions.get(&query_root).map(|d| &d.kind) {
                Some(TypeKind::Object(object)) => object.fields.as_slice(),
                _ => &[],
            };
            for (key, spec) in referenced {
                let field = query_fields
                    .iter()
                    .map(|field| &field.node)
                    .find(|field| field.name.node == spec.field.as_str())
                    .ok_or_else(|| {
                        fail(format!(
                            "unknown query field {} in @remote_relation",
                            spec.field
                        ))
                    })?;
                let output = named(&field.ty.node);
                let kind = definitions
                    .get(output)
                    .filter(|_| !is_root(output))
                    .map(|d| &d.kind);
                let relation =
                    Relation::bind(state.clone(), prefix, spec, field, kind).map_err(fail)?;
                if serves {
                    attached.relations.insert(key.clone(), relation);
                }
            }
        }
        if !serves {
            continue;
        }
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
                        let local_parent_name = local_parent(name);
                        let mut out = Field::new(local_name, local_ref(prefix, &field.ty.node), {
                            let state = state.clone();
                            let role = role.clone();
                            move |ctx| {
                                let state = state.clone();
                                let role = role.clone();
                                let target = target.clone();
                                let local_parent_name = local_parent_name.clone();
                                FieldFuture::new(async move {
                                    forward(
                                        &state,
                                        &role,
                                        operation,
                                        &target,
                                        &local_parent_name,
                                        &ctx,
                                    )
                                    .await
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
    if let Some((name, _)) = pending_relations.first_key_value() {
        return Err(Error::Remote {
            name: (*name).into(),
            message: "schema is referenced by @remote_relation and needs Builder::remote".into(),
        });
    }
    Ok(attached)
}

fn remote_error(remote: &Remote, message: String) -> async_graphql::Error {
    coded(
        format!("the remote schema {} {message}", remote.name),
        "remote-schema-error",
    )
}

pub(super) type Outcome = async_graphql::Result<Arc<Json>>;

pub(crate) struct Relation {
    remote: Arc<Remote>,
    target: String,
    arguments: Vec<(String, String)>,
    child: Option<String>,
}

impl Relation {
    fn bind(
        remote: Arc<Remote>,
        prefix: &str,
        spec: &RestRemoteRelation,
        field: &FieldDefinition,
        kind: Option<&TypeKind>,
    ) -> Result<Self, String> {
        let ty = &field.ty.node;
        let output = local_type(prefix, named(ty));
        if !matches!(
            kind,
            Some(TypeKind::Object(_) | TypeKind::Enum(_) | TypeKind::Scalar)
        ) || output != named(&spec.output)
            || matches!(ty.base, BaseType::List(_)) != matches!(spec.output.base, BaseType::List(_))
            || matches!(&ty.base, BaseType::List(inner) if matches!(inner.base, BaseType::List(_)))
        {
            return Err(format!(
                "@remote_relation to {} requires the imported type {}",
                spec.field,
                local_ref(prefix, ty),
            ));
        }
        for argument in &field.arguments {
            let argument = &argument.node;
            if !argument.ty.node.nullable
                && argument.default_value.is_none()
                && !spec.arguments.contains_key(argument.name.node.as_str())
            {
                return Err(format!(
                    "@remote_relation to {} has no value for required argument {}",
                    spec.field, argument.name.node
                ));
            }
        }
        let arguments = spec
            .arguments
            .keys()
            .map(|name| {
                let argument = field
                    .arguments
                    .iter()
                    .find(|argument| argument.node.name.node == name.as_str())
                    .ok_or_else(|| {
                        format!("unknown argument {}.{name} in @remote_relation", spec.field)
                    })?;
                Ok((name.clone(), argument.node.ty.node.to_string()))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            remote,
            target: spec.field.clone(),
            arguments,
            child: matches!(kind, Some(TypeKind::Object(_))).then_some(output),
        })
    }

    pub(super) fn shape(&self) -> Shape {
        if self.child.is_some() {
            Shape::Object
        } else {
            Shape::Scalar
        }
    }

    pub(super) async fn resolve(
        &self,
        ctx: &ResolverContext<'_>,
        fields: &[SelectionField<'_>],
        keys: &[Vec<Json>],
    ) -> Vec<Outcome> {
        let mut results = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(100) {
            match self.batch(ctx, fields, chunk).await {
                Ok(batch) => results.extend(batch),
                Err(error) => results.extend(std::iter::repeat_n(Err(error), chunk.len())),
            }
        }
        results
    }

    async fn batch(
        &self,
        ctx: &ResolverContext<'_>,
        fields: &[SelectionField<'_>],
        keys: &[Vec<Json>],
    ) -> async_graphql::Result<Vec<Outcome>> {
        let mut document = self.remote.document();
        if let Some(child) = &self.child {
            document.selection_body(child, fields)?;
        }
        let selection = std::mem::take(&mut document.text);
        let aliases: Vec<_> = (0..keys.len()).map(|index| format!("r{index}")).collect();
        for (alias, values) in aliases.iter().zip(keys) {
            write!(document.text, " {alias}: {}", self.target)?;
            if !values.is_empty() {
                document.text.push('(');
                for (index, ((name, ty), value)) in self.arguments.iter().zip(values).enumerate() {
                    if index > 0 {
                        document.text.push(' ');
                    }
                    document.argument(name, ty, value.clone())?;
                }
                document.text.push(')');
            }
            document.text.push_str(&selection);
        }

        let mut reply = self.remote.execute(ctx, document, "query").await?;
        let mut errors = BTreeMap::<_, Vec<_>>::new();
        let mut global = Vec::new();
        for error in reply.errors {
            if let Some(alias) = error["path"][0]
                .as_str()
                .filter(|alias| aliases.iter().any(|name| name == alias))
            {
                errors.entry(alias.to_owned()).or_default().push(error);
            } else {
                global.push(error);
            }
        }
        if !global.is_empty() {
            return Err(graphql_error(global));
        }
        Ok(aliases
            .into_iter()
            .map(|alias| {
                if let Some(errors) = errors.remove(&alias) {
                    return Err(graphql_error(errors));
                }
                reply
                    .data
                    .as_mut()
                    .and_then(|data| data.remove(&alias))
                    .map(Arc::new)
                    .ok_or_else(|| {
                        remote_error(&self.remote, format!("answered without data for {alias}"))
                    })
            })
            .collect())
    }
}

struct Reply {
    data: Option<serde_json::Map<String, Json>>,
    errors: Vec<Json>,
}

fn graphql_error(errors: Vec<Json>) -> async_graphql::Error {
    let message = errors[0]["message"]
        .as_str()
        .unwrap_or("returned an error")
        .to_string();
    async_graphql::Error::new(message).extend_with(|_, e| {
        e.set("code", "remote-schema-error");
        e.set("remote_errors", scalar_value(&Json::Array(errors)));
    })
}

impl Remote {
    fn document(&self) -> Document<'_> {
        Document {
            catalog: &self.catalog,
            text: String::new(),
            definitions: Vec::new(),
            variables: serde_json::Map::new(),
            omit_null: true,
            has_table_rest: false,
        }
    }

    async fn execute(
        &self,
        ctx: &ResolverContext<'_>,
        document: Document<'_>,
        keyword: &str,
    ) -> async_graphql::Result<Reply> {
        let (query, variables) = document.finish(keyword);
        let body = serde_json::json!({ "query": query, "variables": variables });
        let mut headers = self.headers.clone();
        if let Ok(ClientHeaders(client)) = ctx.data::<ClientHeaders>() {
            for name in &self.forward {
                for value in client.get_all(name) {
                    headers.append(name.clone(), value.clone());
                }
            }
        }
        let response = tokio::time::timeout(
            self.timeout,
            self.client.post_json(&self.url, &headers, body.to_string()),
        )
        .await
        .map_err(|_| remote_error(self, format!("did not answer in {:?}", self.timeout)))?
        .map_err(|e| remote_error(self, format!("is unreachable: {e}")))?;
        let mut json: Json = serde_json::from_slice(&response.body).map_err(|_| {
            remote_error(
                self,
                format!("answered {} without a GraphQL response", response.status),
            )
        })?;
        let errors = match json.get_mut("errors").map(Json::take) {
            Some(Json::Array(errors)) => errors,
            _ => Vec::new(),
        };
        let data = match json.get_mut("data").map(Json::take) {
            Some(Json::Object(data)) => Some(data),
            _ => None,
        };
        if errors.is_empty() {
            if !response.status.is_success() {
                return Err(remote_error(self, format!("answered {}", response.status)));
            }
            if data.is_none() {
                return Err(remote_error(self, "answered without data".into()));
            }
        }
        Ok(Reply { data, errors })
    }
}

async fn request(
    remote: &Remote,
    operation: Operation,
    target: &str,
    parent: &str,
    ctx: &ResolverContext<'_>,
) -> async_graphql::Result<Option<FieldValue<'static>>> {
    let keyword = if operation == Operation::Mutation {
        "mutation"
    } else {
        "query"
    };
    let field = ctx.ctx.field();
    let key = field.alias().unwrap_or(field.name());
    let child = remote
        .catalog
        .children
        .get(&(parent.to_string(), field.name().to_string()))
        .map(String::as_str);
    let mut document = remote.document();
    document.field(parent, &field, child, Some(target))?;
    let mut reply = remote.execute(ctx, document, keyword).await?;
    if !reply.errors.is_empty() {
        return Err(graphql_error(reply.errors));
    }
    let value = reply
        .data
        .as_mut()
        .and_then(|data| data.remove(key))
        .unwrap_or(Json::Null);
    Ok(match child {
        Some(_) => owned(value),
        None => (!value.is_null()).then(|| FieldValue::value(scalar_value(&value))),
    })
}

async fn forward(
    remote: &Remote,
    role: &str,
    operation: Operation,
    target: &str,
    parent: &str,
    ctx: &ResolverContext<'_>,
) -> async_graphql::Result<Option<FieldValue<'static>>> {
    session(role, ctx)?;
    if operation != Operation::Mutation {
        return request(remote, operation, target, parent, ctx).await;
    }
    ctx.data::<Arc<Slot>>()?
        .resolve(ctx, async |_| {
            request(remote, operation, target, parent, ctx).await
        })
        .await
}
