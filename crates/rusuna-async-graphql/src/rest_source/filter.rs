use super::*;
use chrono::{DateTime, FixedOffset};
use rusuna::{Comparison, Direction, Filter, Operand, RestCatalog, RestColumn};
use std::borrow::Cow;
use std::cmp::Ordering;

pub(super) struct Plan<'a> {
    pub field: &'a RestField,
    permission: Option<Predicate<'a>>,
    filter: Option<Predicate<'a>>,
    order: Vec<(&'a RestColumn, Direction)>,
    offset: usize,
    limit: usize,
    permission_limit: usize,
}

impl<'a> Plan<'a> {
    pub(super) fn new(
        shared: &'a Shared,
        ctx: &ResolverContext<'_>,
        field: &'a RestField,
        session: &rusuna::Session,
    ) -> async_graphql::Result<Self> {
        let target = named(&field.output);
        let catalog = shared.schema.rest_catalog();
        let permission = catalog.permission(target, &session.role);
        let predicate = permission
            .map(|permission| {
                Predicate::compile(catalog, target, &permission.filter, &|value| match value {
                    Operand::Literal(value) => Ok(value),
                    Operand::Session(name) => {
                        let value = if name.starts_with("x-hasura-") {
                            let mut matches = session
                                .variables
                                .iter()
                                .filter(|(key, _)| key.eq_ignore_ascii_case(name));
                            let value = matches.next().map(|(_, value)| value);
                            if matches.next().is_some() {
                                return Err(invalid(format!("ambiguous session variable {name}")));
                            }
                            value
                        } else {
                            session.variables.get(name)
                        };
                        value.ok_or_else(|| invalid(format!("missing session variable {name}")))
                    }
                })
            })
            .transpose()?;
        let mut plan = Self {
            field,
            permission: predicate,
            filter: None,
            order: Vec::new(),
            offset: 0,
            limit: usize::MAX,
            permission_limit: permission
                .and_then(|p| p.limit)
                .and_then(|limit| usize::try_from(limit).ok())
                .unwrap_or(usize::MAX),
        };
        if field.filter {
            let selection = shared
                .schema
                .rest_selection(
                    target,
                    ctx.args
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_value().clone())),
                )
                .map_err(|error| invalid(error.to_string()))?;
            plan.filter = selection
                .filter
                .as_ref()
                .map(|filter| Predicate::compile(catalog, target, filter, &|value| Ok(value)))
                .transpose()?;
            plan.order = selection
                .order_by
                .into_iter()
                .map(|order| (&catalog.objects[target][&order.field], order.direction))
                .collect();
            plan.offset = selection.offset.unwrap_or(0) as usize;
            plan.limit = selection.limit.map_or(usize::MAX, |limit| limit as usize);
        }
        Ok(plan)
    }

    pub(super) fn active(&self) -> bool {
        self.field.filter || self.permission.is_some()
    }

    pub(super) fn apply(&self, value: Json, ty: &Type) -> Json {
        let accepts = |predicate: Option<&Predicate<'_>>, row: &Json| {
            predicate.is_none_or(|predicate| predicate.matches(row) == Some(true))
        };
        match (value, &ty.base) {
            (Json::Array(values), BaseType::List(item))
                if matches!(item.base, BaseType::List(_)) =>
            {
                Json::Array(
                    values
                        .into_iter()
                        .map(|value| self.apply(value, item))
                        .collect(),
                )
            }
            (Json::Array(mut rows), BaseType::List(_)) => {
                rows.retain(|row| accepts(self.permission.as_ref(), row));
                rows.retain(|row| {
                    (!self.field.aggregate || !row.is_null()) && accepts(self.filter.as_ref(), row)
                });
                if !self.order.is_empty() {
                    rows.sort_by(|a, b| {
                        self.order
                            .iter()
                            .map(|(column, direction)| row_order(column, *direction, a, b))
                            .find(|order| *order != Ordering::Equal)
                            .unwrap_or(Ordering::Equal)
                    });
                }
                Json::Array(
                    rows.into_iter()
                        .skip(self.offset)
                        .take(self.limit.min(self.permission_limit))
                        .collect(),
                )
            }
            (value, BaseType::Named(_)) if !accepts(self.permission.as_ref(), &value) => Json::Null,
            (value, _) => value,
        }
    }
}

