use super::*;
use crate::{Comparison, Direction, Filter, Order, Value};
use async_graphql_parser::{Positioned, parse_query};
use async_graphql_value::{ConstValue as C, Value as V};
use std::collections::BTreeSet;

struct Variable {
    ty: Type,
    value: Option<C>,
    default_nonnull: bool,
}

struct Context<'a> {
    schema: &'a DocumentSchema,
    document: &'a ExecutableDocument,
    variables: BTreeMap<String, Variable>,
    visiting: BTreeSet<String>,
    budget: usize,
}

fn compatible(actual: &Type, expected: &Type) -> bool {
    (expected.nullable || !actual.nullable)
        && match (&actual.base, &expected.base) {
            (BaseType::Named(a), BaseType::Named(b)) => a == b,
            (BaseType::List(a), BaseType::List(b)) => compatible(a, b),
            _ => false,
        }
}

fn coerce_input(
    schema: &DocumentSchema,
    value: C,
    ty: &Type,
    json: bool,
    pos: Pos,
    depth: usize,
    permission: bool,
) -> Result<C, Error> {
    if depth > MAX_DOCUMENT_DEPTH {
        return Err(error(pos, "input nesting limit exceeded"));
    }
    if value == C::Null {
        return if ty.nullable {
            Ok(value)
        } else {
            Err(error(pos, format!("null is not a {ty}")))
        };
    }
    if let BaseType::List(inner) = &ty.base {
        let values = match value {
            C::List(v) => v,
            v => vec![v],
        };
        return Ok(C::List(
            values
                .into_iter()
                .map(|v| coerce_input(schema, v, inner, json, pos, depth + 1, permission))
                .collect::<Result<_, _>>()?,
        ));
    }
    let name = named(ty);
    match schema.types.get(name) {
        Some(TypeKind::InputObject(object)) => {
            let C::Object(values) = value else {
                return Err(error(pos, format!("expected {ty} object")));
            };
            let mut out = values.clone();
            for (key, value) in values {
                let field = object
                    .fields
                    .iter()
                    .find(|f| f.node.name.node == key)
                    .ok_or_else(|| error(pos, format!("unknown field {name}.{key}")))?;
                out.insert(
                    key,
                    coerce_input(
                        schema,
                        value,
                        &field.node.ty.node,
                        json,
                        pos,
                        depth + 1,
                        permission,
                    )?,
                );
            }
            for field in &object.fields {
                if !field.node.ty.node.nullable && !out.contains_key(&field.node.name.node) {
                    return Err(error(
                        pos,
                        format!("required input field {} is missing", field.node.name.node),
                    ));
                }
            }
            Ok(C::Object(out))
        }
        Some(TypeKind::Enum(enumeration)) => {
            let key = match &value {
                C::Enum(v) => v.as_str(),
                C::String(v) if json => v,
                _ => "",
            };
            if enumeration.values.iter().any(|v| v.node.value.node == key) {
                Ok(C::Enum(async_graphql_value::Name::new(key)))
            } else {
                Err(error(pos, format!("invalid value for {name}")))
            }
        }
        Some(TypeKind::Scalar) => {
            if permission && matches!(&value, C::String(s) if s.starts_with('$')) {
                return Ok(value);
            }
            let valid = match (name, &value) {
                ("Int", C::Number(n)) => n.as_i64().is_some_and(|n| i32::try_from(n).is_ok()),
                ("Float", C::Number(n)) => n.as_f64().is_some_and(f64::is_finite),
                ("String" | "DateTime", C::String(_)) | ("Boolean", C::Boolean(_)) => true,
                ("ID", C::String(_)) => true,
                ("ID", C::Number(n)) => n.as_i64().is_some(),
                _ => false,
            };
            if valid {
                Ok(value)
            } else {
                Err(error(pos, format!("value is not a {name}")))
            }
        }
        _ => Err(error(pos, format!("{name} is not an input type"))),
    }
}

fn coerce(
    schema: &DocumentSchema,
    value: C,
    ty: &Type,
    json: bool,
    pos: Pos,
    depth: usize,
) -> Result<C, Error> {
    coerce_input(schema, value, ty, json, pos, depth, false)
}

