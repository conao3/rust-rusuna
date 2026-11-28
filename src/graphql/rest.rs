use super::schema::{directives, string, strings};
use super::*;
use async_graphql_parser::Positioned;
use async_graphql_value::ConstValue;
use serde_json::Value as Json;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default)]
pub struct RestCatalog {
    pub fields: BTreeMap<(String, String), RestField>,
    pub objects: BTreeMap<String, BTreeMap<String, RestColumn>>,
    pub relations: BTreeMap<(String, String), RestRelation>,
}

#[derive(Debug, Clone)]
pub struct RestColumn {
    pub key: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct RestRelation {
    pub target: String,
    pub columns: Vec<String>,
    pub references: Vec<String>,
    pub list: bool,
}

#[derive(Debug, Clone)]
pub struct RestField {
    pub source: String,
    pub method: RestMethod,
    pub body: Option<BTreeMap<String, BodyTemplate>>,
    pub path: Vec<Template>,
    pub query: Vec<(String, Template)>,
    pub result: Vec<String>,
    pub not_found_null: bool,
    pub roles: BTreeSet<String>,
    pub table_columns: Option<Vec<String>>,
    pub batch: Option<RestBatch>,
}

#[derive(Debug, Clone)]
pub struct RestBatch {
    pub query: String,
    pub key: String,
    pub max_size: usize,
}

impl RestField {
    pub fn serves(&self, role: &str) -> bool {
        role == "admin" || self.roles.contains(role)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone)]
pub enum BodyTemplate {
    Literal(Json),
    Field(String),
    Session(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

#[derive(Debug, Clone)]
pub struct Template(pub Vec<TemplatePart>);

#[derive(Debug, Clone)]
pub enum TemplatePart {
    Literal(String),
    Field(String),
    Session(String),
}

impl Template {
    pub fn parse(mut text: &str) -> Result<Self, &'static str> {
        let mut parts = Vec::new();
        while let Some((literal, marker)) = text.split_once("{{") {
            if literal.contains(['{', '}']) {
                return Err("unexpected brace in template");
            }
            if !literal.is_empty() {
                parts.push(TemplatePart::Literal(literal.into()));
            }
            let (name, rest) = marker.split_once("}}").ok_or("unclosed template marker")?;
            let field = name.strip_prefix('$');
            let identifier = field.unwrap_or(name);
            if identifier.is_empty()
                || !identifier.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_alphabetic()
                        || byte == b'_'
                        || (index > 0
                            && (byte.is_ascii_digit() || (field.is_some() && byte == b'-')))
                })
            {
                return Err("invalid template marker name");
            }
            parts.push(if field.is_some() {
                TemplatePart::Session(identifier.into())
            } else {
                TemplatePart::Field(identifier.into())
            });
            text = rest;
        }
        if text.contains(['{', '}']) {
            return Err("unexpected brace in template");
        }
        if !text.is_empty() {
            parts.push(TemplatePart::Literal(text.into()));
        }
        Ok(Self(parts))
    }
}

fn path_template(path: &str) -> Result<Vec<Template>, &'static str> {
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.starts_with('/') || path.contains("//") || path.contains("..") {
        return Err("@rest path must be relative to the source URL");
    }
    path.split('/')
        .enumerate()
        .map(|(index, segment)| {
            let template = Template::parse(segment)?;
            let mut literal_segment = String::new();
            for part in &template.0 {
                match part {
                    TemplatePart::Literal(literal) => {
                        if index == 0 && literal.contains(':') {
                            return Err("@rest path must be relative to the source URL");
                        }
                        let mut bytes = literal.bytes();
                        while let Some(byte) = bytes.next() {
                            if byte == b'%' {
                                let high = bytes.next().and_then(|b| (b as char).to_digit(16));
                                let low = bytes.next().and_then(|b| (b as char).to_digit(16));
                                let decoded =
                                    high.zip(low).map(|(high, low)| (16 * high + low) as u8);
                                match decoded {
                                    Some(b'/' | b'\\' | b'\r' | b'\n') | None => {
                                        return Err("invalid percent escape in @rest path");
                                    }
                                    Some(byte) => literal_segment.push(char::from(byte)),
                                }
                            } else if byte.is_ascii_alphanumeric()
                                || b"-._~!$&'()*+,;=:@".contains(&byte)
                            {
                                literal_segment.push(char::from(byte));
                            } else {
                                return Err("invalid character in @rest path");
                            }
                        }
                    }
                    _ => literal_segment.push('_'),
                }
            }
            if matches!(literal_segment.as_str(), "." | "..") {
                return Err("@rest path cannot traverse directories");
            }
            Ok(template)
        })
        .collect()
}

