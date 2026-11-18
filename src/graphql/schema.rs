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
            "permission" => &["role", "where", "columns", "allow_aggregations"],
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

fn args(target: &str) -> String {
    format!("(where: {target}_bool_exp, order_by: [{target}_order_by!], limit: Int, offset: Int)")
}

pub(super) fn load(sdl: &str) -> Result<Schema, Error> {
    bounded(sdl)?;
    let document = parse_schema(sdl).map_err(syntax)?;
    let mut query = "Query".to_string();
    let mut seen_schema = false;
    let mut objects = BTreeMap::new();
    let mut defs = BTreeMap::new();
    let mut scalars: BTreeSet<String> = ["Int", "Float", "String", "Boolean", "ID"]
        .into_iter()
        .map(str::to_string)
        .collect();
    for definition in document.definitions {
        match definition {
            TypeSystemDefinition::Schema(s) => {
                if seen_schema
                    || s.node.extend
                    || s.node.mutation.is_some()
                    || s.node.subscription.is_some()
                    || !s.node.directives.is_empty()
                {
                    return Err(error(
                        s.pos,
                        "only one query schema definition is supported",
                    ));
                }
                seen_schema = true;
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
                    TypeKind::Object(o) if o.implements.is_empty() => {
                        defs.insert(name.clone(), String::new());
                        objects.insert(name, t);
                    }
                    _ => {
                        return Err(error(
                            t.pos,
                            "only table objects, the query root and scalar DateTime are supported",
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
    let table_names: BTreeSet<_> = objects.keys().filter(|n| **n != query).cloned().collect();
    let mut tables = Vec::new();
    let mut roots = Vec::new();
    let mut targets = BTreeMap::new();
    let mut scalar_fields: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, t) in &objects {
        let TypeKind::Object(o) = &t.node.kind else {
            unreachable!()
        };
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
            if root { &[] } else { &["table", "primary_key"] },
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
        } else {
            let mapping = d
                .get("table")
                .ok_or_else(|| error(t.pos, "row types require @table"))?;
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
                || !f.node.arguments.is_empty()
            {
                return Err(error(
                    f.pos,
                    "reserved or repeated field, or explicitly declared arguments",
                ));
            }
            let d = directives(
                &f.node.directives,
                if root {
                    &["list", "aggregate"]
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
                if root {
                    roots.push(RootField::new(
                        &field,
                        &target,
                        if aggregate.is_some() {
                            RootKind::Aggregate
                        } else {
                            RootKind::List
                        },
                    ));
                } else {
                    let relation = relation.unwrap();
                    let columns = strings(relation, "columns")?;
                    let references = strings(relation, "references")?;
                    if columns.is_empty()
                        || columns.len() != references.len()
                        || columns.iter().collect::<BTreeSet<_>>().len() != columns.len()
                        || references.iter().collect::<BTreeSet<_>>().len() != references.len()
                    {
                        return Err(error(
                            f.pos,
                            "relationship columns and references must be nonempty, distinct and equally long",
                        ));
                    }
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
                    rendered.push_str(&format!("  {field}{}: {ty}\n", args(&target)));
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
    let (mutation_roots, primary_roots) =
        writes::definitions(&tables, &scalar_fields, &mut defs, &query)?;
    let subscription = defs[&query].replacen(&format!("type {query} "), "type Subscription ", 1);
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
            });
        }
    }
    writes::permissions(&objects, &mut tables, &mut document)?;
    let mut schema = Schema::new(tables, roots)?;
    schema.document = Some(document);
    Ok(schema)
}

pub(super) fn for_role(
    document: &DocumentSchema,
    schema: &crate::compiler::model::Schema,
    catalog: &crate::compiler::model::WriteCatalog,
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
                    visible
                        .roots
                        .iter()
                        .any(|r| r.field == f.node.name.node.as_str())
                        || document
                            .primary_roots
                            .get(f.node.name.node.as_str())
                            .is_some_and(|t| visible.tables.iter().any(|v| &v.name == t))
                });
            }
            object
                .fields
                .retain(|f| names.contains(named(&f.node.ty.node)));
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
        let fields = match kind {
            TypeKind::Scalar => {
                if name == "DateTime" {
                    rendered.push(format!("scalar {name}\n"));
                }
                continue;
            }
            TypeKind::Enum(enumeration) => {
                rendered.push(format!(
                    "enum {name} {{ {} }}\n",
                    enumeration
                        .values
                        .iter()
                        .map(|v| v.node.value.node.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                continue;
            }
            TypeKind::Object(object) => object
                .fields
                .iter()
                .map(|f| {
                    let args = if f.node.arguments.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "({})",
                            f.node
                                .arguments
                                .iter()
                                .map(|a| format!("{}: {}", a.node.name.node, a.node.ty.node))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    format!("  {}{args}: {}\n", f.node.name.node, f.node.ty.node)
                })
                .collect::<String>(),
            TypeKind::InputObject(input) => input
                .fields
                .iter()
                .map(|f| format!("  {}: {}\n", f.node.name.node, f.node.ty.node))
                .collect::<String>(),
            _ => unreachable!(),
        };
        let keyword = if matches!(kind, TypeKind::InputObject(_)) {
            "input"
        } else {
            "type"
        };
        rendered.push(if fields.is_empty() {
            format!("{keyword} {name}\n")
        } else {
            format!("{keyword} {name} {{\n{fields}}}\n")
        });
    }
    document.sdl = format!(
        "schema {{ query: {} mutation: Mutation subscription: Subscription }}\n\n{}",
        document.query,
        rendered.join("\n")
    );
    document
}