enum Predicate<'a> {
    Compare(&'a RestColumn, Comparison, Scalar<'static>),
    Like(&'a RestColumn, Pattern),
    In(&'a RestColumn, bool, Vec<Scalar<'static>>),
    IsNull(&'a RestColumn, bool),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
    Nested(&'a RestColumn, Box<Self>),
}

impl<'a> Predicate<'a> {
    fn compile<'v, V: 'v>(
        catalog: &'a RestCatalog,
        target: &str,
        filter: &'v Filter<V>,
        bind: &impl Fn(&'v V) -> async_graphql::Result<&'v rusuna::Value>,
    ) -> async_graphql::Result<Self> {
        let columns = &catalog.objects[target];
        let scalar = |column: &RestColumn, value| {
            let ty = named(&column.ty);
            let value = bind(value)?;
            operand(catalog, ty, value)
                .ok_or_else(|| invalid(format!("invalid comparison operand for {ty}")))
        };
        Ok(match filter {
            Filter::Compare { field, op, value } => {
                let column = &columns[field];
                let value = scalar(column, value)?;
                match (op, value) {
                    (
                        Comparison::Like
                        | Comparison::Nlike
                        | Comparison::Ilike
                        | Comparison::Nilike,
                        Scalar::Text(text),
                    ) => Self::Like(column, Pattern::new(*op, &text)?),
                    (_, value) => Self::Compare(column, *op, value),
                }
            }
            Filter::In {
                field,
                negated,
                values,
            } => {
                let column = &columns[field];
                Self::In(
                    column,
                    *negated,
                    values
                        .iter()
                        .map(|value| scalar(column, value))
                        .collect::<Result<_, _>>()?,
                )
            }
            Filter::IsNull { field, value } => Self::IsNull(&columns[field], *value),
            Filter::And(filters) | Filter::Or(filters) => {
                let children = filters
                    .iter()
                    .map(|filter| Self::compile(catalog, target, filter, bind))
                    .collect::<Result<_, _>>()?;
                if matches!(filter, Filter::And(_)) {
                    Self::And(children)
                } else {
                    Self::Or(children)
                }
            }
            Filter::Not(filter) => {
                Self::Not(Box::new(Self::compile(catalog, target, filter, bind)?))
            }
            Filter::Relation { field, filter } => {
                let column = &columns[field];
                Self::Nested(
                    column,
                    Box::new(Self::compile(catalog, named(&column.ty), filter, bind)?),
                )
            }
        })
    }

    fn matches(&self, row: &Json) -> Option<bool> {
        let value = |column: &RestColumn| row.get(&column.key).unwrap_or(&Json::Null);
        let scalar = |column: &RestColumn| response_scalar(named(&column.ty), value(column));
        match self {
            Self::Compare(column, op, right) => {
                let order = scalar(column)?.compare(right);
                Some(match op {
                    Comparison::Eq => order.is_eq(),
                    Comparison::Neq => order.is_ne(),
                    Comparison::Gt => order.is_gt(),
                    Comparison::Gte => order.is_ge(),
                    Comparison::Lt => order.is_lt(),
                    Comparison::Lte => order.is_le(),
                    _ => unreachable!(),
                })
            }
            Self::Like(column, pattern) => Some(pattern.matches(value(column).as_str()?)),
            Self::In(_, negated, values) if values.is_empty() => Some(*negated),
            Self::In(column, negated, values) => {
                let value = scalar(column)?;
                Some(values.iter().any(|right| value.compare(right).is_eq()) != *negated)
            }
            Self::IsNull(column, is_null) => Some(value(column).is_null() == *is_null),
            Self::And(filters) => Self::boolean(filters, row, false),
            Self::Or(filters) => Self::boolean(filters, row, true),
            Self::Not(filter) => filter.matches(row).map(|value| !value),
            Self::Nested(column, filter) => {
                let value = value(column);
                Some(!value.is_null() && filter.matches(value) == Some(true))
            }
        }
    }

    fn boolean(filters: &[Self], row: &Json, decisive: bool) -> Option<bool> {
        let mut result = Some(!decisive);
        for filter in filters {
            match filter.matches(row) {
                Some(value) if value == decisive => return Some(decisive),
                None => result = None,
                _ => {}
            }
        }
        result
    }
}

enum Scalar<'a> {
    Number(f64),
    Text(Cow<'a, str>),
    Boolean(bool),
    Instant(DateTime<FixedOffset>),
}

impl Scalar<'_> {
    fn compare(&self, other: &Scalar<'_>) -> Ordering {
        match (self, other) {
            (Self::Number(a), Scalar::Number(b)) => a.partial_cmp(b).unwrap(),
            (Self::Text(a), Scalar::Text(b)) => a.cmp(b),
            (Self::Boolean(a), Scalar::Boolean(b)) => a.cmp(b),
            (Self::Instant(a), Scalar::Instant(b)) => a.cmp(b),
            _ => unreachable!(),
        }
    }
}

pub(super) fn date_time(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

fn response_scalar<'a>(ty: &str, value: &'a Json) -> Option<Scalar<'a>> {
    match ty {
        "Int" | "Float" => value.as_f64().map(Scalar::Number),
        "Boolean" => value.as_bool().map(Scalar::Boolean),
        "DateTime" => date_time(value.as_str()?).map(Scalar::Instant),
        "ID" if value.is_number() => {
            let n = value.as_number()?;
            let text = if n.is_i64() || n.is_u64() {
                n.to_string()
            } else {
                format!("{:.0}", n.as_f64()?)
            };
            Some(Scalar::Text(Cow::Owned(text)))
        }
        _ => value
            .as_str()
            .map(|value| Scalar::Text(Cow::Borrowed(value))),
    }
}

fn operand(catalog: &RestCatalog, ty: &str, value: &rusuna::Value) -> Option<Scalar<'static>> {
    use rusuna::Value::*;
    match (ty, value) {
        ("Int", Int(value)) if i32::try_from(*value).is_ok() => Some(Scalar::Number(*value as f64)),
        ("Float", Int(value)) => Some(Scalar::Number(*value as f64)),
        ("Float", Float(value)) if value.is_finite() => Some(Scalar::Number(*value)),
        ("Boolean", Bool(value)) => Some(Scalar::Boolean(*value)),
        ("DateTime", Text(value)) => date_time(value).map(Scalar::Instant),
        ("ID", Int(value)) => Some(Scalar::Text(Cow::Owned(value.to_string()))),
        ("ID" | "String", Text(value)) => Some(Scalar::Text(Cow::Owned(value.clone()))),
        (ty, Text(value))
            if catalog
                .enums
                .get(ty)
                .is_some_and(|values| values.contains(value)) =>
        {
            Some(Scalar::Text(Cow::Owned(value.clone())))
        }
        _ => None,
    }
}

