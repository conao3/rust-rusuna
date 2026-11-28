//! Builds an async-graphql dynamic schema from a [`rusuna::Schema`] for one role.
//!
//! [`Builder`] produces [`Parts`]: the types, input types and root fields of the role,
//! which share one schema with hand-written dynamic fields. SQL root fields resolve with
//! one PostgreSQL statement, and each subscription root field subscribes to that statement
//! on a `rusuna_sqlx::Subscriptions`. The resolvers read a [`rusuna::Session`] from the
//! request or connection data and a `sqlx::PgPool` and a `rusuna_sqlx::Subscriptions` from
//! the schema data.

use async_graphql::dynamic::{
    Enum, Field, FieldFuture, FieldValue, InputObject, InputValue, Object, ResolverContext, Scalar,
    SchemaBuilder, Subscription, SubscriptionField, SubscriptionFieldFuture, TypeRef,
};
use async_graphql::extensions::{
    Extension, ExtensionContext, ExtensionFactory, NextExecute, NextPrepareRequest,
};
use async_graphql::parser::types::{
    BaseType, DocumentOperations, FieldDefinition, InputValueDefinition, Type, TypeKind,
    TypeSystemDefinition,
};
use async_graphql::{
    Data, ErrorExtensions, Name, Request, Response, SelectionField, ServerError, ServerResult,
    Value,
};
use futures_util::Stream;
use serde_json::Value as Json;
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write};
use std::sync::Arc;
use tokio::sync::Mutex;

mod remote;
mod rest_source;

pub use remote::{ClientHeaders, REMOTE_TIMEOUT, RemoteSchema};
pub use rest_source::RestSource;

pub const HIDDEN_PREFIX: &str = "__rusuna_";

