use super::*;
use crate::{Kind, Relation, RootField, RootKind, Table};
use async_graphql_parser::{Positioned, parse_schema};
use async_graphql_value::ConstValue;
use std::collections::BTreeSet;

pub(super) fn directives<'a>(
    values: &'a [Positioned<ConstDirective>],
    allowed: &[&str],
) -> Result<BTreeMap<String, &'a ConstDirective>, Error> {
    let mut out = BTreeMap::new();
    for value in values {
        let name = value.node.name.node.to_string();
        if !allowed.contains(&name.as_str()) || out.insert(name.clone(), &value.node).is_some() {
            return Err(error(
                value.pos,
                format!("unknown or repeated directive @{name}"),
            ));
        }
        let args: &[&str] = match name.as_str() {
            "table" => &["name", "schema", "read_only"],
            "primary_key" => &["columns", "constraint"],
            "insert_permission" => &["role", "columns", "check", "set"],
            "update_permission" => &["role", "columns", "where", "check", "set"],
            "delete_permission" => &["role", "where"],
            "column" => &["name"],
            "relation" => &["columns", "references"],
            "aggregate" => &["target"],
            "list" => &[],
            "permission" => &["role", "where", "columns", "allow_aggregations", "limit"],
            "native_query" => &["sql", "arguments"],
            "function" => &["name", "schema", "arguments"],
            "rest" => &[
                "source",
                "method",
                "path",
                "query",
                "body",
                "result",
                "not_found",
                "roles",
            ],
            _ => unreachable!(),
        };
        let mut seen = BTreeSet::new();
        for (key, _) in &value.node.arguments {
            if !args.contains(&key.node.as_str()) || !seen.insert(&key.node) {
                return Err(error(
                    key.pos,
                    format!("unknown or repeated argument {}", key.node),
                ));
            }
        }
    }
    Ok(out)
}

pub(super) fn string(d: &ConstDirective, key: &str) -> Result<String, Error> {
    match d.get_argument(key) {
        Some(Positioned {
            node: ConstValue::String(value),
            ..
        }) if !value.is_empty() => Ok(value.clone()),
        _ => Err(error(
            d.name.pos,
            format!("@{} requires a nonempty string {key}", d.name.node),
        )),
    }
}

pub(super) fn strings(d: &ConstDirective, key: &str) -> Result<Vec<String>, Error> {
    let Some(Positioned {
        node: ConstValue::List(values),
        ..
    }) = d.get_argument(key)
    else {
        return Err(error(
            d.name.pos,
            format!("{key} must be a list of column names"),
        ));
    };
    values
        .iter()
        .map(|v| match v {
            ConstValue::String(v) if !v.is_empty() => Ok(v.clone()),
            _ => Err(error(d.name.pos, "column names must be nonempty strings")),
        })
        .collect()
}

pub(super) fn relation_keys(d: &ConstDirective) -> Result<(Vec<String>, Vec<String>), Error> {
    let columns = strings(d, "columns")?;
    let references = strings(d, "references")?;
    if columns.is_empty()
        || columns.len() != references.len()
        || columns.iter().collect::<BTreeSet<_>>().len() != columns.len()
        || references.iter().collect::<BTreeSet<_>>().len() != references.len()
    {
        return Err(error(
            d.name.pos,
            "relationship columns and references must be nonempty, distinct and equally long",
        ));
    }
    Ok((columns, references))
}

pub(super) fn insert(
    defs: &mut BTreeMap<String, String>,
    name: String,
    definition: String,
    pos: Pos,
) -> Result<(), Error> {
    if defs.insert(name.clone(), definition).is_some() {
        return Err(error(
            pos,
            format!("generated type {name} collides with another type"),
        ));
    }
    Ok(())
}

fn args(target: &str, arguments: &[(String, String)]) -> String {
    let declared: String = arguments
        .iter()
        .map(|(name, ty)| format!("{name}: {ty}, "))
        .collect();
    format!(
        "({declared}where: {target}_bool_exp, order_by: [{target}_order_by!], limit: Int, offset: Int)"
    )
}

fn function_schema(d: &ConstDirective) -> Result<String, Error> {
    if d.get_argument("schema").is_some() {
        string(d, "schema")
    } else {
        Ok("public".into())
    }
}