fn check_template_field(name: &str, names: &BTreeSet<&str>, pos: Pos) -> Result<(), Error> {
    if !names.contains(name) {
        return Err(error(pos, format!("unknown REST template field {name}")));
    }
    Ok(())
}

fn body_template(
    value: &ConstValue,
    names: &BTreeSet<&str>,
    pos: Pos,
) -> Result<BodyTemplate, Error> {
    match value {
        ConstValue::Object(values) => values
            .iter()
            .map(|(name, value)| Ok((name.to_string(), body_template(value, names, pos)?)))
            .collect::<Result<_, _>>()
            .map(BodyTemplate::Object),
        ConstValue::List(values) => values
            .iter()
            .map(|value| body_template(value, names, pos))
            .collect::<Result<_, _>>()
            .map(BodyTemplate::Array),
        ConstValue::String(text) => {
            if let Ok(Template(parts)) = Template::parse(text) {
                match parts.as_slice() {
                    [TemplatePart::Field(name)] => {
                        check_template_field(name, names, pos)?;
                        return Ok(BodyTemplate::Field(name.clone()));
                    }
                    [TemplatePart::Session(name)] => {
                        return Ok(BodyTemplate::Session(name.clone()));
                    }
                    _ => {}
                }
            }
            Ok(BodyTemplate::Literal(Json::String(text.clone())))
        }
        value => value
            .clone()
            .into_json()
            .map(BodyTemplate::Literal)
            .map_err(|e| error(pos, e.to_string())),
    }
}