const BUILTIN_SCALARS: [&str; 5] = ["Int", "Float", "String", "Boolean", "ID"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Schema(rusuna::Error),
    Sdl(String),
    Field {
        type_name: String,
        field: String,
        message: String,
    },
    /// A remote schema that cannot be added: an invalid definition or SDL, an unsupported
    /// type or a name collision.
    Remote {
        name: String,
        message: String,
    },
    Rest {
        name: String,
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schema(error) => write!(f, "{error}"),
            Self::Sdl(message) => write!(f, "expanded SDL: {message}"),
            Self::Field {
                type_name,
                field,
                message,
            } => write!(f, "{type_name}.{field}: {message}"),
            Self::Remote { name, message } => write!(f, "remote schema {name}: {message}"),
            Self::Rest { name, message } => write!(f, "REST source {name}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

struct Extra {
    type_name: String,
    name: String,
    field: Field,
    requires: Vec<String>,
}

pub struct Builder {
    schema: Arc<rusuna::Schema>,
    role: String,
    extras: Vec<Extra>,
    remotes: Vec<RemoteSchema>,
    rest: Vec<RestSource>,
}

#[derive(Clone, Copy)]
enum Shape {
    Scalar,
    Object,
}

#[derive(Default)]
struct Catalog {
    children: BTreeMap<(String, String), String>,
    arguments: BTreeMap<(String, String), BTreeMap<String, String>>,
    extras: BTreeMap<(String, String), Vec<String>>,
}

struct Shared {
    schema: Arc<rusuna::Schema>,
    role: String,
    query: String,
    mutation: String,
    subscription: String,
    catalog: Catalog,
    rest: rest_source::RestRuntime,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Query,
    Mutation,
    Subscription,
}

#[derive(Clone)]
struct Root {
    definition: FieldDefinition,
    operation: Operation,
}

pub struct Parts {
    query: Object,
    query_name: String,
    mutation: Option<Object>,
    mutation_name: String,
    subscription: Option<Subscription>,
    subscription_name: String,
    types: Vec<async_graphql::dynamic::Type>,
    roots: BTreeMap<String, Root>,
    shared: Arc<Shared>,
}

impl Builder {
    pub fn new(schema: Arc<rusuna::Schema>, role: impl Into<String>) -> Self {
        Self {
            schema,
            role: role.into(),
            extras: Vec::new(),
            remotes: Vec::new(),
            rest: Vec::new(),
        }
    }

    /// Adds the query and mutation root fields of a remote schema, when it serves the role.
    pub fn remote(mut self, remote: RemoteSchema) -> Self {
        self.remotes.push(remote);
        self
    }

    pub fn rest(mut self, source: RestSource) -> Self {
        self.rest.push(source);
        self
    }

    pub fn field<F>(
        mut self,
        type_name: impl Into<String>,
        name: impl Into<String>,
        ty: impl Into<TypeRef>,
        requires: &[&str],
        resolver: F,
    ) -> Self
    where
        F: for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync + 'static,
    {
        let name = name.into();
        let field = Field::new(name.clone(), ty, resolver);
        self.extras.push(Extra {
            type_name: type_name.into(),
            field,
            name,
            requires: requires.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    pub fn build(self) -> Result<Parts, Error> {
        let sdl = self
            .schema
            .expanded_sdl(&self.role)
            .map_err(Error::Schema)?;
        let document =
            async_graphql::parser::parse_schema(&sdl).map_err(|e| Error::Sdl(e.to_string()))?;
        let mut query_name = "Query".to_string();
        let mut mutation_name = "Mutation".to_string();
        let mut subscription_name = "Subscription".to_string();
        let mut definitions = BTreeMap::new();
        for definition in document.definitions {
            match definition {
                TypeSystemDefinition::Schema(schema) => {
                    if let Some(query) = &schema.node.query {
                        query_name = query.node.to_string();
                    }
                    if let Some(mutation) = &schema.node.mutation {
                        mutation_name = mutation.node.to_string();
                    }
                    if let Some(subscription) = &schema.node.subscription {
                        subscription_name = subscription.node.to_string();
                    }
                }
                TypeSystemDefinition::Type(ty) => {
                    definitions.insert(ty.node.name.node.to_string(), ty.node.kind);
                }
                TypeSystemDefinition::Directive(_) => {
                    return Err(Error::Sdl("unexpected directive definition".into()));
                }
            }
        }
        prune(&mut definitions);
        let mut children = BTreeMap::new();
        let mut arguments = BTreeMap::new();
        let mut scalars = BTreeSet::new();
        for (name, kind) in &definitions {
            if let TypeKind::Object(object) = kind {
                for field in &object.fields {
                    let field = &field.node;
                    arguments.insert(
                        (name.clone(), field.name.node.to_string()),
                        field
                            .arguments
                            .iter()
                            .map(|a| (a.node.name.node.to_string(), a.node.ty.node.to_string()))
                            .collect::<BTreeMap<_, _>>(),
                    );
                    let ty = named(&field.ty.node);
                    let shape = match definitions.get(ty) {
                        Some(TypeKind::Object(_)) => Shape::Object,
                        _ => Shape::Scalar,
                    };
                    if let Shape::Object = shape {
                        children
                            .insert((name.clone(), field.name.node.to_string()), ty.to_string());
                    } else {
                        scalars.insert((name.clone(), field.name.node.to_string()));
                    }
                }
            }
        }
        let rest = rest_source::RestRuntime::new(&self.schema, self.rest, &definitions)?;
        let columns = if self.extras.is_empty() {
            BTreeMap::new()
        } else {
            row_columns(&self.schema)?
        };

        let mut extras = BTreeMap::new();
        let mut extra_fields: BTreeMap<String, Vec<Field>> = BTreeMap::new();
        for extra in self.extras {
            let Some(declared) = columns.get(&extra.type_name) else {
                return Err(Error::Field {
                    type_name: extra.type_name,
                    field: extra.name,
                    message: "no row type has this name".into(),
                });
            };
            if let Some(unknown) = extra.requires.iter().find(|c| !declared.contains(*c)) {
                return Err(Error::Field {
                    type_name: extra.type_name.clone(),
                    field: extra.name.clone(),
                    message: format!("the row type has no scalar field {unknown}"),
                });
            }
            let Some(TypeKind::Object(object)) = definitions.get(&extra.type_name) else {
                continue;
            };
            if object.fields.iter().any(|f| f.node.name.node == extra.name) {
                return Err(Error::Field {
                    type_name: extra.type_name,
                    field: extra.name,
                    message: "the type already has a field with this name".into(),
                });
            }
            if let Some(long) = extra
                .requires
                .iter()
                .find(|c| HIDDEN_PREFIX.len() + c.len() > 63)
            {
                return Err(Error::Field {
                    type_name: extra.type_name.clone(),
                    field: extra.name.clone(),
                    message: format!("the required column name {long} is longer than 54 bytes"),
                });
            }
            if !extra
                .requires
                .iter()
                .all(|c| scalars.contains(&(extra.type_name.clone(), c.clone())))
            {
                continue;
            }
            extras.insert(
                (extra.type_name.clone(), extra.name.clone()),
                extra.requires,
            );
            extra_fields
                .entry(extra.type_name)
                .or_default()
                .push(extra.field);
        }
        let shared = Arc::new(Shared {
            schema: self.schema,
            rest,
            role: self.role,
            query: query_name.clone(),
            mutation: mutation_name.clone(),
            subscription: subscription_name.clone(),
            catalog: Catalog {
                children,
                arguments,
                extras,
            },
        });
        let root_names = |root: &str| -> BTreeSet<String> {
            match definitions.get(root) {
                Some(TypeKind::Object(object)) => object
                    .fields
                    .iter()
                    .map(|f| f.node.name.node.to_string())
                    .collect(),
                _ => BTreeSet::new(),
            }
        };
        let mut taken = remote::Taken {
            types: definitions.keys().cloned().collect(),
            query: root_names(&query_name),
            mutation: root_names(&mutation_name),
        };
        let attached = remote::attach(
            self.remotes
                .into_iter()
                .filter(|remote| remote.serves(&shared.role))
                .collect(),
            &shared,
            &mut taken,
        )?;
        let mut query = Object::new(&query_name);
        let mut mutation = Object::new(&mutation_name);
        let mut has_mutation = false;
        let mut subscription = Subscription::new(&subscription_name);
        let mut has_subscription = false;
        let mut types = Vec::new();
        let mut roots = BTreeMap::new();
        for (name, kind) in definitions {
            match kind {
                TypeKind::Object(object) if name == subscription_name => {
                    for field in object.fields {
                        let root = Root {
                            definition: field.node,
                            operation: Operation::Subscription,
                        };
                        has_subscription = true;
                        subscription = subscription.field(subscription_field(&shared, &root));
                    }
                }
                TypeKind::Object(object) if name == query_name || name == mutation_name => {
                    let is_mutation = name == mutation_name;
                    for field in object.fields {
                        let root = Root {
                            definition: field.node,
                            operation: if is_mutation {
                                Operation::Mutation
                            } else {
                                Operation::Query
                            },
                        };
                        let resolver = root_field(&shared, &root);
                        if is_mutation {
                            has_mutation = true;
                            mutation = mutation.field(resolver);
                        } else {
                            query = query.field(resolver);
                            roots.insert(root.definition.name.node.to_string(), root);
                        }
                    }
                }
                TypeKind::Object(object) => {
                    let mut out = Object::new(&name);
                    for field in object.fields {
                        let field = field.node;
                        if let Some(rest) = rest_source::RestRuntime::field(&shared, &name, &field)
                        {
                            out = out.field(rest);
                            continue;
                        }
                        let shape = if shared
                            .catalog
                            .children
                            .contains_key(&(name.clone(), field.name.node.to_string()))
                        {
                            Shape::Object
                        } else {
                            Shape::Scalar
                        };
                        let mut child = Field::new(
                            field.name.node.to_string(),
                            type_ref(&field.ty.node),
                            move |ctx| FieldFuture::Value(child_value(&ctx, shape)),
                        );
                        for argument in field.arguments {
                            child = child.argument(InputValue::new(
                                argument.node.name.node.to_string(),
                                type_ref(&argument.node.ty.node),
                            ));
                        }
                        out = out.field(child);
                    }
                    for extra in extra_fields.remove(&name).unwrap_or_default() {
                        out = out.field(extra);
                    }
                    types.push(out.into());
                }
                TypeKind::InputObject(input) => {
                    let mut out = InputObject::new(&name);
                    for field in input.fields {
                        out = out.field(input_value(&field.node, type_ref(&field.node.ty.node)));
                    }
                    types.push(out.into());
                }
                TypeKind::Enum(enumeration) => {
                    types.push(
                        Enum::new(&name)
                            .items(
                                enumeration
                                    .values
                                    .iter()
                                    .map(|v| v.node.value.node.to_string()),
                            )
                            .into(),
                    );
                }
                TypeKind::Scalar if !BUILTIN_SCALARS.contains(&name.as_str()) => {
                    types.push(Scalar::new(&name).into());
                }
                _ => {}
            }
        }
        for field in attached.query {
            query = query.field(field);
        }
        for field in attached.mutation {
            has_mutation = true;
            mutation = mutation.field(field);
        }
        types.extend(attached.types);
        Ok(Parts {
            query,
            query_name,
            mutation: has_mutation.then_some(mutation),
            mutation_name,
            subscription: has_subscription.then_some(subscription),
            subscription_name,
            types,
            roots,
            shared,
        })
    }
}

impl Parts {
    pub fn query_name(&self) -> &str {
        &self.query_name
    }

    pub fn mutation_name(&self) -> Option<&str> {
        self.mutation.as_ref().map(|_| self.mutation_name.as_str())
    }

    pub fn subscription_name(&self) -> Option<&str> {
        self.subscription
            .as_ref()
            .map(|_| self.subscription_name.as_str())
    }

    pub fn query_field(mut self, field: Field) -> Self {
        self.query = self.query.field(field);
        self
    }

    pub fn mutation_field(mut self, field: Field) -> Self {
        self.mutation = Some(
            self.mutation
                .take()
                .unwrap_or_else(|| Object::new(&self.mutation_name))
                .field(field),
        );
        self
    }

    pub fn subscription_field(mut self, field: SubscriptionField) -> Self {
        self.subscription = Some(
            self.subscription
                .take()
                .unwrap_or_else(|| Subscription::new(&self.subscription_name))
                .field(field),
        );
        self
    }

    pub fn root_field(&self, name: &str) -> Option<Field> {
        self.roots
            .get(name)
            .map(|root| root_field(&self.shared, root))
    }

    pub fn register(self, mut builder: SchemaBuilder) -> SchemaBuilder {
        builder = builder.register(self.query);
        if let Some(mutation) = self.mutation {
            builder = builder.register(mutation);
        }
        if let Some(subscription) = self.subscription {
            builder = builder.register(subscription);
        }
        for ty in self.types {
            builder = builder.register(ty);
        }
        if !self.shared.schema.rest_catalog().fields.is_empty() {
            builder = builder.extension(rest_source::RestExecution);
        }
        builder.extension(Transactions)
    }

    pub fn schema(self) -> SchemaBuilder {
        let builder = async_graphql::dynamic::Schema::build(
            &self.query_name,
            self.mutation_name(),
            self.subscription_name(),
        );
        self.register(builder)
    }
}

pub fn parent_column<'a>(ctx: &ResolverContext<'a>, name: &str) -> async_graphql::Result<&'a Json> {
    let parent: &'a FieldValue<'a> = ctx.parent_value;
    parent
        .downcast_ref::<Json>()
        .and_then(|row| row.get(format!("{HIDDEN_PREFIX}{name}")))
        .ok_or_else(|| {
            async_graphql::Error::new(format!(
                "the parent row has no column {name}; declare it in the field's requirements"
            ))
        })
}

fn row_columns(schema: &rusuna::Schema) -> Result<BTreeMap<String, BTreeSet<String>>, Error> {
    let sdl = schema.expanded_sdl("admin").map_err(Error::Schema)?;
    let document =
        async_graphql::parser::parse_schema(&sdl).map_err(|e| Error::Sdl(e.to_string()))?;
    let mut types = BTreeMap::new();
    for definition in &document.definitions {
        if let TypeSystemDefinition::Type(ty) = definition {
            types.insert(ty.node.name.node.to_string(), &ty.node.kind);
        }
    }
    Ok(types
        .iter()
        .filter(|(name, _)| types.contains_key(&format!("{name}_bool_exp")))
        .filter_map(|(name, kind)| match kind {
            TypeKind::Object(object) => Some((
                name.clone(),
                object
                    .fields
                    .iter()
                    .filter(|f| {
                        !matches!(types.get(named(&f.node.ty.node)), Some(TypeKind::Object(_)))
                    })
                    .map(|f| f.node.name.node.to_string())
                    .collect(),
            )),
            _ => None,
        })
        .collect())
}

fn named(ty: &Type) -> &str {
    match &ty.base {
        BaseType::Named(name) => name.as_str(),
        BaseType::List(inner) => named(inner),
    }
}

fn type_ref(ty: &Type) -> TypeRef {
    let base = match &ty.base {
        BaseType::Named(name) => TypeRef::Named(name.to_string().into()),
        BaseType::List(inner) => TypeRef::List(Box::new(type_ref(inner))),
    };
    if ty.nullable {
        base
    } else {
        TypeRef::NonNull(Box::new(base))
    }
}

fn input_value(definition: &InputValueDefinition, ty: TypeRef) -> InputValue {
    let mut value = InputValue::new(definition.name.node.to_string(), ty);
    if let Some(default) = &definition.default_value {
        value = value.default_value(default.node.clone());
    }
    if let Some(description) = &definition.description {
        value = value.description(description.node.clone());
    }
    value
}

fn prune(definitions: &mut BTreeMap<String, TypeKind>) {
    loop {
        let empty: BTreeSet<String> = definitions
            .iter()
            .filter(|(_, kind)| match kind {
                TypeKind::Object(object) => object.fields.is_empty(),
                TypeKind::InputObject(input) => input.fields.is_empty(),
                _ => false,
            })
            .map(|(name, _)| name.clone())
            .collect();
        if empty.is_empty() {
            return;
        }
        for name in &empty {
            definitions.remove(name);
        }
        for kind in definitions.values_mut() {
            match kind {
                TypeKind::Object(object) => {
                    object.fields.retain(|field| {
                        !empty.contains(named(&field.node.ty.node))
                            && field.node.arguments.iter().all(|argument| {
                                argument.node.ty.node.nullable
                                    || !empty.contains(named(&argument.node.ty.node))
                            })
                    });
                    for field in &mut object.fields {
                        field
                            .node
                            .arguments
                            .retain(|argument| !empty.contains(named(&argument.node.ty.node)));
                    }
                }
                TypeKind::InputObject(input) => {
                    input
                        .fields
                        .retain(|field| !empty.contains(named(&field.node.ty.node)));
                }
                _ => {}
            }
        }
    }
}

fn scalar_value(value: &Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(v) => Value::Boolean(*v),
        Json::Number(v) => Value::Number(v.clone()),
        Json::String(v) => Value::String(v.clone()),
        Json::Array(v) => Value::List(v.iter().map(scalar_value).collect()),
        Json::Object(v) => Value::Object(
            v.iter()
                .map(|(k, v)| (Name::new(k), scalar_value(v)))
                .collect(),
        ),
    }
}

fn borrowed(value: &Json, shape: Shape) -> Option<FieldValue<'_>> {
    match (value, shape) {
        (Json::Null, _) => None,
        (Json::Array(items), Shape::Object) => Some(FieldValue::list(
            items.iter().map(|item| FieldValue::borrowed_any(item)),
        )),
        (value, Shape::Object) => Some(FieldValue::borrowed_any(value)),
        (value, Shape::Scalar) => Some(FieldValue::value(scalar_value(value))),
    }
}

fn owned<'a>(value: Json) -> Option<FieldValue<'a>> {
    match value {
        Json::Null => None,
        Json::Array(items) => Some(FieldValue::list(
            items.into_iter().map(FieldValue::owned_any),
        )),
        value => Some(FieldValue::owned_any(value)),
    }
}

fn child_value<'a>(ctx: &ResolverContext<'a>, shape: Shape) -> Option<FieldValue<'a>> {
    let parent: &'a FieldValue<'a> = ctx.parent_value;
    let field = ctx.ctx.field();
    let key = field.alias().unwrap_or(field.name());
    parent
        .downcast_ref::<Json>()
        .and_then(|row| row.get(key))
        .and_then(|value| borrowed(value, shape))
}

