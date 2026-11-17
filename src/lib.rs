//! Compiles a GraphQL selection over tables and relationships into one PostgreSQL statement
//! that returns the response as a single JSON value.
//!
//! ```
//! use rusuna::{Column, Direction, Kind, Relation, Schema, Selection, Table};
//!
//! let schema = Schema::new(vec![
//!     Table::new("author", "public", "author").column("id", "id").column("name", "name"),
//!     Table::new("article", "public", "article")
//!         .column("id", "id")
//!         .column("title", "title")
//!         .relation(Relation::new("author", Kind::Object, "author", ["author_id"], ["id"])),
//! ]);
//! let query = Selection::new("article")
//!     .order_by("id", Direction::Asc)
//!     .limit(10)
//!     .field(Selection::new("title"))
//!     .field(Selection::new("author").field(Selection::new("name")));
//! let compiled = schema.compile(&query).unwrap();
//! assert!(compiled.sql.starts_with("WITH"));
//! assert!(compiled.params.is_empty());
//! # let _ = Column::new("id", "id");
//! ```

mod compiler;

use compiler::model;
use std::fmt;

/// A value of a query, sent to the database as a statement parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Text(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Ascending, nulls last.
    Asc,
    /// Descending, nulls first.
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// At most one related row; the field is an object or null.
    Object,
    /// Any number of related rows; the field is a list.
    Array,
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

/// A table exposed under `name`, both as a root field and as the target of relations.
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

/// `field = value` on the rows of a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub field: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub field: String,
    pub direction: Direction,
}

/// A field of a query with its arguments and its own selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub alias: String,
    pub name: String,
    pub conditions: Vec<Condition>,
    pub order_by: Vec<Order>,
    pub limit: Option<u64>,
    pub fields: Vec<Selection>,
}

impl Selection {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            alias: name.clone(),
            name,
            conditions: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            fields: Vec::new(),
        }
    }

    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = alias.into();
        self
    }

    pub fn filter(mut self, field: impl Into<String>, value: Value) -> Self {
        self.conditions.push(Condition {
            field: field.into(),
            value,
        });
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
/// The statement returns one row with one `json` column: the list of the selected rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub sql: String,
    pub params: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No table has this name.
    UnknownType { name: String },
    /// The table has no column or relation with this field name.
    UnknownField { table: String, field: String },
    /// A column was given arguments or a selection.
    ColumnArguments { table: String, field: String },
    /// An object relation was given arguments.
    ObjectArguments { table: String, field: String },
    /// The relation has no columns, or `columns` and `references` differ in length.
    InvalidRelation { table: String, field: String },
    /// The selection on this table is empty or too large, or an alias is empty, longer than
    /// 63 bytes, repeated or reserved.
    InvalidSelection { table: String },
    /// The query nests relations deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Self::TooDeep => write!(
                f,
                "the query nests relations deeper than {MAX_DEPTH} levels"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// The deepest nesting of relations a query may have.
pub const MAX_DEPTH: usize = 32;

/// The largest number of fields one selection may have.
pub const MAX_FIELDS: usize = 1000;

/// The tables and relations queries are compiled against.
#[derive(Debug, Clone)]
pub struct Schema {
    model: model::Schema,
}

impl Schema {
    pub fn new(tables: Vec<Table>) -> Self {
        Self {
            model: model::Schema {
                tables: tables.into_iter().map(table_model).collect(),
            },
        }
    }

    /// Compiles the root field `root` into one statement.
    pub fn compile(&self, root: &Selection) -> Result<Compiled, Error> {
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
                },
                target: relation.target,
                parent: relation.columns,
                child: relation.references,
            })
            .collect(),
    }
}

fn selection_model(selection: &Selection, depth: usize) -> Result<model::Selection, Error> {
    if depth > MAX_DEPTH + 1 {
        return Err(Error::TooDeep);
    }
    Ok(model::Selection {
        alias: selection.alias.clone(),
        name: selection.name.clone(),
        conds: selection
            .conditions
            .iter()
            .map(|condition| model::Cond {
                field: condition.field.clone(),
                value: value_model(&condition.value),
            })
            .collect(),
        order: selection
            .order_by
            .iter()
            .map(|order| model::Order {
                field: order.field.clone(),
                direction: match order.direction {
                    Direction::Asc => model::Direction::Asc,
                    Direction::Desc => model::Direction::Desc,
                },
            })
            .collect(),
        limit: selection.limit.map(Into::into),
        children: selection
            .fields
            .iter()
            .map(|field| selection_model(field, depth + 1))
            .collect::<Result<_, _>>()?,
    })
}

fn value_model(value: &Value) -> model::Value {
    match value {
        Value::Int(v) => model::Value::Int { v: (*v).into() },
        Value::Text(v) => model::Value::Text { v: v.clone() },
        Value::Bool(v) => model::Value::Bool { v: *v },
    }
}

fn value(value: model::Value) -> Value {
    match value {
        model::Value::Int { v } => {
            Value::Int(i64::try_from(v).expect("the parameters are the values of the selection"))
        }
        model::Value::Text { v } => Value::Text(v),
        model::Value::Bool { v } => Value::Bool(v),
    }
}

fn error_from_model(error: model::Error) -> Error {
    match error {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_match_the_lean_definitions() {
        assert_eq!(model::max_depth(), (MAX_DEPTH as u64).into());
        assert_eq!(model::max_fields(), (MAX_FIELDS as u64).into());
    }
}