fn native_arguments(
    d: &ConstDirective,
    scalars: &BTreeSet<String>,
) -> Result<Vec<(String, String)>, Error> {
    match d.get_argument("arguments") {
        None => Ok(Vec::new()),
        Some(Positioned {
            node: ConstValue::Object(fields),
            pos,
        }) => fields
            .iter()
            .map(|(name, ty)| match ty {
                ConstValue::String(ty) if scalars.contains(ty.strip_suffix('!').unwrap_or(ty)) => {
                    Ok((name.to_string(), ty.clone()))
                }
                _ => Err(error(
                    *pos,
                    format!(
                        "the type of argument {name} must name a scalar, as \"Int\" or \"Int!\""
                    ),
                )),
            })
            .collect(),
        Some(value) => Err(error(
            value.pos,
            "arguments must be an object from argument names to scalar type names",
        )),
    }
}

fn render_input(field: &InputValueDefinition) -> String {
    let mut rendered = format!("{}: {}", field.name.node, field.ty.node);
    if let Some(value) = &field.default_value {
        rendered.push_str(&format!(" = {}", value.node));
    }
    rendered
}

fn render_field(field: &FieldDefinition) -> String {
    let args = if field.arguments.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            field
                .arguments
                .iter()
                .map(|a| render_input(&a.node))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut rendered = format!("  {}{args}: {}", field.name.node, field.ty.node);
    for directive in &field.directives {
        let directive = &directive.node;
        let args = directive
            .arguments
            .iter()
            .map(|(name, value)| format!("{}: {}", name.node, value.node))
            .collect::<Vec<_>>()
            .join(", ");
        rendered.push_str(&format!(" @{}({args})", directive.name.node));
    }
    rendered.push('\n');
    rendered
}

fn render_input_fields(input: &InputObjectType) -> String {
    input
        .fields
        .iter()
        .map(|field| format!("  {}\n", render_input(&field.node)))
        .collect()
}