fn coded(message: impl Into<String>, code: &'static str) -> async_graphql::Error {
    async_graphql::Error::new(message).extend_with(|_, e| e.set("code", code))
}

fn execution_error(error: &rusuna_sqlx::Error) -> async_graphql::Error {
    let code = match error {
        rusuna_sqlx::Error::PermissionCheck => "permission-error",
        rusuna_sqlx::Error::Parameter { .. } => "data-exception",
        rusuna_sqlx::Error::Database(sqlx::Error::Database(db)) => {
            match db.code().as_deref().map(|c| &c[..2.min(c.len())]) {
                Some("23") => "constraint-violation",
                Some("22") => "data-exception",
                _ => "unexpected",
            }
        }
        rusuna_sqlx::Error::Statement(_) | rusuna_sqlx::Error::Database(_) => "unexpected",
    };
    coded(error.to_string(), code)
}

struct Document<'a> {
    catalog: &'a Catalog,
    text: String,
    definitions: Vec<String>,
    variables: serde_json::Map<String, Json>,
    omit_null: bool,
}

impl Document<'_> {
    fn field(
        &mut self,
        parent: &str,
        field: &SelectionField<'_>,
        child: Option<&str>,
        target: Option<&str>,
    ) -> async_graphql::Result<()> {
        let name = field.name();
        match (field.alias(), target) {
            (Some(alias), Some(target)) => write!(self.text, "{alias}: {target}")?,
            (None, Some(target)) => write!(self.text, "{name}: {target}")?,
            (Some(alias), None) => write!(self.text, "{alias}: {name}")?,
            (None, None) => self.text.push_str(name),
        }
        let mut arguments = field.arguments()?;
        if self.omit_null {
            arguments.retain(|(_, value)| *value != Value::Null);
        }
        if !arguments.is_empty() {
            let types = self
                .catalog
                .arguments
                .get(&(parent.to_string(), name.to_string()));
            self.text.push('(');
            for (i, (key, value)) in arguments.into_iter().enumerate() {
                let ty = types
                    .and_then(|types| types.get(key.as_str()))
                    .ok_or_else(|| {
                        async_graphql::Error::new(format!("unknown argument {parent}.{name}.{key}"))
                    })?;
                let variable = format!("v{}", self.definitions.len());
                if i > 0 {
                    self.text.push(' ');
                }
                write!(self.text, "{key}: ${variable}")?;
                self.definitions.push(format!("${variable}: {ty}"));
                self.variables.insert(variable, value.into_json()?);
            }
            self.text.push(')');
        }
        if let Some(child) = child {
            self.text.push_str(" {");
            let start = self.text.len();
            self.selection(child, field)?;
            if self.text.len() == start {
                self.text.push_str(" __typename");
            }
            self.text.push_str(" }");
        }
        Ok(())
    }

    fn selection(&mut self, parent: &str, field: &SelectionField<'_>) -> async_graphql::Result<()> {
        for item in field.selection_set() {
            let name = item.name();
            if name == "__typename" {
                continue;
            }
            let key = (parent.to_string(), name.to_string());
            if let Some(requires) = self.catalog.extras.get(&key) {
                for column in requires {
                    write!(self.text, " {HIDDEN_PREFIX}{column}: {column}")?;
                }
                continue;
            }
            let child = self.catalog.children.get(&key).map(String::as_str);
            self.text.push(' ');
            self.field(parent, &item, child, None)?;
        }
        Ok(())
    }
}