impl Context<'_> {
    fn value(&self, value: &V, ty: &Type, pos: Pos, depth: usize) -> Result<Option<C>, Error> {
        if depth > MAX_DOCUMENT_DEPTH {
            return Err(error(pos, "input nesting limit exceeded"));
        }
        if let V::Variable(name) = value {
            let variable = self
                .variables
                .get(name.as_str())
                .ok_or_else(|| error(pos, format!("undefined variable ${name}")))?;
            let mut actual = variable.ty.clone();
            if variable.default_nonnull {
                actual.nullable = false;
            }
            if !compatible(&actual, ty) {
                return Err(error(
                    pos,
                    format!(
                        "variable ${name} of type {} cannot be used as {ty}",
                        variable.ty
                    ),
                ));
            }
            return Ok(variable.value.clone());
        }
        if let BaseType::List(inner) = &ty.base
            && !matches!(value, V::Null)
        {
            let values = match value {
                V::List(v) => v.as_slice(),
                v => std::slice::from_ref(v),
            };
            return Ok(Some(C::List(
                values
                    .iter()
                    .map(|v| {
                        let value = self.value(v, inner, pos, depth + 1)?.unwrap_or(C::Null);
                        coerce(self.schema, value, inner, false, pos, depth + 1)
                    })
                    .collect::<Result<_, _>>()?,
            )));
        }
        if let V::Object(values) = value
            && let Some(TypeKind::InputObject(object)) = self.schema.types.get(named(ty))
        {
            let mut out = async_graphql_value::indexmap::IndexMap::new();
            for (key, value) in values {
                let field = object
                    .fields
                    .iter()
                    .find(|f| f.node.name.node == *key)
                    .ok_or_else(|| error(pos, format!("unknown field {}.{key}", named(ty))))?;
                if let Some(value) = self.value(value, &field.node.ty.node, pos, depth + 1)? {
                    out.insert(key.clone(), value);
                }
            }
            return coerce(self.schema, C::Object(out), ty, false, pos, depth).map(Some);
        }
        let constant = value
            .clone()
            .into_const()
            .ok_or_else(|| error(pos, "variable in an incompatible input value"))?;
        coerce(self.schema, constant, ty, false, pos, depth).map(Some)
    }

    fn include(&self, directives: &[Positioned<Directive>]) -> Result<bool, Error> {
        let mut included = true;
        let mut seen = BTreeSet::new();
        for d in directives {
            let name = d.node.name.node.as_str();
            if !["skip", "include"].contains(&name)
                || !seen.insert(name)
                || d.node.arguments.len() != 1
                || d.node.arguments[0].0.node != "if"
            {
                return Err(error(
                    d.pos,
                    "expected one @skip(if: Boolean!) or @include(if: Boolean!)",
                ));
            }
            let C::Boolean(condition) = self
                .value(
                    &d.node.arguments[0].1.node,
                    &Type::new("Boolean!").unwrap(),
                    d.pos,
                    0,
                )?
                .unwrap_or(C::Null)
            else {
                return Err(error(d.pos, "directive condition must be Boolean!"));
            };
            included &= if name == "skip" {
                !condition
            } else {
                condition
            };
        }
        Ok(included)
    }

    fn fields(
        &mut self,
        set: &Positioned<SelectionSet>,
        parent: &str,
        depth: usize,
    ) -> Result<Vec<Selection>, Error> {
        if depth > MAX_DOCUMENT_DEPTH {
            return Err(error(
                set.pos,
                "fragment or selection nesting limit exceeded",
            ));
        }
        let mut out = Vec::new();
        for item in &set.node.items {
            self.budget = self
                .budget
                .checked_sub(1)
                .ok_or_else(|| error(item.pos, "fragment expansion limit exceeded"))?;
            let include = self.include(item.node.directives())?;
            let fields = match &item.node {
                async_graphql_parser::types::Selection::Field(field) => {
                    vec![self.field(field, parent, depth)?]
                }
                async_graphql_parser::types::Selection::InlineFragment(fragment) => {
                    if let Some(condition) = &fragment.node.type_condition
                        && condition.node.on.node != parent
                    {
                        return Err(error(
                            condition.pos,
                            format!("fragment cannot apply to {parent}"),
                        ));
                    }
                    self.fields(&fragment.node.selection_set, parent, depth + 1)?
                }
                async_graphql_parser::types::Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.to_string();
                    if !self.visiting.insert(name.clone()) {
                        return Err(error(spread.pos, "fragment cycle"));
                    }
                    let fragment = self
                        .document
                        .fragments
                        .get(name.as_str())
                        .ok_or_else(|| error(spread.pos, format!("unknown fragment {name}")))?;
                    if !fragment.node.directives.is_empty()
                        || fragment.node.type_condition.node.on.node != parent
                    {
                        return Err(error(
                            fragment.pos,
                            format!(
                                "fragment cannot apply to {parent} or has unsupported directives"
                            ),
                        ));
                    }
                    let fields = self.fields(&fragment.node.selection_set, parent, depth + 1)?;
                    self.visiting.remove(&name);
                    fields
                }
            };
            if include {
                for field in fields {
                    merge(&mut out, field, item.pos)?;
                }
            }
        }
        Ok(out)
    }

    fn field(
        &mut self,
        field: &Positioned<Field>,
        parent: &str,
        depth: usize,
    ) -> Result<Selection, Error> {
        let f = &field.node;
        let name = f.name.node.as_str();
        if name == "__schema" || name == "__type" {
            return Err(error(field.pos, "introspection is unsupported"));
        }
        let mut selection = Selection::new(name).alias(f.response_key().node.to_string());
        if name == "__typename" {
            if !self
                .schema
                .types
                .contains_key(&format!("{parent}_bool_exp"))
                || !f.arguments.is_empty()
                || !f.selection_set.node.items.is_empty()
            {
                return Err(error(
                    field.pos,
                    "__typename is supported on row objects without arguments or selections",
                ));
            }
            return Ok(selection);
        }
        let Some(TypeKind::Object(object)) = self.schema.types.get(parent) else {
            return Err(error(field.pos, format!("{parent} is not an object")));
        };
        let spec = object
            .fields
            .iter()
            .find(|s| s.node.name.node == name)
            .ok_or_else(|| error(field.pos, format!("unknown field {parent}.{name}")))?;
        let mutation = self
            .schema
            .mutation_roots
            .get(name)
            .filter(|_| parent == "Mutation")
            .cloned();
        let primary = self
            .schema
            .primary_roots
            .get(name)
            .filter(|_| parent == self.schema.query)
            .cloned();
        let mut writes = mutation.as_ref().map(|(target, kind, single, by_key)| {
            let mut input = crate::MutationArguments::new(target, *kind);
            input.single = *single;
            input.by_key = *by_key;
            input
        });
        let mut keys = Vec::new();
        let mut present = BTreeSet::new();
        let mut arguments = BTreeSet::new();
        for (key, value) in &f.arguments {
            if !arguments.insert(key.node.as_str()) {
                return Err(error(key.pos, "repeated argument"));
            }
            let arg = spec
                .node
                .arguments
                .iter()
                .find(|a| a.node.name.node == key.node)
                .ok_or_else(|| error(key.pos, format!("unknown argument {key}")))?;
            let Some(value) = self.value(&value.node, &arg.node.ty.node, value.pos, 0)? else {
                continue;
            };
            let value = coerce(self.schema, value, &arg.node.ty.node, false, field.pos, 0)?;
            present.insert(key.node.to_string());
            if value == C::Null {
                continue;
            }
            if let Some(input) = &mut writes {
                match key.node.as_str() {
                    "on_conflict" => {
                        let C::Object(fields) = value else {
                            unreachable!()
                        };
                        let C::Enum(constraint) = &fields["constraint"] else {
                            unreachable!()
                        };
                        let C::List(columns) = &fields["update_columns"] else {
                            unreachable!()
                        };
                        input.on_conflict = Some(crate::OnConflict {
                            constraint: constraint.to_string(),
                            update_columns: columns
                                .iter()
                                .map(|c| {
                                    if let C::Enum(c) = c {
                                        c.to_string()
                                    } else {
                                        unreachable!()
                                    }
                                })
                                .collect(),
                            filter: fields
                                .get("where")
                                .filter(|v| **v != C::Null)
                                .map(|v| {
                                    filter(self.schema, &input.target, v, key.pos, 0, &mut scalar)
                                })
                                .transpose()?,
                        });
                    }
                    "objects" => {
                        let C::List(rows) = value else { unreachable!() };
                        input.objects = rows
                            .iter()
                            .map(|r| row_values(self.schema, &input.target, r, key.pos))
                            .collect::<Result<_, _>>()?;
                    }
                    "object" => {
                        input.objects =
                            vec![row_values(self.schema, &input.target, &value, key.pos)?]
                    }
                    "_set" => input.set = row_values(self.schema, &input.target, &value, key.pos)?,
                    "_inc" => {
                        input.increment = row_values(self.schema, &input.target, &value, key.pos)?
                    }
                    "pk_columns" => {
                        input.keys = row_values(self.schema, &input.target, &value, key.pos)?
                    }
                    "where" => {
                        input.filter = Some(filter(
                            self.schema,
                            &input.target,
                            &value,
                            key.pos,
                            0,
                            &mut scalar,
                        )?)
                    }
                    _ if input.by_key => input.keys.push((
                        key.node.to_string(),
                        scalar(&value, named(&arg.node.ty.node), key.pos)?,
                    )),
                    _ => return Err(error(key.pos, "unsupported mutation argument")),
                }
                continue;
            }
            if primary.is_some() {
                keys.push((
                    key.node.to_string(),
                    scalar(&value, named(&arg.node.ty.node), key.pos)?,
                ));
                continue;
            }
            match key.node.as_str() {
                "limit" => selection.limit = Some(unsigned(&value, key.pos)?),
                "offset" => selection.offset = Some(unsigned(&value, key.pos)?),
                "where" => {
                    let target = &self.schema.targets[&(parent.to_string(), name.to_string())];
                    selection.filter = Some(filter(
                        self.schema,
                        target,
                        &value,
                        key.pos,
                        0,
                        &mut scalar,
                    )?);
                }
                "order_by" => selection.order_by = order(&value, key.pos)?,
                _ => unreachable!(),
            }
        }
        for arg in &spec.node.arguments {
            if !arg.node.ty.node.nullable && !present.contains(arg.node.name.node.as_str()) {
                return Err(error(
                    field.pos,
                    format!("required argument {} is missing", arg.node.name.node),
                ));
            }
        }
        selection.mutation = writes;
        selection.primary_key = primary.map(|target| (target, keys));
        let child_type = named(&spec.node.ty.node);
        if matches!(self.schema.types.get(child_type), Some(TypeKind::Object(_))) {
            selection.fields = self.fields(&f.selection_set, child_type, depth + 1)?;
        } else if !f.selection_set.node.items.is_empty() {
            return Err(error(field.pos, "scalar fields cannot have selections"));
        }
        Ok(selection)
    }
}

