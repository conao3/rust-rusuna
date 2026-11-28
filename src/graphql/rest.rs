use super::schema::{directives, string, strings};
use super::*;
use crate::{Filter, Operand};
use async_graphql_parser::Positioned;
use async_graphql_value::ConstValue;
use serde_json::Value as Json;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default)]
pub struct RestCatalog {
    pub fields: BTreeMap<(String, String), RestField>,
    pub objects: BTreeMap<String, BTreeMap<String, RestColumn>>,
    pub relations: BTreeMap<(String, String), RestRelation>,
    pub permissions: BTreeMap<String, BTreeMap<String, RestPermission>>,
    pub aggregate_types: BTreeSet<String>,
    pub enums: BTreeMap<String, BTreeSet<String>>,
    filter_types: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct RestPermission {
    pub columns: BTreeSet<String>,
    pub filter: Filter<Operand>,
    pub limit: Option<u64>,
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
    pub filter: bool,
    pub output: Type,
    pub aggregate: bool,
}

#[derive(Debug, Clone)]
pub struct RestBatch {
    pub query: String,
    pub key: String,
    pub max_size: usize,
}

impl RestCatalog {
    pub fn type_visible(&self, name: &str, role: &str) -> bool {
        role == "admin"
            || self
                .permissions
                .get(name)
                .is_none_or(|roles| roles.contains_key(role))
    }

    pub fn permission(&self, name: &str, role: &str) -> Option<&RestPermission> {
        self.permissions.get(name).and_then(|roles| roles.get(role))
    }
}

pub(super) const FILTER_ARGUMENTS: [&str; 4] = ["where", "order_by", "limit", "offset"];

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
                    let key = field
                        .arguments
                        .iter()
                        .find(|argument| argument.node.name.node == *name)
                        .and_then(|argument| column_key(&argument.node.directives))
                        .and_then(|value| match &value.node {
                            ConstValue::String(key) => Some(key.as_str()),
                            _ => None,
                        })
                        .unwrap_or(name.as_str());
                    Ok((key.to_owned(), Template::parse(template).map_err(invalid)?))
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
    let filter = match d.get_argument("filter").map(|value| &value.node) {
        None | Some(ConstValue::Boolean(false)) => false,
        Some(ConstValue::Boolean(true)) => {
            if table
                || !matches!(&field.ty.node.base, BaseType::List(inner) if matches!(inner.base, BaseType::Named(_)))
            {
                return Err(invalid(
                    "@rest filter requires a list root or a REST-to-REST list field",
                ));
            }
            if field
                .arguments
                .iter()
                .any(|arg| FILTER_ARGUMENTS.contains(&arg.node.name.node.as_str()))
            {
                return Err(invalid(
                    "declared argument collides with an @rest filter argument",
                ));
            }
            true
        }
        _ => return Err(invalid("@rest filter must be a boolean")),
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
        filter,
        output: field.ty.node.clone(),
        aggregate: false,
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
            for directive in &t.node.directives {
                directives(std::slice::from_ref(directive), &["permission"])?;
            }
            if !t.node.directives.is_empty() {
                catalog.filter_types.insert(owner.into());
            }
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
                let args = validate_inputs(&f.node.arguments, &input_type, root)?;
                let spec = definition(
                    directive,
                    &f.node,
                    if root { &args } else { &parent_names },
                    mutation,
                    table,
                )?;
                if spec.filter {
                    if !objects_reached.contains(target) {
                        return Err(error(f.pos, "@rest filter requires a list of REST objects"));
                    }
                    catalog.filter_types.insert(target.into());
                    if root {
                        let aggregate_name = format!("{name}_aggregate");
                        if fields
                            .iter()
                            .any(|field| field.node.name.node == aggregate_name.as_str())
                        {
                            return Err(error(
                                f.pos,
                                "generated REST aggregate root collides with a declared field",
                            ));
                        }
                        catalog.aggregate_types.insert(target.into());
                        catalog.fields.insert(
                            (owner.into(), aggregate_name),
                            RestField {
                                aggregate: true,
                                ..spec.clone()
                            },
                        );
                    }
                }
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
                catalog.enums.insert(
                    t.node.name.node.to_string(),
                    names.into_iter().map(str::to_owned).collect(),
                );
            }
            TypeKind::InputObject(input) => {
                if input.fields.is_empty() {
                    return Err(error(t.pos, "input types require fields"));
                }
                validate_inputs(&input.fields, &input_type, true)?;
            }
            _ => unreachable!(),
        }
    }
    let mut pending: Vec<_> = catalog.filter_types.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        for column in catalog.objects[&name].values() {
            let BaseType::Named(ty) = &column.ty.base else {
                continue;
            };
            if catalog.objects.contains_key(ty.as_str())
                && catalog.filter_types.insert(ty.to_string())
            {
                pending.push(ty.to_string());
            }
        }
    }
    Ok(catalog)
}