fn render_enum(name: &str, enumeration: &EnumType) -> String {
    let values = enumeration
        .values
        .iter()
        .map(|v| v.node.value.node.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!("enum {name} {{ {values} }}\n")
}

fn render_type(keyword: &str, name: &str, fields: String) -> String {
    if fields.is_empty() {
        format!("{keyword} {name}\n")
    } else {
        format!("{keyword} {name} {{\n{fields}}}\n")
    }
}

pub(super) fn load(sdl: &str) -> Result<Schema, Error> {
    bounded(sdl)?;
    let document = parse_schema(sdl).map_err(syntax)?;
    let mut query = "Query".to_string();
    let mut seen_schema = false;
    let mut declares_mutation = false;
    let mut objects = BTreeMap::new();
    let mut defs = BTreeMap::new();
    let mut inputs = BTreeMap::new();
    let mut json_scalar = false;
    let mut scalars: BTreeSet<String> = ["Int", "Float", "String", "Boolean", "ID"]
        .into_iter()
        .map(str::to_string)
        .collect();
    for definition in document.definitions {
        match definition {
            TypeSystemDefinition::Schema(s) => {
                if seen_schema
                    || s.node.extend
                    || s.node
                        .mutation
                        .as_ref()
                        .is_some_and(|name| name.node != "Mutation")
                    || s.node.subscription.is_some()
                    || !s.node.directives.is_empty()
                {
                    return Err(error(
                        s.pos,
                        "expected one schema definition with a query and optional Mutation root",
                    ));
                }
                seen_schema = true;
                declares_mutation = s.node.mutation.is_some();
                query = s
                    .node
                    .query
                    .ok_or_else(|| error(s.pos, "query root is required"))?
                    .node
                    .to_string();
            }
            TypeSystemDefinition::Type(t) => {
                let name = t.node.name.node.to_string();
                if t.node.extend
                    || name.starts_with("__")
                    || defs.contains_key(&name)
                    || scalars.contains(&name)
                {
                    return Err(error(
                        t.pos,
                        format!("reserved, extended or repeated type {name}"),
                    ));
                }
                match &t.node.kind {
                    TypeKind::Scalar if name == "DateTime" && t.node.directives.is_empty() => {
                        scalars.insert(name.clone());
                        defs.insert(name.clone(), format!("scalar {name}\n"));
                    }
                    TypeKind::Scalar if name == "JSON" && t.node.directives.is_empty() => {
                        json_scalar = true;
                        defs.insert(name, "scalar JSON\n".into());
                    }
                    TypeKind::Enum(enumeration) if t.node.directives.is_empty() => {
                        defs.insert(name.clone(), render_enum(&name, enumeration));
                        inputs.insert(name, t);
                    }
                    TypeKind::InputObject(input) if t.node.directives.is_empty() => {
                        defs.insert(
                            name.clone(),
                            render_type("input", &name, render_input_fields(input)),
                        );
                        inputs.insert(name, t);
                    }
                    TypeKind::Object(o) if o.implements.is_empty() => {
                        defs.insert(name.clone(), String::new());
                        objects.insert(name, t);
                    }
                    _ => {
                        return Err(error(
                            t.pos,
                            "expected an object, enum, input object, scalar DateTime or scalar JSON",
                        ));
                    }
                }
            }
            TypeSystemDefinition::Directive(d) => {
                return Err(error(
                    d.pos,
                    "mapping directives are built in; directive definitions are unsupported",
                ));
            }
        }
    }
    if !objects.contains_key(&query) {
        return Err(error(Pos::default(), "query root type is missing"));
    }
    if query == "Mutation" {
        return Err(error(
            objects[&query].pos,
            "Mutation is reserved for the mutation root",
        ));
    }
    if declares_mutation && !objects.contains_key("Mutation") {
        return Err(error(Pos::default(), "mutation root type is missing"));
    }
    let mapped_types = objects
        .iter()
        .filter(|(name, ty)| {
            *name != &query
                && name.as_str() != "Mutation"
                && ty.node.directives.iter().any(|d| {
                    matches!(
                        d.node.name.node.as_str(),
                        "table" | "native_query" | "function"
                    )
                })
        })
        .map(|(name, _)| name.clone())
        .collect();
    let mut rest_scalars = scalars.clone();
    if !defs.contains_key("DateTime") {
        rest_scalars.insert("DateTime".into());
    }
    if json_scalar {
        rest_scalars.insert("JSON".into());
    }
    let rest = rest::load(&query, &objects, &mapped_types, &rest_scalars, &inputs)?;
    if !defs.contains_key("DateTime") && rest::uses_date_time(&query, &objects, &inputs, &rest) {
        defs.insert("DateTime".into(), "scalar DateTime\n".into());
    }
    let table_names: BTreeSet<_> = objects
        .keys()
        .filter(|n| **n != query && n.as_str() != "Mutation" && !rest.objects.contains_key(*n))
        .cloned()
        .collect();
    let mut natives: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, t) in &objects {
        if let Some(d) = t
            .node
            .directives
            .iter()
            .find(|d| ["native_query", "function"].contains(&d.node.name.node.as_str()))
        {
            natives.insert(name.clone(), native_arguments(&d.node, &scalars)?);
        }
    }
    let mut tables = Vec::new();
    let mut roots = Vec::new();
    let mut targets = BTreeMap::new();
    let mut scalar_fields: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, t) in &objects {
        if name == "Mutation" {
            continue;
        }
        let TypeKind::Object(o) = &t.node.kind else {
            unreachable!()
        };
        if rest.objects.contains_key(name) {
            defs.insert(
                name.clone(),
                render_type(
                    "type",
                    name,
                    o.fields
                        .iter()
                        .map(|f| {
                            let field = f.node.name.node.to_string();
                            let Some(relation) = rest.relations.get(&(name.clone(), field.clone()))
                            else {
                                return render_field(&f.node);
                            };
                            targets.insert((name.clone(), field.clone()), relation.target.clone());
                            let mut ty = f.node.ty.node.clone();
                            if !relation.list {
                                ty.nullable = true;
                            }
                            format!("  {field}{}: {ty}\n", args(&relation.target, &[]))
                        })
                        .collect(),
                ),
            );
            continue;
        }
        let root = name == &query;
        let mappings: Vec<_> = t
            .node
            .directives
            .iter()
            .filter(|d| {
                ![
                    "permission",
                    "insert_permission",
                    "update_permission",
                    "delete_permission",
                ]
                .contains(&d.node.name.node.as_str())
            })
            .cloned()
            .collect();
        let d = directives(
            &mappings,
            if root {
                &[]
            } else {
                &["table", "primary_key", "native_query", "function"]
            },
        )?;
        if root
            && t.node.directives.iter().any(|d| {
                [
                    "permission",
                    "insert_permission",
                    "update_permission",
                    "delete_permission",
                ]
                .contains(&d.node.name.node.as_str())
            })
        {
            return Err(error(t.pos, "permission directives apply to table types"));
        }
        let mut table = if root {
            Table::new(name, "", "")
        } else if let Some(function) = d.get("function") {
            if d.contains_key("table")
                || d.contains_key("primary_key")
                || d.contains_key("native_query")
            {
                return Err(error(
                    t.pos,
                    "@function types take neither @table, @primary_key nor @native_query",
                ));
            }
            Table::function(
                name,
                function_schema(function)?,
                string(function, "name")?,
                natives[name].iter().map(|(n, _)| n.clone()),
            )
        } else if let Some(native) = d.get("native_query") {
            if d.contains_key("table") || d.contains_key("primary_key") {
                return Err(error(
                    t.pos,
                    "@native_query types take neither @table nor @primary_key",
                ));
            }
            Table::statement(
                name,
                &string(native, "sql")?,
                natives[name].iter().map(|(n, _)| n.clone()),
            )
            .map_err(|e| error(t.pos, e.to_string()))?
        } else {
            let mapping = d.get("table").ok_or_else(|| {
                error(
                    t.pos,
                    "row types require @table, @native_query or @function",
                )
            })?;
            Table::new(
                name,
                if mapping.get_argument("schema").is_some() {
                    string(mapping, "schema")?
                } else {
                    "public".into()
                },
                string(mapping, "name")?,
            )
        };
        if let Some(mapping) = d.get("table") {
            match mapping.get_argument("read_only") {
                None => {}
                Some(Positioned {
                    node: ConstValue::Boolean(b),
                    ..
                }) => table.read_only = *b,
                Some(value) => return Err(error(value.pos, "read_only must be a boolean")),
            }
        }
        if let Some(key) = d.get("primary_key") {
            table.primary_key = Some(crate::PrimaryKey {
                columns: strings(key, "columns")?,
                constraint: if key.get_argument("constraint").is_some() {
                    string(key, "constraint")?
                } else {
                    format!("{}_pkey", table.table)
                },
            });
        }
        let mut fields = BTreeSet::new();
        let mut rendered = format!("type {name} {{\n");
        for f in &o.fields {
            let field = f.node.name.node.to_string();
            if field.starts_with("__")
                || (!root && ["_and", "_or", "_not"].contains(&field.as_str()))
                || !fields.insert(field.clone())
                || (!rest::has_rest(&f.node) && !f.node.arguments.is_empty())
            {
                return Err(error(
                    f.pos,
                    "reserved or repeated field, or explicitly declared arguments",
                ));
            }
            if root && rest::has_rest(&f.node) {
                rendered.push_str(&render_field(&f.node));
                continue;
            }
            let d = directives(
                &f.node.directives,
                if root {
                    &["list", "aggregate", "function"]
                } else {
                    &["column", "relation", "aggregate"]
                },
            )?;
            let ty = &f.node.ty.node;
            let list = matches!(ty.base, BaseType::List(_));
            if let BaseType::List(inner) = &ty.base
                && matches!(inner.base, BaseType::List(_))
            {
                return Err(error(f.pos, "nested lists are unsupported"));
            }
            let aggregate = d.get("aggregate");
            let target = if let Some(a) = aggregate {
                let target = string(a, "target")?;
                if list || named(ty) != format!("{target}_aggregate") {
                    return Err(error(
                        f.pos,
                        "@aggregate requires the non-list type <target>_aggregate",
                    ));
                }
                target
            } else {
                named(ty).to_string()
            };
            let relation = d.get("relation");
            let mapped = root || relation.is_some() || aggregate.is_some();
            if mapped {
                if !table_names.contains(&target) {
                    return Err(error(f.pos, format!("unknown table type {target}")));
                }
                if !root && natives.get(&target).is_some_and(|a| !a.is_empty()) {
                    return Err(error(
                        f.pos,
                        format!("{target} takes arguments and can only be read from root fields"),
                    ));
                }
                if d.contains_key("column")
                    || (root && (d.contains_key("list") == aggregate.is_some()))
                    || (!root && relation.is_none())
                    || (root && aggregate.is_none() && !list)
                {
                    return Err(error(
                        f.pos,
                        "incompatible mapping directives or field type",
                    ));
                }
                let mut declared = if root {
                    natives.get(&target).cloned().unwrap_or_default()
                } else {
                    Vec::new()
                };
                if root {
                    let mut root_field = RootField::new(
                        &field,
                        &target,
                        if aggregate.is_some() {
                            RootKind::Aggregate
                        } else {
                            RootKind::List
                        },
                    );
                    if let Some(function) = d.get("function") {
                        if natives.contains_key(&target) {
                            return Err(error(
                                f.pos,
                                "a root @function returns the rows of a @table type",
                            ));
                        }
                        declared = native_arguments(function, &scalars)?;
                        root_field = root_field.body(crate::Body::Function {
                            schema: function_schema(function)?,
                            name: string(function, "name")?,
                            arguments: declared.iter().map(|(n, _)| n.clone()).collect(),
                        });
                    }
                    roots.push(root_field);
                } else {
                    let (columns, references) = relation_keys(relation.unwrap())?;
                    table.relations.push(Relation::new(
                        &field,
                        if aggregate.is_some() {
                            Kind::Aggregate
                        } else if list {
                            Kind::Array
                        } else {
                            Kind::Object
                        },
                        &target,
                        columns,
                        references,
                    ));
                }
                if root || list || aggregate.is_some() {
                    targets.insert((name.clone(), field.clone()), target.clone());
                    rendered.push_str(&format!("  {field}{}: {ty}\n", args(&target, &declared)));
                } else {
                    rendered.push_str(&format!("  {field}: {ty}\n"));
                }
            } else {
                if list || !scalars.contains(&target) {
                    return Err(error(
                        f.pos,
                        format!("unknown scalar {target} or missing @relation"),
                    ));
                }
                let column = d
                    .get("column")
                    .map(|d| string(d, "name"))
                    .transpose()?
                    .unwrap_or_else(|| field.clone());
                table.columns.push(crate::Column::new(&field, column));
                scalar_fields
                    .entry(name.clone())
                    .or_default()
                    .push((field.clone(), target));
                rendered.push_str(&format!("  {field}: {ty}\n"));
            }
        }
        if fields.is_empty() {
            return Err(error(t.pos, "object types require fields"));
        }
        rendered.push_str("}\n");
        defs.insert(name.clone(), rendered);
        if !root {
            tables.push(table);
        }
    }
    for ((owner, field), relation) in &rest.relations {
        let declared = scalar_fields.get(&relation.target);
        if relation.references.iter().any(|reference| {
            !declared.is_some_and(|columns| columns.iter().any(|(name, _)| name == reference))
        }) {
            let TypeKind::Object(object) = &objects[owner].node.kind else {
                unreachable!()
            };
            let field = object
                .fields
                .iter()
                .find(|f| f.node.name.node == field.as_str())
                .unwrap();
            return Err(error(
                field.pos,
                "REST relationship references must be scalar columns of the target",
            ));
        }
    }
    let pos = objects[&query].pos;
    insert(&mut defs, "order_by".into(), "enum order_by { asc asc_nulls_first asc_nulls_last desc desc_nulls_first desc_nulls_last }\n".into(), pos)?;
    for scalar in &scalars {
        let mut fields = format!("input {scalar}_comparison_exp {{\n");
        for op in ["_eq", "_neq", "_gt", "_gte", "_lt", "_lte"] {
            fields.push_str(&format!("  {op}: {scalar}\n"));
        }
        for op in ["_in", "_nin"] {
            fields.push_str(&format!("  {op}: [{scalar}!]\n"));
        }
        fields.push_str("  _is_null: Boolean\n");
        if scalar == "String" {
            for op in ["_like", "_nlike", "_ilike", "_nilike"] {
                fields.push_str(&format!("  {op}: String\n"));
            }
        }
        fields.push_str("}\n");
        insert(&mut defs, format!("{scalar}_comparison_exp"), fields, pos)?;
    }
    for table in &tables {
        let name = &table.name;
        let pos = objects[name].pos;
        let columns = scalar_fields.get(name).cloned().unwrap_or_default();
        let mut boolean = format!(
            "input {name}_bool_exp {{\n  _and: [{name}_bool_exp!]\n  _or: [{name}_bool_exp!]\n  _not: {name}_bool_exp\n"
        );
        let mut order = format!("input {name}_order_by {{\n");
        for (field, scalar) in &columns {
            boolean.push_str(&format!("  {field}: {scalar}_comparison_exp\n"));
            order.push_str(&format!("  {field}: order_by\n"));
        }
        for r in &table.relations {
            if r.kind != Kind::Aggregate {
                boolean.push_str(&format!("  {}: {}_bool_exp\n", r.field, r.target));
            }
        }
        boolean.push_str("}\n");
        order.push_str("}\n");
        if columns.is_empty() {
            return Err(error(
                objects[name].pos,
                "table types require at least one scalar column",
            ));
        }
        insert(&mut defs, format!("{name}_bool_exp"), boolean, pos)?;
        insert(&mut defs, format!("{name}_order_by"), order, pos)?;
        insert(
            &mut defs,
            format!("{name}_aggregate"),
            format!(
                "type {name}_aggregate {{\n  aggregate: {name}_aggregate_fields\n  nodes: [{name}!]!\n}}\n"
            ),
            pos,
        )?;
        let mut summary = format!("type {name}_aggregate_fields {{\n  count: Int!\n");
        for op in ["sum", "avg", "min", "max"] {
            let applicable: Vec<_> = columns
                .iter()
                .filter(|(_, s)| {
                    if op == "sum" || op == "avg" {
                        s == "Int" || s == "Float"
                    } else {
                        s != "Boolean"
                    }
                })
                .collect();
            if applicable.is_empty() {
                continue;
            }
            summary.push_str(&format!("  {op}: {name}_{op}_fields\n"));
            let mut metrics = format!("type {name}_{op}_fields {{\n");
            for (field, scalar) in applicable {
                metrics.push_str(&format!(
                    "  {field}: {}\n",
                    if op == "avg" { "Float" } else { scalar }
                ));
            }
            metrics.push_str("}\n");
            insert(&mut defs, format!("{name}_{op}_fields"), metrics, pos)?;
        }
        summary.push_str("}\n");
        insert(&mut defs, format!("{name}_aggregate_fields"), summary, pos)?;
    }
    let mut mutation_fields = String::new();
    let (mutation_roots, primary_roots) = writes::definitions(
        &tables,
        &scalar_fields,
        &mut defs,
        &query,
        &mut mutation_fields,
    )?;
    let mutation_pos = if let Some(mutation) = objects.get("Mutation") {
        let TypeKind::Object(object) = &mutation.node.kind else {
            unreachable!()
        };
        for field in &object.fields {
            if mutation_roots.contains_key(field.node.name.node.as_str()) {
                return Err(error(
                    field.pos,
                    "generated mutation root collides with a declared field",
                ));
            }
            mutation_fields.push_str(&render_field(&field.node));
        }
        defs.remove("Mutation");
        mutation.pos
    } else {
        Pos::default()
    };
    insert(
        &mut defs,
        "Mutation".into(),
        render_type("type", "Mutation", mutation_fields),
        mutation_pos,
    )?;
    let subscription = defs[&query]
        .lines()
        .filter(|line| {
            let field = line
                .trim_start()
                .split(['(', ':'])
                .next()
                .unwrap_or_default();
            rest.fields.is_empty() || !rest.fields.contains_key(&(query.clone(), field.into()))
        })
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .replacen(&format!("type {query} "), "type Subscription ", 1);
    let subscription = if subscription == "type Subscription {\n}\n" {
        "type Subscription\n".into()
    } else {
        subscription
    };
    insert(
        &mut defs,
        "Subscription".into(),
        subscription,
        objects[&query].pos,
    )?;
    let sdl = format!(
        "schema {{ query: {query} mutation: Mutation subscription: Subscription }}\n\n{}",
        defs.values().cloned().collect::<Vec<_>>().join("\n")
    );
    let expanded = parse_schema(&sdl).map_err(syntax)?;
    let mut types = BTreeMap::new();
    for definition in expanded.definitions {
        if let TypeSystemDefinition::Type(t) = definition {
            types.insert(t.node.name.node.to_string(), t.node.kind);
        }
    }
    for scalar in scalars {
        types.insert(scalar, TypeKind::Scalar);
    }
    let mut document = DocumentSchema {
        types,
        targets,
        sdl,
        query,
        session_types: Vec::new(),
        mutation_roots,
        primary_roots,
        row_types: scalar_fields
            .into_iter()
            .flat_map(|(t, fields)| fields.into_iter().map(move |(f, ty)| ((t.clone(), f), ty)))
            .collect(),
    };
    for kind in document.types.values() {
        let args: Vec<_> = match kind {
            TypeKind::Object(object) => object
                .fields
                .iter()
                .flat_map(|f| &f.node.arguments)
                .collect(),
            TypeKind::InputObject(input) => input.fields.iter().collect(),
            _ => continue,
        };
        for arg in args {
            if let Some(value) = &arg.node.default_value {
                query::coerce(
                    &document,
                    value.node.clone(),
                    &arg.node.ty.node,
                    false,
                    value.pos,
                    0,
                )?;
            }
        }
    }
    for table in &mut tables {
        let mut roles = BTreeSet::new();
        for directive in &objects[&table.name].node.directives {
            if directive.node.name.node != "permission" {
                continue;
            }
            directives(std::slice::from_ref(directive), &["permission"])?;
            let d = &directive.node;
            let role = string(d, "role")?;
            if role == "admin" || !roles.insert(role.clone()) {
                return Err(error(
                    directive.pos,
                    "permission roles must be distinct and cannot be admin",
                ));
            }
            let columns = strings(d, "columns")?;
            if columns.iter().collect::<BTreeSet<_>>().len() != columns.len()
                || columns
                    .iter()
                    .any(|f| !table.columns.iter().any(|c| &c.field == f))
            {
                return Err(error(
                    directive.pos,
                    "permission columns must be distinct scalar field names",
                ));
            }
            let value = d
                .get_argument("where")
                .ok_or_else(|| error(directive.pos, "@permission requires where"))?;
            let (filter, session_types) =
                query::permission_filter(&document, &table.name, &value.node, value.pos)?;
            let allow_aggregations = match d.get_argument("allow_aggregations") {
                None => false,
                Some(Positioned {
                    node: ConstValue::Boolean(b),
                    ..
                }) => *b,
                _ => return Err(error(directive.pos, "allow_aggregations must be a boolean")),
            };
            let limit = match d.get_argument("limit") {
                None => None,
                Some(Positioned {
                    node: ConstValue::Number(n),
                    ..
                }) if n.as_u64().is_some() => n.as_u64(),
                _ => {
                    return Err(error(directive.pos, "limit must be a non-negative integer"));
                }
            };
            document.session_types.extend(
                session_types
                    .into_iter()
                    .map(|(name, ty)| (role.clone(), name, ty)),
            );
            table.permissions.push(crate::Permission {
                role,
                filter,
                columns,
                allow_aggregations,
                limit,
            });
        }
    }
    writes::permissions(&objects, &mut tables, &mut document)?;
    let mut schema = Schema::new(tables, roots)?;
    schema.document = Some(document);
    schema.rest = rest;
    Ok(schema)
}

