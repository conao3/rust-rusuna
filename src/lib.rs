//! Compiles GraphQL documents to one PostgreSQL statement per root field.
//!
//! ```
//! use rusuna::Schema;
//!
//! let schema = Schema::from_sdl(r#"
//!     type Author @table(name: "author") {
//!         id: Int!
//!         name: String!
//!     }
//!     type Query { authors: [Author!]! @list }
//! "#).unwrap();
//! let fields = schema.compile(
//!     "query Authors($limit: Int = 10) { authors(limit: $limit) { id name } }",
//!     &serde_json::json!({}),
//!     Some("Authors"),
//! ).unwrap();
//! assert_eq!(fields[0].response_key, "authors");
//! assert!(fields[0].compiled.sql.starts_with("WITH"));
//! assert!(fields[0].compiled.params.is_empty());
//! ```

mod compiler;
mod graphql;

pub use graphql::{CompiledField, MAX_DOCUMENT_BYTES, MAX_DOCUMENT_DEPTH};

use compiler::model;
use std::fmt;

/// A value of a query, sent to the database as a statement parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Text(String),
    Bool(bool),
    Float(f64),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Ascending, nulls last.
    Asc,
    /// Descending, nulls first.
    Desc,
    AscNullsFirst,
    AscNullsLast,
    DescNullsFirst,
    DescNullsLast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// At most one related row; the field is an object or null.
    Object,
    /// Any number of related rows; the field is a list.
    Array,
    Aggregate,
}

/// A field backed by a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub field: String,
    pub column: String,
}

impl Column {
    pub fn new(field: impl Into<String>, column: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            column: column.into(),
        }
    }
}

/// A field backed by the rows of `target` whose `references` columns equal the `columns` of
/// this table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub field: String,
    pub kind: Kind,
    pub target: String,
    pub columns: Vec<String>,
    pub references: Vec<String>,
}

impl Relation {
    pub fn new<C: Into<String>, R: Into<String>>(
        field: impl Into<String>,
        kind: Kind,
        target: impl Into<String>,
        columns: impl IntoIterator<Item = C>,
        references: impl IntoIterator<Item = R>,
    ) -> Self {
        Self {
            field: field.into(),
            kind,
            target: target.into(),
            columns: columns.into_iter().map(Into::into).collect(),
            references: references.into_iter().map(Into::into).collect(),
        }
    }
}

/// A table exposed under the GraphQL type `name` and targeted by roots and relations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<Column>,
    pub relations: Vec<Relation>,
}

impl Table {
    pub fn new(
        name: impl Into<String>,
        schema: impl Into<String>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            schema: schema.into(),
            table: table.into(),
            columns: Vec::new(),
            relations: Vec::new(),
        }
    }

    pub fn column(mut self, field: impl Into<String>, column: impl Into<String>) -> Self {
        self.columns.push(Column::new(field, column));
        self
    }

    pub fn relation(mut self, relation: Relation) -> Self {
        self.relations.push(relation);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Like,
    Nlike,
    Ilike,
    Nilike,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Compare {
        field: String,
        op: Comparison,
        value: Value,
    },
    In {
        field: String,
        negated: bool,
        values: Vec<Value>,
    },
    IsNull {
        field: String,
        value: bool,
    },
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    Relation {
        field: String,
        filter: Box<Filter>,
    },
}

impl Filter {
    pub fn compare(field: impl Into<String>, op: Comparison, value: Value) -> Self {
        Self::Compare {
            field: field.into(),
            op,
            value,
        }
    }

    pub fn eq(field: impl Into<String>, value: Value) -> Self {
        Self::compare(field, Comparison::Eq, value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub field: String,
    pub direction: Direction,
}

/// A field of a query with its arguments and its own selection.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub alias: String,
    pub name: String,
    pub filter: Option<Filter>,
    pub order_by: Vec<Order>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub fields: Vec<Selection>,
}

impl Selection {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            alias: name.clone(),
            name,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            fields: Vec::new(),
        }
    }

    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = alias.into();
        self
    }

    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn order_by(mut self, field: impl Into<String>, direction: Direction) -> Self {
        self.order_by.push(Order {
            field: field.into(),
            direction,
        });
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn field(mut self, field: Selection) -> Self {
        self.fields.push(field);
        self
    }
}