fn merge(fields: &mut Vec<Selection>, field: Selection, pos: Pos) -> Result<(), Error> {
    if let Some(existing) = fields.iter_mut().find(|s| s.alias == field.alias) {
        if existing.name != field.name
            || existing.filter != field.filter
            || existing.order_by != field.order_by
            || existing.limit != field.limit
            || existing.offset != field.offset
            || existing.mutation != field.mutation
            || existing.primary_key != field.primary_key
        {
            return Err(error(pos, "conflicting fields with the same response key"));
        }
        for child in field.fields {
            merge(&mut existing.fields, child, pos)?;
        }
    } else {
        fields.push(field);
    }
    Ok(())
}

fn unsigned(value: &C, pos: Pos) -> Result<u64, Error> {
    if let C::Number(v) = value
        && let Some(v) = v.as_u64()
    {
        return Ok(v);
    }
    Err(error(pos, "pagination requires a nonnegative integer"))
}

fn scalar(value: &C, ty: &str, pos: Pos) -> Result<Value, Error> {
    Ok(match value {
        C::Number(v) if ty == "Float" => {
            Value::Float(v.as_f64().ok_or_else(|| error(pos, "invalid float"))?)
        }
        C::Number(v) => Value::Int(v.as_i64().ok_or_else(|| error(pos, "invalid integer"))?),
        C::String(v) => Value::Text(v.clone()),
        C::Boolean(v) => Value::Bool(*v),
        C::Null => Value::Null,
        _ => return Err(error(pos, "expected a scalar value")),
    })
}