pub(super) fn definitions(
    catalog: &RestCatalog,
    defs: &mut BTreeMap<String, String>,
    table_scalars: &BTreeSet<String>,
    objects: &BTreeMap<String, Positioned<TypeDefinition>>,
) -> Result<(), Error> {
    let mut comparisons = BTreeSet::new();
    for name in &catalog.filter_types {
        let pos = objects[name].pos;
        let mut boolean = schema::boolean_input(name);
        let mut order = format!("input {name}_order_by {{\n");
        let mut columns = Vec::new();
        for (field, column) in &catalog.objects[name] {
            let BaseType::Named(ty) = &column.ty.base else {
                continue;
            };
            if ty == "JSON" {
                continue;
            }
            if ["_and", "_or", "_not"].contains(&field.as_str()) {
                return Err(error(pos, format!("reserved filter field {name}.{field}")));
            }
            if catalog.objects.contains_key(ty.as_str()) {
                boolean.push_str(&format!("  {field}: {ty}_bool_exp\n"));
            } else {
                boolean.push_str(&format!("  {field}: {ty}_comparison_exp\n"));
                order.push_str(&format!("  {field}: order_by\n"));
                columns.push((field.clone(), ty.to_string()));
                if !table_scalars.contains(ty.as_str()) && comparisons.insert(ty.clone()) {
                    schema::comparison_definition(defs, ty, pos)?;
                }
            }
        }
        boolean.push_str("}\n");
        schema::insert(defs, format!("{name}_bool_exp"), boolean, pos)?;
        if catalog
            .fields
            .values()
            .any(|f| f.filter && named(&f.output) == name)
        {
            order.push_str("}\n");
            schema::insert(defs, format!("{name}_order_by"), order, pos)?;
        }
        if catalog.aggregate_types.contains(name) {
            schema::aggregate_definitions(defs, name, &columns, pos)?;
        }
    }
    Ok(())
}

pub(super) fn permissions(
    catalog: &mut RestCatalog,
    document: &DocumentSchema,
    objects: &BTreeMap<String, Positioned<TypeDefinition>>,
) -> Result<(), Error> {
    for name in catalog.objects.keys() {
        for directive in &objects[name].node.directives {
            let d = &directive.node;
            let role = string(d, "role")?;
            if role == "admin" || catalog.permission(name, &role).is_some() {
                return Err(error(
                    directive.pos,
                    "permission roles must be distinct and cannot be admin",
                ));
            }
            if d.get_argument("where").is_some() || d.get_argument("allow_aggregations").is_some() {
                return Err(error(
                    directive.pos,
                    "REST permissions use filter, columns and limit",
                ));
            }
            let columns = strings(d, "columns")?;
            let declared = fields(&objects[name].node);
            let unique: BTreeSet<_> = columns.iter().cloned().collect();
            if unique.len() != columns.len()
                || columns
                    .iter()
                    .any(|f| !declared.iter().any(|d| d.node.name.node == f.as_str()))
            {
                return Err(error(
                    directive.pos,
                    "permission columns must be distinct REST field names",
                ));
            }
            let value = d
                .get_argument("filter")
                .ok_or_else(|| error(directive.pos, "@permission requires filter"))?;
            let (filter, _) =
                query::permission_filter(document, name, &value.node, value.pos, true)?;
            catalog.permissions.entry(name.clone()).or_default().insert(
                role,
                RestPermission {
                    columns: unique,
                    filter,
                    limit: schema::permission_limit(d)?,
                },
            );
        }
    }
    Ok(())
}

pub(super) fn restrict(document: &mut DocumentSchema, catalog: &RestCatalog, role: &str) {
    let denied: BTreeSet<_> = catalog
        .objects
        .keys()
        .filter(|name| !catalog.type_visible(name, role))
        .cloned()
        .collect();
    for name in catalog.objects.keys() {
        if denied.contains(name) {
            document.types.remove(name);
        } else if let Some(permission) = catalog.permission(name, role)
            && let Some(TypeKind::Object(object)) = document.types.get_mut(name)
        {
            object
                .fields
                .retain(|field| permission.columns.contains(field.node.name.node.as_str()));
        }
        for suffix in [
            "_bool_exp",
            "_order_by",
            "_sum_fields",
            "_avg_fields",
            "_min_fields",
            "_max_fields",
        ] {
            if !catalog.filter_types.contains(name) {
                break;
            }
            let fields = document.types.get_mut(&format!("{name}{suffix}"));
            let visible = |field: &str| {
                catalog
                    .permission(name, role)
                    .is_none_or(|permission| permission.columns.contains(field))
            };
            match fields {
                Some(TypeKind::InputObject(input)) => input.fields.retain(|field| {
                    let field = field.node.name.node.as_str();
                    (suffix == "_bool_exp" && ["_and", "_or", "_not"].contains(&field))
                        || (!denied.contains(name)
                            && visible(field)
                            && catalog.objects[name]
                                .get(field)
                                .is_none_or(|column| !denied.contains(named(&column.ty))))
                }),
                Some(TypeKind::Object(object)) => object
                    .fields
                    .retain(|field| visible(field.node.name.node.as_str())),
                _ => {}
            }
        }
    }
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

fn column_key(values: &[Positioned<ConstDirective>]) -> Option<&Positioned<ConstValue>> {
    values
        .iter()
        .find(|directive| directive.node.name.node == "column")
        .and_then(|directive| directive.node.get_argument("name"))
}

fn validate_inputs<'a>(
    fields: &'a [Positioned<InputValueDefinition>],
    input_type: &impl Fn(&str) -> bool,
    allow_column: bool,
) -> Result<BTreeSet<&'a str>, Error> {
    let mut names = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for field in fields {
        let name = field.node.name.node.as_str();
        let allowed: &[_] = if allow_column { &["column"] } else { &[] };
        let directives = directives(&field.node.directives, allowed)?;
        if name.starts_with("__") || !names.insert(name) || !input_type(named(&field.node.ty.node))
        {
            return Err(error(
                field.pos,
                "reserved or repeated name or invalid input type",
            ));
        }
        let key = match column_key(&field.node.directives) {
            Some(value) => match &value.node {
                ConstValue::String(key) => key.as_str(),
                _ => return Err(error(value.pos, "@column name must be a string")),
            },
            None if directives.contains_key("column") => {
                return Err(error(field.pos, "@column requires a name"));
            }
            None => name,
        };
        if !keys.insert(key) {
            return Err(error(field.pos, "repeated REST input key"));
        }
    }
    Ok(names)
}