fn render(
    shared: &Shared,
    root: &Root,
    field: &SelectionField<'_>,
) -> async_graphql::Result<(String, Json)> {
    let (parent, operation) = match root.operation {
        Operation::Query => (shared.query.as_str(), "query"),
        Operation::Mutation => (shared.mutation.as_str(), "mutation"),
        Operation::Subscription => (shared.subscription.as_str(), "subscription"),
    };
    let child = shared
        .catalog
        .children
        .get(&(parent.to_string(), root.definition.name.node.to_string()))
        .map(String::as_str);
    let mut document = Document {
        catalog: &shared.catalog,
        text: String::new(),
        definitions: Vec::new(),
        variables: serde_json::Map::new(),
        omit_null: false,
    };
    document.field(parent, field, child, None)?;
    let definitions = if document.definitions.is_empty() {
        String::new()
    } else {
        format!("({})", document.definitions.join(", "))
    };
    Ok((
        format!("{operation}{definitions} {{ {} }}", document.text),
        Json::Object(document.variables),
    ))
}

#[derive(Default)]
struct State {
    transaction: Option<Transaction<'static, Postgres>>,
    failed: bool,
    keys: BTreeSet<String>,
}

#[derive(Default)]
struct Slot {
    state: Mutex<State>,
}

impl Slot {
    async fn resolve(
        &self,
        ctx: &ResolverContext<'_>,
        run: impl AsyncFnOnce(&mut State) -> async_graphql::Result<Option<FieldValue<'static>>>,
    ) -> async_graphql::Result<Option<FieldValue<'static>>> {
        let mut state = self.state.lock().await;
        if state.failed {
            return Ok(None);
        }
        let field = ctx.ctx.field();
        let key = field.alias().unwrap_or(field.name()).to_string();
        let result = if state.keys.insert(key.clone()) {
            run(&mut state).await
        } else {
            Err(coded(
                format!("the mutation repeats the root response key {key}"),
                "validation-failed",
            ))
        };
        if result.is_err() {
            state.failed = true;
            if let Some(transaction) = state.transaction.take() {
                transaction
                    .rollback()
                    .await
                    .map_err(|e| execution_error(&e.into()))?;
            }
        }
        result
    }
}