fn conjunction<T>(mut values: Vec<Filter<T>>) -> Filter<T> {
    if values.len() == 1 {
        values.remove(0)
    } else {
        Filter::And(values)
    }
}

fn filter<T>(
    schema: &DocumentSchema,
    target: &str,
    value: &C,
    pos: Pos,
    depth: usize,
    scalar: &mut impl FnMut(&C, &str, Pos) -> Result<T, Error>,
) -> Result<Filter<T>, Error> {
    if depth > MAX_DOCUMENT_DEPTH {
        return Err(error(pos, "input nesting limit exceeded"));
    }
    let C::Object(values) = value else {
        return Err(error(pos, "expected filter object"));
    };
    let mut out = Vec::new();
    for (field, value) in values {
        match field.as_str() {
            "_and" | "_or" => {
                let C::List(values) = value else {
                    return Err(error(pos, "boolean list cannot be null"));
                };
                let children = values
                    .iter()
                    .map(|v| filter(schema, target, v, pos, depth + 1, scalar))
                    .collect::<Result<_, _>>()?;
                out.push(if field == "_and" {
                    Filter::And(children)
                } else {
                    Filter::Or(children)
                });
            }
            "_not" => out.push(Filter::Not(Box::new(filter(
                schema,
                target,
                value,
                pos,
                depth + 1,
                scalar,
            )?))),
            _ => {
                let TypeKind::InputObject(input) = &schema.types[&format!("{target}_bool_exp")]
                else {
                    unreachable!()
                };
                let spec = input
                    .fields
                    .iter()
                    .find(|f| f.node.name.node == *field)
                    .unwrap();
                let ty = named(&spec.node.ty.node);
                if let Some(related) = ty.strip_suffix("_bool_exp") {
                    out.push(Filter::Relation {
                        field: field.to_string(),
                        filter: Box::new(filter(schema, related, value, pos, depth + 1, scalar)?),
                    });
                } else {
                    let ty = ty.strip_suffix("_comparison_exp").unwrap();
                    let C::Object(ops) = value else {
                        return Err(error(pos, "comparison object cannot be null"));
                    };
                    for (op, value) in ops {
                        let field = field.to_string();
                        out.push(match op.as_str() {
                            "_is_null" => {
                                let C::Boolean(value) = value else {
                                    return Err(error(pos, "_is_null requires a boolean"));
                                };
                                Filter::IsNull {
                                    field,
                                    value: *value,
                                }
                            }
                            "_in" | "_nin" => {
                                let C::List(values) = value else {
                                    return Err(error(pos, "membership requires a list"));
                                };
                                Filter::In {
                                    field,
                                    negated: op == "_nin",
                                    values: values
                                        .iter()
                                        .map(|v| scalar(v, ty, pos))
                                        .collect::<Result<_, _>>()?,
                                }
                            }
                            _ => Filter::Compare {
                                field,
                                value: scalar(value, ty, pos)?,
                                op: match op.as_str() {
                                    "_eq" => Comparison::Eq,
                                    "_neq" => Comparison::Neq,
                                    "_gt" => Comparison::Gt,
                                    "_gte" => Comparison::Gte,
                                    "_lt" => Comparison::Lt,
                                    "_lte" => Comparison::Lte,
                                    "_like" => Comparison::Like,
                                    "_nlike" => Comparison::Nlike,
                                    "_ilike" => Comparison::Ilike,
                                    "_nilike" => Comparison::Nilike,
                                    _ => unreachable!(),
                                },
                            },
                        });
                    }
                }
            }
        }
    }
    Ok(conjunction(out))
}