pub(super) fn for_role(
    document: &DocumentSchema,
    schema: &crate::compiler::model::Schema,
    catalog: &crate::compiler::model::WriteCatalog,
    rest: &crate::RestCatalog,
    role: &str,
) -> DocumentSchema {
    if role == "admin" {
        return document.clone();
    }
    let visible = crate::compiler::model::visible_schema(schema.clone(), role);
    let mut document = document.clone();
    let suffixes = [
        "",
        "_bool_exp",
        "_order_by",
        "_aggregate",
        "_aggregate_fields",
        "_sum_fields",
        "_avg_fields",
        "_min_fields",
        "_max_fields",
    ];
    for table in &schema.tables {
        let permitted = visible.tables.iter().find(|t| t.name == table.name);
        let aggregate =
            crate::compiler::model::aggregate_visible(schema.clone(), role, table.name.clone());
        for suffix in suffixes {
            let name = format!("{}{suffix}", table.name);
            let Some(permitted) = permitted else {
                document.types.remove(&name);
                continue;
            };
            if !aggregate && !["", "_bool_exp", "_order_by"].contains(&suffix) {
                document.types.remove(&name);
                continue;
            }
            let column = |f: &str| permitted.columns.iter().any(|c| c.field == f);
            let relation = |f: &str| permitted.relations.iter().any(|r| r.field == f);
            match document.types.get_mut(&name) {
                Some(TypeKind::Object(object)) => {
                    if suffix.is_empty() {
                        object.fields.retain(|f| {
                            column(f.node.name.node.as_str()) || relation(f.node.name.node.as_str())
                        });
                        for field in &mut object.fields {
                            if permitted.relations.iter().any(|r| {
                                r.field == field.node.name.node.as_str()
                                    && matches!(r.kind, crate::compiler::model::Kind::Object)
                            }) {
                                field.node.ty.node.nullable = true;
                            }
                        }
                    } else if ["_sum_fields", "_avg_fields", "_min_fields", "_max_fields"]
                        .contains(&suffix)
                    {
                        object.fields.retain(|f| column(f.node.name.node.as_str()));
                        if object.fields.is_empty() {
                            document.types.remove(&name);
                        }
                    }
                }
                Some(TypeKind::InputObject(input)) => input.fields.retain(|f| {
                    let field = f.node.name.node.as_str();
                    column(field)
                        || (suffix == "_bool_exp"
                            && (["_and", "_or", "_not"].contains(&field) || relation(field)))
                }),
                _ => {}
            }
        }
    }
    writes::restrict(&mut document, schema, catalog, role);
    let names: BTreeSet<_> = document.types.keys().cloned().collect();
    for (name, kind) in &mut document.types {
        if let TypeKind::Object(object) = kind {
            if name == &document.query || name == "Subscription" {
                object.fields.retain(|f| {
                    (!rest.fields.is_empty()
                        && name == &document.query
                        && rest
                            .fields
                            .get(&(name.clone(), f.node.name.node.to_string()))
                            .is_some_and(|field| field.serves(role)))
                        || visible
                            .roots
                            .iter()
                            .any(|r| r.field == f.node.name.node.as_str())
                        || document
                            .primary_roots
                            .get(f.node.name.node.as_str())
                            .is_some_and(|t| visible.tables.iter().any(|v| &v.name == t))
                });
            }
            object.fields.retain(|f| {
                names.contains(named(&f.node.ty.node))
                    && rest
                        .fields
                        .get(&(name.clone(), f.node.name.node.to_string()))
                        .is_none_or(|field| field.serves(role))
                    && rest
                        .relations
                        .get(&(name.clone(), f.node.name.node.to_string()))
                        .is_none_or(|relation| {
                            crate::compiler::model::table_visible(
                                schema.clone(),
                                role,
                                &relation.target,
                            )
                        })
            });
        }
    }
    document.targets.retain(|(parent, field), _| matches!(document.types.get(parent), Some(TypeKind::Object(o)) if o.fields.iter().any(|f| f.node.name.node == field.as_str())));
    let mut reachable = BTreeSet::new();
    let mut pending = vec![
        document.query.clone(),
        "Mutation".to_string(),
        "Subscription".to_string(),
    ];
    while let Some(name) = pending.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        match document.types.get(&name) {
            Some(TypeKind::Object(object)) => {
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
            Some(TypeKind::InputObject(input)) => {
                pending.extend(
                    input
                        .fields
                        .iter()
                        .map(|f| named(&f.node.ty.node).to_string()),
                );
            }
            _ => {}
        }
    }
    document.types.retain(|name, _| {
        reachable.contains(name)
            || ["Int", "Float", "String", "Boolean", "ID"].contains(&name.as_str())
    });
    let mut rendered = Vec::new();
    for (name, kind) in &document.types {
        match kind {
            TypeKind::Scalar => {
                if matches!(name.as_str(), "DateTime" | "JSON") {
                    rendered.push(format!("scalar {name}\n"));
                }
            }
            TypeKind::Enum(enumeration) => rendered.push(render_enum(name, enumeration)),
            TypeKind::Object(object) => rendered.push(render_type(
                "type",
                name,
                object
                    .fields
                    .iter()
                    .map(|f| render_field(&f.node))
                    .collect(),
            )),
            TypeKind::InputObject(input) => {
                rendered.push(render_type("input", name, render_input_fields(input)))
            }
            _ => unreachable!(),
        }
    }
    document.sdl = format!(
        "schema {{ query: {} mutation: Mutation subscription: Subscription }}\n\n{}",
        document.query,
        rendered.join("\n")
    );
    document
}