fn session<'a>(
    shared: &Shared,
    ctx: &ResolverContext<'a>,
) -> async_graphql::Result<&'a rusuna::Session> {
    let session = ctx
        .data::<rusuna::Session>()
        .map_err(|_| coded("the request carries no rusuna::Session", "access-denied"))?;
    if session.role != shared.role {
        return Err(coded(
            format!(
                "the session role {} does not match the schema role {}",
                session.role, shared.role
            ),
            "access-denied",
        ));
    }
    Ok(session)
}

async fn subscribe<'a>(
    shared: &Shared,
    root: &Root,
    ctx: &ResolverContext<'a>,
) -> async_graphql::Result<impl Stream<Item = async_graphql::Result<FieldValue<'a>>> + use<'a>> {
    let session = session(shared, ctx)?;
    let subscriptions = ctx.data::<rusuna_sqlx::Subscriptions>()?;
    let (text, variables) = render(shared, root, &ctx.ctx.field())?;
    let field = shared
        .schema
        .compile(&text, &variables, None, session)
        .map_err(|e| coded(e.to_string(), "validation-failed"))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            coded(
                "the subscription selects no root field",
                "validation-failed",
            )
        })?;
    let subscription = subscriptions
        .subscribe(&shared.role, &field.compiled)
        .await
        .map_err(|e| execution_error(&e))?;
    Ok(futures_util::stream::unfold(
        subscription,
        |mut subscription| async move {
            let item = match subscription.next().await? {
                Ok(value) => Ok(owned(value).unwrap_or(FieldValue::NULL)),
                Err(error) => Err(execution_error(&error)),
            };
            Some((item, subscription))
        },
    ))
}