type PermissionFilter = (Filter<crate::Operand>, Vec<(String, String)>);

pub(super) fn permission_filter(
    schema: &DocumentSchema,
    target: &str,
    value: &C,
    pos: Pos,
) -> Result<PermissionFilter, Error> {
    let ty = Type::new(&format!("{target}_bool_exp!")).unwrap();
    let value = coerce_input(schema, value.clone(), &ty, false, pos, 0, true)?;
    let mut types = Vec::new();
    let filter = filter(schema, target, &value, pos, 0, &mut |value, ty, pos| {
        if let C::String(text) = value
            && let Some(literal) = text.strip_prefix("$$")
        {
            return scalar(&C::String(format!("${literal}")), ty, pos).map(crate::Operand::Literal);
        }
        if let C::String(name) = value
            && let Some(name) = name.strip_prefix('$')
        {
            if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                return Err(error(
                    pos,
                    "session names require letters, digits or underscores after $",
                ));
            }
            types.push((name.to_string(), ty.to_string()));
            return Ok(crate::Operand::Session(name.to_string()));
        }
        scalar(value, ty, pos).map(crate::Operand::Literal)
    })?;
    Ok((filter, types))
}

pub(super) fn check_session(
    schema: &DocumentSchema,
    session: &crate::Session,
) -> Result<(), Error> {
    for (role, name, ty) in &schema.session_types {
        if role != &session.role {
            continue;
        }
        let Some(value) = session.variables.get(name) else {
            continue;
        };
        let value = match value {
            Value::Int(v) => C::Number((*v).into()),
            Value::Float(v) => C::Number(
                serde_json::Number::from_f64(*v)
                    .ok_or_else(|| error(Pos::default(), "session floats must be finite"))?,
            ),
            Value::Text(v) => C::String(v.clone()),
            Value::Bool(v) => C::Boolean(*v),
            Value::Null => C::Null,
        };
        coerce(
            schema,
            value,
            &Type::new(&format!("{ty}!")).unwrap(),
            true,
            Pos::default(),
            0,
        )?;
    }
    Ok(())
}