fn row_order(column: &RestColumn, direction: Direction, a: &Json, b: &Json) -> Ordering {
    let ty = named(&column.ty);
    let a = a
        .get(&column.key)
        .and_then(|value| response_scalar(ty, value));
    let b = b
        .get(&column.key)
        .and_then(|value| response_scalar(ty, value));
    let nulls_first = matches!(
        direction,
        Direction::AscNullsFirst | Direction::Desc | Direction::DescNullsFirst
    );
    let descending = matches!(
        direction,
        Direction::Desc | Direction::DescNullsFirst | Direction::DescNullsLast
    );
    match (a, b) {
        (Some(a), Some(b)) if descending => b.compare(&a),
        (Some(a), Some(b)) => a.compare(&b),
        (None, None) => Ordering::Equal,
        (None, Some(_)) if nulls_first => Ordering::Less,
        (Some(_), None) if nulls_first => Ordering::Greater,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
    }
}

#[derive(PartialEq)]
enum PatternPart {
    Literal(char),
    One,
    Many,
}

struct Pattern {
    parts: Vec<PatternPart>,
    insensitive: bool,
    negated: bool,
}

impl Pattern {
    fn new(op: Comparison, text: &str) -> async_graphql::Result<Self> {
        let insensitive = matches!(op, Comparison::Ilike | Comparison::Nilike);
        let pattern = if insensitive {
            Cow::Owned(text.to_lowercase())
        } else {
            Cow::Borrowed(text)
        };
        let mut chars = pattern.chars();
        let mut parts = Vec::new();
        while let Some(ch) = chars.next() {
            parts.push(match ch {
                '%' => PatternPart::Many,
                '_' => PatternPart::One,
                '\\' => PatternPart::Literal(
                    chars
                        .next()
                        .ok_or_else(|| invalid("LIKE pattern ends with an escape"))?,
                ),
                ch => PatternPart::Literal(ch),
            });
        }
        Ok(Self {
            parts,
            insensitive,
            negated: matches!(op, Comparison::Nlike | Comparison::Nilike),
        })
    }