/// One SQL statement and its parameters, in the order of `$1`, `$2`, ….
///
/// The statement returns one row with one `json` column: a list or an aggregate object.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub sql: String,
    pub params: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Document {
        message: String,
        line: usize,
        column: usize,
    },
    /// No table has this name.
    UnknownType {
        name: String,
    },
    UnknownRoot {
        name: String,
    },
    /// The table has no column or relation with this field name.
    UnknownField {
        table: String,
        field: String,
    },
    /// A column was given arguments or a selection.
    ColumnArguments {
        table: String,
        field: String,
    },
    /// An object relation was given arguments.
    ObjectArguments {
        table: String,
        field: String,
    },
    /// The relation has no columns, or `columns` and `references` differ in length.
    InvalidRelation {
        table: String,
        field: String,
    },
    /// The selection on this table is empty or too large, or an alias is empty, longer than
    /// 63 bytes, repeated or reserved.
    InvalidSelection {
        table: String,
    },
    /// The query or filter exceeds its nesting limit.
    TooDeep,
    InvalidFilter,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Document {
                message,
                line,
                column,
            } => write!(f, "{line}:{column}: {message}"),
            Self::UnknownRoot { name } => write!(f, "no root field is named `{name}`"),
            Self::UnknownType { name } => write!(f, "no table is named `{name}`"),
            Self::UnknownField { table, field } => write!(f, "`{table}` has no field `{field}`"),
            Self::ColumnArguments { table, field } => {
                write!(
                    f,
                    "the column `{table}.{field}` takes neither arguments nor a selection"
                )
            }
            Self::ObjectArguments { table, field } => {
                write!(
                    f,
                    "the object relation `{table}.{field}` takes no arguments"
                )
            }
            Self::InvalidRelation { table, field } => {
                write!(
                    f,
                    "the relation `{table}.{field}` needs as many columns as references, and at least one"
                )
            }
            Self::InvalidSelection { table } => write!(
                f,
                "the selection on `{table}` needs 1 to {MAX_FIELDS} fields with distinct aliases of 1 to 63 bytes"
            ),
            Self::InvalidFilter => write!(
                f,
                "the filter contains null, a nonfinite float, or too many entries"
            ),
            Self::TooDeep => write!(
                f,
                "the query or filter exceeds its nesting limit ({MAX_DEPTH} relation levels, {MAX_FILTER_DEPTH} filter edges)"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// The deepest nesting of relations a query may have.
pub const MAX_DEPTH: usize = 32;

/// The largest number of fields one selection may have.
pub const MAX_FIELDS: usize = 1000;
pub const MAX_FILTER_DEPTH: usize = 32;
pub const MAX_IN_VALUES: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    List,
    Aggregate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootField {
    pub field: String,
    pub target: String,
    pub kind: RootKind,
}

impl RootField {
    pub fn new(field: impl Into<String>, target: impl Into<String>, kind: RootKind) -> Self {
        Self {
            field: field.into(),
            target: target.into(),
            kind,
        }
    }
}

/// The tables and relations queries are compiled against.
#[derive(Debug, Clone)]
pub struct Schema {
    model: model::Schema,
    document: Option<graphql::DocumentSchema>,
}

impl Schema {
    pub fn new(tables: Vec<Table>, roots: Vec<RootField>) -> Self {
        Self {
            document: None,
            model: model::Schema {
                tables: tables.into_iter().map(table_model).collect(),
                roots: roots
                    .into_iter()
                    .map(|root| model::RootField {
                        field: root.field,
                        target: root.target,
                        kind: match root.kind {
                            RootKind::List => model::RootKind::List,
                            RootKind::Aggregate => model::RootKind::Aggregate,
                        },
                    })
                    .collect(),
            },
        }
    }

    /// Compiles the root field `root` into one statement.
    pub fn compile_selection(&self, root: &Selection) -> Result<Compiled, Error> {
        let root = selection_model(root, 0)?;
        match model::run(self.model.clone(), root) {
            Ok(compiled) => Ok(Compiled {
                sql: compiled.sql,
                params: compiled.params.into_iter().map(value).collect(),
            }),
            Err(error) => Err(error_from_model(error)),
        }
    }
}

fn table_model(table: Table) -> model::Table {
    model::Table {
        name: table.name,
        schema: table.schema,
        table: table.table,
        columns: table
            .columns
            .into_iter()
            .map(|column| model::Column {
                field: column.field,
                column: column.column,
            })
            .collect(),
        relations: table
            .relations
            .into_iter()
            .map(|relation| model::Relation {
                field: relation.field,
                kind: match relation.kind {
                    Kind::Object => model::Kind::Object,
                    Kind::Array => model::Kind::Array,
                    Kind::Aggregate => model::Kind::Aggregate,
                },
                target: relation.target,
                parent: relation.columns,
                child: relation.references,
            })
            .collect(),
    }
}

fn selection_model(selection: &Selection, depth: usize) -> Result<model::Selection, Error> {
    if depth > MAX_DEPTH + 4 {
        return Err(Error::TooDeep);
    }
    Ok(model::Selection {
        alias: selection.alias.clone(),
        name: selection.name.clone(),
        conds: selection
            .filter
            .as_ref()
            .map(|filter| filter_model(filter, 0))
            .transpose()?,
        order: selection
            .order_by
            .iter()
            .map(|order| model::Order {
                field: order.field.clone(),
                direction: match order.direction {
                    Direction::Asc => model::Direction::Asc,
                    Direction::Desc => model::Direction::Desc,
                    Direction::DescNullsLast => model::Direction::DescNullsLast,
                    Direction::DescNullsFirst => model::Direction::DescNullsFirst,
                    Direction::AscNullsLast => model::Direction::AscNullsLast,
                    Direction::AscNullsFirst => model::Direction::AscNullsFirst,
                },
            })
            .collect(),
        limit: selection.limit.map(Into::into),
        offset: selection.offset.map(Into::into),
        children: selection
            .fields
            .iter()
            .map(|field| selection_model(field, depth + 1))
            .collect::<Result<_, _>>()?,
    })
}

fn filter_model(filter: &Filter, depth: usize) -> Result<model::ExpressionString, Error> {
    if depth > MAX_FILTER_DEPTH {
        return Err(Error::TooDeep);
    }
    Ok(match filter {
        Filter::Compare { field, op, value } => model::ExpressionString::Compare {
            field: field.clone(),
            op: match op {
                Comparison::Eq => model::Comparison::Eq,
                Comparison::Neq => model::Comparison::Neq,
                Comparison::Gt => model::Comparison::Gt,
                Comparison::Gte => model::Comparison::Gte,
                Comparison::Lt => model::Comparison::Lt,
                Comparison::Lte => model::Comparison::Lte,
                Comparison::Like => model::Comparison::Like,
                Comparison::Nlike => model::Comparison::Nlike,
                Comparison::Ilike => model::Comparison::Ilike,
                Comparison::Nilike => model::Comparison::Nilike,
            },
            value: value_model(value),
        },
        Filter::In {
            field,
            negated,
            values,
        } => model::ExpressionString::Member {
            field: field.clone(),
            negated: *negated,
            values: values.iter().map(value_model).collect(),
        },
        Filter::IsNull { field, value } => model::ExpressionString::IsNull {
            field: field.clone(),
            value: *value,
        },
        Filter::And(filters) => model::ExpressionString::And {
            children: filters
                .iter()
                .map(|f| filter_model(f, depth + 1))
                .collect::<Result<_, _>>()?,
        },
        Filter::Or(filters) => model::ExpressionString::Or {
            children: filters
                .iter()
                .map(|f| filter_model(f, depth + 1))
                .collect::<Result<_, _>>()?,
        },
        Filter::Not(filter) => model::ExpressionString::Not {
            child: Box::new(filter_model(filter, depth + 1)?),
        },
        Filter::Relation { field, filter } => model::ExpressionString::Relation {
            reference: field.clone(),
            child: Box::new(filter_model(filter, depth + 1)?),
        },
    })
}

fn value_model(value: &Value) -> model::Value {
    match value {
        Value::Int(v) => model::Value::Int { v: (*v).into() },
        Value::Text(v) => model::Value::Text { v: v.clone() },
        Value::Bool(v) => model::Value::Bool { v: *v },
        Value::Float(v) => model::Value::Float { bits: v.to_bits() },
        Value::Null => model::Value::Null,
    }
}

fn value(value: model::Value) -> Value {
    match value {
        model::Value::Int { v } => {
            Value::Int(i64::try_from(v).expect("the parameters are the values of the selection"))
        }
        model::Value::Text { v } => Value::Text(v),
        model::Value::Bool { v } => Value::Bool(v),
        model::Value::Float { bits } => Value::Float(f64::from_bits(bits)),
        model::Value::Null => Value::Null,
    }
}

fn error_from_model(error: model::Error) -> Error {
    match error {
        model::Error::UnknownRoot { name } => Error::UnknownRoot { name },
        model::Error::UnknownType { name } => Error::UnknownType { name },
        model::Error::UnknownField { r#type, field } => Error::UnknownField {
            table: r#type,
            field,
        },
        model::Error::ColumnArguments { r#type, field } => Error::ColumnArguments {
            table: r#type,
            field,
        },
        model::Error::ObjectArguments { r#type, field } => Error::ObjectArguments {
            table: r#type,
            field,
        },
        model::Error::InvalidRelation { r#type, field } => Error::InvalidRelation {
            table: r#type,
            field,
        },
        model::Error::InvalidSelection { r#type } => Error::InvalidSelection { table: r#type },
        model::Error::TooDeep => Error::TooDeep,
        model::Error::InvalidFilter => Error::InvalidFilter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_match_the_lean_definitions() {
        assert_eq!(model::max_depth(), (MAX_DEPTH as u64).into());
        assert_eq!(model::max_fields(), (MAX_FIELDS as u64).into());
        assert_eq!(model::max_filter_depth(), (MAX_FILTER_DEPTH as u64).into());
        assert_eq!(model::max_in_values(), (MAX_IN_VALUES as u64).into());
    }

    #[test]
    fn lean_rejects_filters_beyond_the_depth_limit() {
        let schema = model::Schema {
            tables: vec![],
            roots: vec![],
        };
        let table = table_model(Table::new("t", "public", "t").column("id", "id"));
        let filter = (0..=MAX_FILTER_DEPTH).fold(
            model::ExpressionString::IsNull {
                field: "id".into(),
                value: true,
            },
            |child, _| model::ExpressionString::Not {
                child: Box::new(child),
            },
        );
        assert!(matches!(
            model::resolve_expression(schema, table, 0u64.into(), filter),
            Err(model::Error::TooDeep)
        ));
    }
}