fn order(value: &C, pos: Pos) -> Result<Vec<Order>, Error> {
    let C::List(values) = value else {
        return Err(error(pos, "expected order list"));
    };
    let mut out = Vec::new();
    for value in values {
        let C::Object(fields) = value else {
            return Err(error(pos, "expected order object"));
        };
        for (field, value) in fields {
            if value == &C::Null {
                continue;
            }
            let C::Enum(direction) = value else {
                return Err(error(pos, "expected order_by enum"));
            };
            out.push(Order {
                field: field.to_string(),
                direction: match direction.as_str() {
                    "asc" => Direction::Asc,
                    "desc" => Direction::Desc,
                    "asc_nulls_first" => Direction::AscNullsFirst,
                    "asc_nulls_last" => Direction::AscNullsLast,
                    "desc_nulls_first" => Direction::DescNullsFirst,
                    "desc_nulls_last" => Direction::DescNullsLast,
                    _ => unreachable!(),
                },
            });
        }
    }
    Ok(out)
}

pub(super) fn parse(
    schema: &DocumentSchema,
    query: &str,
    variables: &serde_json::Value,
    operation_name: Option<&str>,
) -> Result<Vec<Selection>, Error> {
    bounded(query)?;
    let mut pending = vec![(variables, 0usize)];
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_DOCUMENT_DEPTH || bytes > MAX_DOCUMENT_BYTES {
            return Err(error(Pos::default(), "variables exceed the input limits"));
        }
        bytes += 1;
        match value {
            serde_json::Value::Array(values) => {
                pending.extend(values.iter().map(|v| (v, depth + 1)))
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    bytes += key.len();
                    pending.push((value, depth + 1));
                }
            }
            serde_json::Value::String(value) => bytes += value.len(),
            _ => {}
        }
    }
    if bytes > MAX_DOCUMENT_BYTES {
        return Err(error(Pos::default(), "variables exceed the input limits"));
    }
    if serde_json::to_vec(variables)
        .map_err(|e| error(Pos::default(), e.to_string()))?
        .len()
        > MAX_DOCUMENT_BYTES
    {
        return Err(error(Pos::default(), "variables exceed the byte limit"));
    }
    let document = parse_query(query).map_err(syntax)?;
    let operation = if let Some(name) = operation_name {
        document
            .operations
            .iter()
            .find(|(n, _)| n.is_some_and(|n| n == name))
            .map(|(_, op)| op)
    } else if document.operations.iter().len() == 1 {
        document.operations.iter().next().map(|(_, op)| op)
    } else {
        None
    }
    .ok_or_else(|| {
        error(
            Pos::default(),
            "provide the name of an existing query operation",
        )
    })?;
    if operation.node.ty == OperationType::Subscription || !operation.node.directives.is_empty() {
        return Err(error(
            operation.pos,
            "query and mutation operations without operation directives are supported",
        ));
    }
    let values = variables
        .as_object()
        .ok_or_else(|| error(operation.pos, "variables must be a JSON object"))?;
    let mut context = Context {
        schema,
        document: &document,
        variables: BTreeMap::new(),
        visiting: BTreeSet::new(),
        budget: 100_000,
    };
    for v in &operation.node.variable_definitions {
        let name = v.node.name.node.to_string();
        if context.variables.contains_key(&name) || !v.node.directives.is_empty() {
            return Err(error(
                v.pos,
                "repeated variable or unsupported variable directive",
            ));
        }
        let ty = &v.node.var_type.node;
        if !matches!(
            schema.types.get(named(ty)),
            Some(TypeKind::Scalar | TypeKind::Enum(_) | TypeKind::InputObject(_))
        ) {
            return Err(error(v.pos, "variable requires an input type"));
        }
        let default = v
            .node
            .default_value
            .as_ref()
            .map(|d| coerce(schema, d.node.clone(), ty, false, d.pos, 0))
            .transpose()?;
        let default_nonnull = default.as_ref().is_some_and(|d| *d != C::Null);
        let value = if let Some(value) = values.get(&name) {
            Some(coerce(
                schema,
                C::from_json(value.clone()).map_err(|e| error(v.pos, e.to_string()))?,
                ty,
                true,
                v.pos,
                0,
            )?)
        } else {
            default
        };
        if value.is_none() && !ty.nullable {
            return Err(error(
                v.pos,
                format!("required variable ${name} is missing"),
            ));
        }
        context.variables.insert(
            name,
            Variable {
                ty: ty.clone(),
                value,
                default_nonnull,
            },
        );
    }
    context.fields(
        &operation.node.selection_set,
        if operation.node.ty == OperationType::Mutation {
            "Mutation"
        } else {
            &schema.query
        },
        0,
    )
}