fn definition(
    d: &ConstDirective,
    field: &FieldDefinition,
    names: &BTreeSet<&str>,
    mutation: bool,
    table: bool,
) -> Result<RestField, Error> {
    let invalid = |message| error(d.name.pos, message);
    let method = match d.get_argument("method").map(|value| &value.node) {
        None => "GET",
        Some(ConstValue::Enum(method)) => method.as_str(),
        _ => return Err(invalid("@rest method must be an enum value")),
    };
    let method = match (mutation, method) {
        (false, "GET") => RestMethod::Get,
        (true, "POST") => RestMethod::Post,
        (true, "PUT") => RestMethod::Put,
        (true, "PATCH") => RestMethod::Patch,
        (true, "DELETE") => RestMethod::Delete,
        (false, _) => return Err(invalid("@rest query fields use method GET")),
        (true, _) => {
            return Err(invalid(
                "@rest mutation fields use POST, PUT, PATCH or DELETE",
            ));
        }
    };
    let body = match d.get_argument("body") {
        None => None,
        Some(_) if !mutation => return Err(invalid("@rest body applies to mutation fields")),
        Some(value) => match body_template(&value.node, names, value.pos)? {
            BodyTemplate::Object(values) => Some(values),
            _ => return Err(error(value.pos, "@rest body must be an object")),
        },
    };
    let not_found_null = match d.get_argument("not_found") {
        None => false,
        Some(value) if value.node == ConstValue::Null && field.ty.node.nullable => true,
        _ => return Err(invalid("not_found: null requires a nullable field")),
    };
    let path = path_template(&string(d, "path")?).map_err(invalid)?;
    let query = match d.get_argument("query") {
        None => Vec::new(),
        Some(value) => match &value.node {
            ConstValue::Object(values) => values
                .iter()
                .map(|(name, value)| {
                    let ConstValue::String(template) = value else {
                        return Err(invalid("@rest query values must be template strings"));
                    };
                    Ok((
                        name.to_string(),
                        Template::parse(template).map_err(invalid)?,
                    ))
                })
                .collect::<Result<_, _>>()?,
            _ => return Err(invalid("@rest query must be an object")),
        },
    };
    let mut columns = BTreeSet::new();
    for template in path
        .iter()
        .chain(query.iter().map(|(_, template)| template))
    {
        for name in template_fields(template) {
            check_template_field(name, names, d.name.pos)?;
            columns.insert(name.to_owned());
        }
    }
    if table && let Some(column) = columns.iter().find(|name| name.len() > 54) {
        return Err(error(
            d.name.pos,
            format!("the required column name {column} is longer than 54 bytes"),
        ));
    }
    let batch = if let Some(value) = d.get_argument("batch") {
        let ConstValue::Object(batch) = &value.node else {
            return Err(invalid("@rest batch must be an object"));
        };
        if !table || columns.len() != 1 {
            return Err(invalid(
                "@rest batch requires a table field with one parent column",
            ));
        }
        let name = |key: &str| match batch.get(key) {
            Some(ConstValue::String(name)) if !name.is_empty() => Ok(name.clone()),
            _ => Err(error(
                d.name.pos,
                format!("@rest batch requires a nonempty string {key}"),
            )),
        };
        let parameter = name("query")?;
        let key = name("key")?;
        let max_size = match batch.get("max_size") {
            Some(ConstValue::Number(size)) => {
                size.as_u64().and_then(|size| usize::try_from(size).ok())
            }
            _ => None,
        }
        .filter(|&size| size > 0)
        .ok_or_else(|| invalid("@rest batch max_size must be a positive integer"))?;
        if batch.len() != 3 {
            return Err(invalid("unknown @rest batch argument"));
        }
        if path
            .iter()
            .any(|template| template_fields(template).next().is_some())
            || query
                .iter()
                .filter(|(_, template)| template_fields(template).next().is_some())
                .map(|(name, _)| name.as_str())
                .ne([parameter.as_str()])
        {
            return Err(invalid(
                "@rest batch requires one parent marker in its query parameter",
            ));
        }
        Some(RestBatch {
            query: parameter,
            key,
            max_size,
        })
    } else {
        None
    };
    let result = match d.get_argument("result").map(|value| &value.node) {
        None => Vec::new(),
        Some(ConstValue::List(values)) => values
            .iter()
            .map(|value| match value {
                ConstValue::String(value) => Ok(value.clone()),
                _ => Err(invalid("@rest result must be a list of strings")),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(invalid("@rest result must be a list of strings")),
    };
    Ok(RestField {
        source: string(d, "source")?,
        method,
        body,
        path,
        query,
        result,
        not_found_null,
        table_columns: table.then(|| columns.into_iter().collect()),
        batch,
        roles: if d.get_argument("roles").is_some() {
            strings(d, "roles")?.into_iter().collect()
        } else {
            BTreeSet::new()
        },
    })
}

fn template_fields(template: &Template) -> impl Iterator<Item = &str> {
    template.0.iter().filter_map(|part| match part {
        TemplatePart::Field(name) => Some(name.as_str()),
        _ => None,
    })
}

fn fields(ty: &TypeDefinition) -> &[Positioned<FieldDefinition>] {
    match &ty.kind {
        TypeKind::Object(object) => &object.fields,
        _ => unreachable!(),
    }
}

pub(super) fn has_rest(field: &FieldDefinition) -> bool {
    field.directives.iter().any(|d| d.node.name.node == "rest")
}

pub(super) fn load(
    query: &str,
    objects: &BTreeMap<String, Positioned<TypeDefinition>>,
    rows: &BTreeSet<String>,
    scalars: &BTreeSet<String>,
    inputs: &BTreeMap<String, Positioned<TypeDefinition>>,
) -> Result<RestCatalog, Error> {
    let mutation = objects.get("Mutation");
    if let Some(mutation) = mutation {
        directives(&mutation.node.directives, &[])?;
        for field in fields(&mutation.node) {
            if !has_rest(&field.node) {
                return Err(error(field.pos, "declared mutation fields require @rest"));
            }
        }
    }
    let tables: BTreeSet<_> = rows
        .iter()
        .filter(|&name| {
            objects[name]
                .node
                .directives
                .iter()
                .any(|d| d.node.name.node == "table")
        })
        .map(String::as_str)
        .collect();
    let mut pending: Vec<_> = fields(&objects[query].node)
        .iter()
        .chain(mutation.into_iter().flat_map(|ty| fields(&ty.node)))
        .chain(tables.iter().flat_map(|&name| fields(&objects[name].node)))
        .filter(|f| has_rest(&f.node))
        .map(|f| named(&f.node.ty.node))
        .collect();
    let mut objects_reached = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if name != query
            && name != "Mutation"
            && !rows.contains(name)
            && let Some(object) = objects.get(name)
            && objects_reached.insert(name)
        {
            pending.extend(fields(&object.node).iter().map(|f| named(&f.node.ty.node)));
        }
    }
    let scalar_output = |name: &str| {
        scalars.contains(name)
            || inputs
                .get(name)
                .is_some_and(|t| matches!(t.node.kind, TypeKind::Enum(_)))
    };
    let input_type = |name: &str| scalars.contains(name) || inputs.contains_key(name);
    let mut catalog = RestCatalog::default();
    for owner in std::iter::once(query)
        .chain(mutation.map(|_| "Mutation"))
        .chain(objects_reached.iter().copied())
        .chain(tables.iter().copied())
    {
        let mutation = owner == "Mutation";
        let root = owner == query || mutation;
        let t = &objects[owner];
        let fields = fields(&t.node);
        let table = tables.contains(owner);
        if !root && !table {
            directives(&t.node.directives, &[])?;
            if fields.is_empty() {
                return Err(error(t.pos, "object types require fields"));
            }
        }
        let mut keys = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let parent_names: BTreeSet<_> = fields
            .iter()
            .filter(|f| {
                !f.node
                    .directives
                    .iter()
                    .any(|d| matches!(d.node.name.node.as_str(), "rest" | "relation" | "aggregate"))
                    && matches!(&f.node.ty.node.base, BaseType::Named(name) if scalar_output(name))
            })
            .map(|f| f.node.name.node.as_str())
            .collect();
        for f in fields
            .iter()
            .filter(|f| (!root && !table) || has_rest(&f.node))
        {
            let name = f.node.name.node.as_str();
            if name.starts_with("__") || !seen.insert(name) {
                return Err(error(f.pos, "reserved or repeated field"));
            }
            let target = named(&f.node.ty.node);
            let d = directives(
                &f.node.directives,
                if root || table {
                    &["rest"]
                } else {
                    &["rest", "column", "relation"]
                },
            )?;
            if let Some(directive) = d.get("relation") {
                let list = matches!(f.node.ty.node.base, BaseType::List(_));
                if d.len() != 1
                    || !f.node.arguments.is_empty()
                    || !rows.contains(target)
                    || !objects[target]
                        .node
                        .directives
                        .iter()
                        .any(|d| d.node.name.node == "table")
                    || matches!(&f.node.ty.node.base, BaseType::List(inner) if matches!(inner.base, BaseType::List(_)))
                {
                    return Err(error(
                        f.pos,
                        "@relation requires a @table type or a list of that type",
                    ));
                }
                let (columns, references) = schema::relation_keys(directive)?;
                if columns
                    .iter()
                    .any(|column| !parent_names.contains(column.as_str()))
                {
                    return Err(error(
                        f.pos,
                        "REST relationship columns must be scalar fields",
                    ));
                }
                catalog.relations.insert(
                    (owner.into(), name.into()),
                    RestRelation {
                        target: target.into(),
                        columns,
                        references,
                        list,
                    },
                );
                continue;
            }
            if !scalar_output(target) && !objects_reached.contains(target) {
                return Err(error(f.pos, format!("invalid REST output type {target}")));
            }
            if let Some(directive) = d.get("rest") {
                if d.contains_key("column") {
                    return Err(error(f.pos, "@rest and @column apply to different fields"));
                }
                let args = validate_inputs(&f.node.arguments, &input_type)?;
                let spec = definition(
                    directive,
                    &f.node,
                    if root { &args } else { &parent_names },
                    mutation,
                    table,
                )?;
                catalog.fields.insert((owner.into(), name.into()), spec);
            } else {
                if !f.node.arguments.is_empty() {
                    return Err(error(f.pos, "only @rest fields take arguments"));
                }
                let key = match d.get("column").and_then(|d| d.get_argument("name")) {
                    Some(value) => match &value.node {
                        ConstValue::String(key) => key.clone(),
                        _ => return Err(error(value.pos, "@column name must be a string")),
                    },
                    None if d.contains_key("column") => {
                        return Err(error(f.pos, "@column requires a name"));
                    }
                    None => name.into(),
                };
                keys.insert(
                    name.into(),
                    RestColumn {
                        key,
                        ty: f.node.ty.node.clone(),
                    },
                );
            }
        }
        if !root && !table {
            catalog.objects.insert(owner.into(), keys);
        }
    }
    for t in inputs.values() {
        match &t.node.kind {
            TypeKind::Enum(values) => {
                let mut names = BTreeSet::new();
                for value in &values.values {
                    let name = value.node.value.node.as_str();
                    directives(&value.node.directives, &[])?;
                    if name.starts_with("__")
                        || matches!(name, "true" | "false" | "null")
                        || !names.insert(name)
                    {
                        return Err(error(value.pos, "reserved or repeated enum value"));
                    }
                }
                if names.is_empty() {
                    return Err(error(t.pos, "enum types require values"));
                }
            }
            TypeKind::InputObject(input) => {
                if input.fields.is_empty() {
                    return Err(error(t.pos, "input types require fields"));
                }
                validate_inputs(&input.fields, &input_type)?;
            }
            _ => unreachable!(),
        }
    }
    Ok(catalog)
}

pub(super) fn uses_date_time(
    query: &str,
    objects: &BTreeMap<String, Positioned<TypeDefinition>>,
    inputs: &BTreeMap<String, Positioned<TypeDefinition>>,
    catalog: &RestCatalog,
) -> bool {
    let date_time_arg =
        |arg: &Positioned<InputValueDefinition>| named(&arg.node.ty.node) == "DateTime";
    let output = |field: &Positioned<FieldDefinition>| {
        named(&field.node.ty.node) == "DateTime" || field.node.arguments.iter().any(date_time_arg)
    };
    fields(&objects[query].node)
        .iter()
        .chain(
            objects
                .get("Mutation")
                .into_iter()
                .flat_map(|ty| fields(&ty.node)),
        )
        .chain(
            catalog
                .fields
                .iter()
                .filter(|(_, field)| field.table_columns.is_some())
                .flat_map(|((owner, name), _)| {
                    fields(&objects[owner].node)
                        .iter()
                        .filter(move |field| field.node.name.node == name.as_str())
                }),
        )
        .filter(|field| has_rest(&field.node))
        .any(output)
        || catalog
            .objects
            .keys()
            .any(|name| fields(&objects[name].node).iter().any(output))
        || inputs.values().any(|ty| match &ty.node.kind {
            TypeKind::InputObject(input) => input.fields.iter().any(date_time_arg),
            _ => false,
        })
}

pub(super) fn reject_operation(
    catalog: &RestCatalog,
    query: &str,
    document: &ExecutableDocument,
    operation: &OperationDefinition,
) -> Result<(), Error> {
    if catalog.fields.is_empty() {
        return Ok(());
    }
    let root = if operation.ty == OperationType::Mutation {
        "Mutation"
    } else {
        query
    };
    let mut fragments = BTreeSet::new();
    let mut pending = vec![&operation.selection_set.node];
    while let Some(set) = pending.pop() {
        for item in &set.items {
            match &item.node {
                async_graphql_parser::types::Selection::Field(field) => {
                    let name = field.node.name.node.as_str();
                    if catalog.fields.contains_key(&(root.into(), name.into())) {
                        return Err(Error::RestField { field: name.into() });
                    }
                }
                async_graphql_parser::types::Selection::InlineFragment(fragment) => {
                    if fragment
                        .node
                        .type_condition
                        .as_ref()
                        .is_none_or(|ty| ty.node.on.node == root)
                    {
                        pending.push(&fragment.node.selection_set.node);
                    }
                }
                async_graphql_parser::types::Selection::FragmentSpread(spread) => {
                    let name = &spread.node.fragment_name.node;
                    if fragments.insert(name)
                        && let Some(fragment) = document.fragments.get(name)
                        && fragment.node.type_condition.node.on.node == root
                    {
                        pending.push(&fragment.node.selection_set.node);
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_inputs<'a>(
    fields: &'a [Positioned<InputValueDefinition>],
    input_type: &impl Fn(&str) -> bool,
) -> Result<BTreeSet<&'a str>, Error> {
    let mut names = BTreeSet::new();
    for field in fields {
        let name = field.node.name.node.as_str();
        directives(&field.node.directives, &[])?;
        if name.starts_with("__") || !names.insert(name) || !input_type(named(&field.node.ty.node))
        {
            return Err(error(
                field.pos,
                "reserved or repeated name or invalid input type",
            ));
        }
    }
    Ok(names)
}