fn subscription_field(shared: &Arc<Shared>, root: &Root) -> SubscriptionField {
    let definition = &root.definition;
    let mut field = SubscriptionField::new(
        definition.name.node.to_string(),
        type_ref(&definition.ty.node),
        {
            let shared = shared.clone();
            let root = root.clone();
            move |ctx| {
                let shared = shared.clone();
                let root = root.clone();
                SubscriptionFieldFuture::new(async move { subscribe(&shared, &root, &ctx).await })
            }
        },
    );
    for argument in &definition.arguments {
        field = field.argument(InputValue::new(
            argument.node.name.node.to_string(),
            type_ref(&argument.node.ty.node),
        ));
    }
    field
}

async fn resolve_root(
    shared: &Shared,
    root: &Root,
    ctx: &ResolverContext<'_>,
) -> async_graphql::Result<Option<FieldValue<'static>>> {
    let session = session(shared, ctx)?;
    let pool = ctx.data::<PgPool>()?;
    if root.operation == Operation::Query {
        let (text, variables) = render(shared, root, &ctx.ctx.field())?;
        let fields = shared
            .schema
            .compile(&text, &variables, None, session)
            .map_err(|e| coded(e.to_string(), "validation-failed"))?;
        let Some(field) = fields.into_iter().next() else {
            return Ok(None);
        };
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| execution_error(&e.into()))?;
        let value = rusuna_sqlx::fetch(&mut conn, &field.compiled)
            .await
            .map_err(|e| execution_error(&e))?;
        return Ok(owned(value));
    }
    ctx.data::<Arc<Slot>>()?
        .resolve(ctx, async |state| {
            let (text, variables) = render(shared, root, &ctx.ctx.field())?;
            let fields = shared
                .schema
                .compile(&text, &variables, None, session)
                .map_err(|e| coded(e.to_string(), "validation-failed"))?;
            let Some(field) = fields.into_iter().next() else {
                return Ok(None);
            };
            if state.transaction.is_none() {
                state.transaction =
                    Some(pool.begin().await.map_err(|e| execution_error(&e.into()))?);
            }
            let transaction = state.transaction.as_mut().unwrap();
            let value = rusuna_sqlx::fetch(transaction, &field.compiled)
                .await
                .map_err(|e| execution_error(&e))?;
            Ok(owned(value))
        })
        .await
}