    fn matches(&self, text: &str) -> bool {
        let text = if self.insensitive {
            Cow::Owned(text.to_lowercase())
        } else {
            Cow::Borrowed(text)
        };
        let mut remaining = text.chars();
        let mut retry = None;
        let mut part = 0;
        while let Some(ch) = remaining.next() {
            match self.parts.get(part) {
                Some(PatternPart::One) => part += 1,
                Some(PatternPart::Literal(literal)) if ch == *literal => part += 1,
                Some(PatternPart::Many) => {
                    part += 1;
                    let start = text.len() - remaining.as_str().len() - ch.len_utf8();
                    retry = Some((part, text[start..].chars()));
                    remaining = text[start..].chars();
                }
                _ => {
                    let Some((part_after_many, tail)) = &mut retry else {
                        return self.negated;
                    };
                    if tail.next().is_none() {
                        return self.negated;
                    }
                    part = *part_after_many;
                    remaining = tail.clone();
                }
            }
        }
        self.parts[part..]
            .iter()
            .all(|part| *part == PatternPart::Many)
            != self.negated
    }
}

pub(super) fn aggregate_field(
    shared: &Arc<Shared>,
    owner: &str,
    definition: &FieldDefinition,
) -> Option<Field> {
    let catalog = shared.schema.rest_catalog();
    let (target, operation) = [
        ("_aggregate", ""),
        ("_aggregate_fields", ""),
        ("_sum_fields", "sum"),
        ("_avg_fields", "avg"),
        ("_min_fields", "min"),
        ("_max_fields", "max"),
    ]
    .into_iter()
    .find_map(|(suffix, op)| {
        let target = owner.strip_suffix(suffix)?;
        catalog
            .aggregate_types
            .contains(target)
            .then_some((target, op))
    })?;
    let shared = shared.clone();
    let target = target.to_owned();
    let name = definition.name.node.to_string();
    let ty = definition.ty.node.clone();
    Some(Field::new(name.clone(), type_ref(&ty), move |ctx| {
        let shared = shared.clone();
        let name = name.clone();
        let target = target.clone();
        let ty = ty.clone();
        FieldFuture::new(async move {
            let rows = ctx.parent_value.try_downcast_ref::<Arc<Json>>()?;
            match (operation, name.as_str()) {
                ("", "nodes") if matches!(ty.base, BaseType::List(_)) => {
                    shared.rest.complete(&shared, &ctx, &ty, rows, None).await
                }
                ("", "count") => {
                    let count = i32::try_from(rows.as_array().unwrap().len())
                        .map_err(|_| invalid("REST aggregate count exceeds Int"))?;
                    Ok(Some(FieldValue::value(count)))
                }
                ("", _) => Ok(Some(FieldValue::owned_any(rows.clone()))),
                (operation, _) => {
                    let catalog = shared.schema.rest_catalog();
                    let column = &catalog.objects[&target][&name];
                    metric(&shared.rest, rows.as_array().unwrap(), column, operation)
                        .map(|value| value.map(FieldValue::value))
                }
            }
        })
    }))
}

fn metric(
    rest: &RestRuntime,
    rows: &[Json],
    column: &RestColumn,
    operation: &str,
) -> async_graphql::Result<Option<Value>> {
    let ty = named(&column.ty);
    let values = rows
        .iter()
        .filter_map(|row| row.get(&column.key).filter(|value| !value.is_null()));
    if matches!(operation, "min" | "max") {
        let best = values.reduce(|best, value| {
            let order = response_scalar(ty, value)
                .unwrap()
                .compare(&response_scalar(ty, best).unwrap());
            if (operation == "min" && order.is_lt()) || (operation == "max" && order.is_gt()) {
                value
            } else {
                best
            }
        });
        return Ok(best.and_then(|value| rest.scalar(ty, value)));
    }
    let (sum, count) = values.fold((0.0, 0), |(sum, count), value| {
        (sum + value.as_f64().unwrap(), count + 1)
    });
    if count == 0 {
        return Ok(None);
    }
    let result = if operation == "avg" {
        sum / f64::from(count)
    } else {
        sum
    };
    let json = serde_json::Number::from_f64(result).map(Json::Number);
    let output_ty = if operation == "avg" { "Float" } else { ty };
    json.and_then(|value| rest.scalar(output_ty, &value))
        .map(Some)
        .ok_or_else(|| invalid(format!("REST aggregate {operation} exceeds {output_ty}")))
}