fn row_values(
    schema: &DocumentSchema,
    target: &str,
    value: &C,
    pos: Pos,
) -> Result<Vec<(String, Value)>, Error> {
    let C::Object(values) = value else {
        return Err(error(pos, "expected row object"));
    };
    values
        .iter()
        .map(|(f, v)| {
            let ty = schema
                .row_types
                .get(&(target.to_string(), f.to_string()))
                .ok_or_else(|| error(pos, "unknown scalar column"))?;
            Ok((f.to_string(), scalar(v, ty, pos)?))
        })
        .collect()
}

pub(super) fn preset(
    schema: &DocumentSchema,
    target: &str,
    field: &str,
    value: &C,
    pos: Pos,
) -> Result<(crate::Operand, String), Error> {
    let TypeKind::Object(object) = &schema.types[target] else {
        unreachable!()
    };
    let f = object
        .fields
        .iter()
        .find(|f| {
            f.node.name.node == field
                && matches!(
                    schema.types.get(named(&f.node.ty.node)),
                    Some(TypeKind::Scalar)
                )
        })
        .ok_or_else(|| error(pos, "preset requires a scalar column"))?;
    let ty = named(&f.node.ty.node).to_string();
    if let C::String(text) = value {
        if let Some(text) = text.strip_prefix("$$") {
            let v = C::String(format!("${text}"));
            coerce(schema, v.clone(), &f.node.ty.node, false, pos, 0)?;
            return Ok((crate::Operand::Literal(scalar(&v, &ty, pos)?), ty));
        }
        if let Some(name) = text.strip_prefix('$') {
            if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                return Err(error(pos, "invalid session variable"));
            }
            return Ok((crate::Operand::Session(name.into()), ty));
        }
    }
    coerce(schema, value.clone(), &f.node.ty.node, false, pos, 0)?;
    Ok((crate::Operand::Literal(scalar(value, &ty, pos)?), ty))
}