fn root_field(shared: &Arc<Shared>, root: &Root) -> Field {
    let definition = &root.definition;
    let owner = match root.operation {
        Operation::Query => &shared.query,
        Operation::Mutation => &shared.mutation,
        Operation::Subscription => unreachable!(),
    };
    if let Some(rest) = rest_source::RestRuntime::field(shared, owner, definition) {
        return rest;
    }
    let mut field = Field::new(
        definition.name.node.to_string(),
        type_ref(&definition.ty.node),
        {
            let shared = shared.clone();
            let root = root.clone();
            move |ctx| {
                let shared = shared.clone();
                let root = root.clone();
                FieldFuture::new(async move { resolve_root(&shared, &root, &ctx).await })
            }
        },
    );
    for argument in &definition.arguments {
        field = field.argument(InputValue::new(
            argument.node.name.node.to_string(),
            type_ref(&argument.node.ty.node),
        ));
    }
    field
}

struct Transactions;

impl ExtensionFactory for Transactions {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(TransactionExtension)
    }
}

struct TransactionExtension;

#[async_trait::async_trait]
impl Extension for TransactionExtension {
    async fn prepare_request(
        &self,
        ctx: &ExtensionContext<'_>,
        mut request: Request,
        next: NextPrepareRequest<'_>,
    ) -> ServerResult<Request> {
        let operation_name = request.operation_name.clone();
        let definitions: Vec<(Name, Option<Value>, bool)> = {
            let document = request.parsed_query()?;
            let operation = match &document.operations {
                DocumentOperations::Single(operation) => Some(operation),
                DocumentOperations::Multiple(operations) => match &operation_name {
                    Some(name) => operations.get(name.as_str()),
                    None if operations.len() == 1 => operations.values().next(),
                    None => None,
                },
            };
            operation
                .map(|operation| {
                    operation
                        .node
                        .variable_definitions
                        .iter()
                        .map(|d| {
                            (
                                d.node.name.node.clone(),
                                d.node.default_value().cloned(),
                                d.node.var_type.node.nullable,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        for (name, default, nullable) in definitions {
            if request.variables.contains_key(&name) {
                continue;
            }
            match default {
                Some(value) => {
                    request.variables.insert(name, value);
                }
                None if nullable => {
                    request.variables.insert(name, Value::Null);
                }
                None => {}
            }
        }
        next.run(ctx, request).await
    }

    async fn execute(
        &self,
        ctx: &ExtensionContext<'_>,
        operation_name: Option<&str>,
        next: NextExecute<'_>,
    ) -> Response {
        let slot = Arc::new(Slot::default());
        let mut data = Data::default();
        data.insert(slot.clone());
        let mut response = next.run_with_data(ctx, operation_name, data).await;
        let mut state = slot.state.lock().await;
        let transaction = state.transaction.take();
        if state.failed {
            response.data = Value::Null;
        }
        if let Some(transaction) = transaction {
            let result = if response.errors.is_empty() {
                transaction.commit().await
            } else {
                response.data = Value::Null;
                transaction.rollback().await
            };
            if let Err(error) = result {
                response.data = Value::Null;
                response
                    .errors
                    .push(ServerError::new(error.to_string(), None));
            }
        }
        response
    }
}
