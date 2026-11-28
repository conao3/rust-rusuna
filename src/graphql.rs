mod query;
pub(crate) mod rest;
mod schema;
mod writes;

use crate::{Compiled, Error, Schema, Selection, Session};
use async_graphql_parser::{Pos, types::*};
use serde_json::Value as Json;
use std::collections::BTreeMap;

pub const MAX_DOCUMENT_BYTES: usize = 1_048_576;
pub const MAX_DOCUMENT_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledField {
    pub response_key: String,
    pub mutation: bool,
    pub compiled: Compiled,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentSchema {
    pub types: BTreeMap<String, TypeKind>,
    pub targets: BTreeMap<(String, String), String>,
    pub sdl: String,
    pub query: String,
    pub mutation_roots: BTreeMap<String, (String, crate::WriteKind, bool, bool)>,
    pub primary_roots: BTreeMap<String, String>,
    pub row_types: BTreeMap<(String, String), String>,
    pub session_types: Vec<(String, String, String)>,
}

impl Schema {
    pub fn from_sdl(sdl: &str) -> Result<Self, Error> {
        schema::load(sdl)
    }

    pub fn expanded_sdl(&self, role: &str) -> Result<String, Error> {
        Ok(schema::for_role(
            self.document()?,
            &self.model,
            &self.writes,
            &self.rest,
            role,
        )
        .sdl)
    }

    pub fn parse_query(
        &self,
        query: &str,
        variables: &serde_json::Value,
        operation_name: Option<&str>,
        role: &str,
    ) -> Result<Vec<Selection>, Error> {
        if role == "admin" {
            return query::parse(
                self.document()?,
                &self.rest,
                query,
                variables,
                operation_name,
                None,
            );
        }
        query::parse(
            &schema::for_role(
                self.document()?,
                &self.model,
                &self.writes,
                &self.rest,
                role,
            ),
            &self.rest,
            query,
            variables,
            operation_name,
            None,
        )
    }

    pub fn compile(
        &self,
        query: &str,
        variables: &serde_json::Value,
        operation_name: Option<&str>,
        session: &Session,
    ) -> Result<Vec<CompiledField>, Error> {
        if self.rest.fields.is_empty() {
            query::check_session(self.document()?, session)?;
        }
        let selections = self.parse_query(query, variables, operation_name, &session.role)?;
        if !self.rest.fields.is_empty() {
            query::check_session(self.document()?, session)?;
        }
        selections
            .into_iter()
            .map(|selection| {
                Ok(CompiledField {
                    response_key: selection.alias.clone(),
                    mutation: selection.mutation.is_some(),
                    compiled: self.compile_selection(&selection, session)?,
                })
            })
            .collect()
    }

    pub fn rest_selection<'a>(
        &self,
        target: &str,
        arguments: impl IntoIterator<Item = (&'a str, async_graphql_value::ConstValue)>,
    ) -> Result<Selection, Error> {
        query::rest_selection(self.document()?, target, arguments)
    }

    pub fn rest_relation_query(
        &self,
        parent: &str,
        query: &str,
        variables: &Json,
        session: &Session,
    ) -> Result<RestRelationQuery<'_>, Error> {
        use crate::compiler::model;

        let document = self.document()?;
        let role = schema::for_role(
            document,
            &self.model,
            &self.writes,
            &self.rest,
            &session.role,
        );
        let mut selections = query::parse(&role, &self.rest, query, variables, None, Some(parent))?;
        let [selection] = selections.as_mut_slice() else {
            return Err(error(Pos::default(), "expected one REST relationship"));
        };
        let relation = self
            .rest
            .relations
            .get(&(parent.into(), selection.name.clone()))
            .ok_or_else(|| error(Pos::default(), "expected a REST relationship"))?;
        query::check_session(document, session)?;
        let mut schema = self.model.clone();
        schema.roots = vec![model::RootField {
            field: selection.name.clone(),
            target: relation.target.clone(),
            kind: model::RootKind::List,
            body: None,
        }];
        if !relation.list {
            selection.limit = Some(selection.limit.unwrap_or(1).min(1));
        }
        let plan = model::resolve(
            schema,
            crate::session_model(session),
            crate::selection_model(selection, 0)?,
        )
        .map_err(crate::error_from_model)?;
        Ok(RestRelationQuery {
            schema: self,
            relation,
            plan,
        })
    }

    fn document(&self) -> Result<&DocumentSchema, Error> {
        self.document
            .as_ref()
            .ok_or_else(|| error(Pos::default(), "text compilation requires Schema::from_sdl"))
    }
}

pub struct RestRelationQuery<'a> {
    schema: &'a Schema,
    relation: &'a crate::RestRelation,
    plan: crate::compiler::model::Query,
}

impl RestRelationQuery<'_> {
    pub fn compile(&self, key: &[&Json]) -> Result<Compiled, Error> {
        use crate::compiler::model;

        let pos = Pos::default();
        let document = self.schema.document()?;
        let relation = self.relation;
        if key.len() != relation.references.len() {
            return Err(error(pos, "wrong number of REST relationship keys"));
        }
        let filters = relation
            .references
            .iter()
            .zip(key)
            .map(|(reference, value)| {
                let ty = &document.row_types[&(relation.target.clone(), reference.clone())];
                let value = async_graphql_value::ConstValue::from_json((*value).clone())
                    .map_err(|e| error(pos, e.to_string()))?;
                let value = query::coerce(
                    document,
                    value,
                    &Type::new(&format!("{ty}!")).unwrap(),
                    true,
                    pos,
                    0,
                )?;
                Ok(crate::Filter::Compare {
                    field: reference.clone(),
                    op: crate::Comparison::Eq,
                    value: query::scalar(&value, ty, pos)?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let schema = self.schema.model.clone();
        let table = model::find_table(schema.clone(), &relation.target).unwrap();
        let join = model::resolve_filter(
            schema,
            table,
            Some(crate::filter_model(&crate::Filter::And(filters), 0)?),
        )
        .map_err(crate::error_from_model)?;
        let mut plan = self.plan.clone();
        plan.scan = model::append_condition(plan.scan, join);
        let compiled = model::compile(plan);
        Ok(Compiled {
            sql: compiled.sql,
            params: compiled.params.into_iter().map(crate::value).collect(),
        })
    }
}

fn error(pos: Pos, message: impl Into<String>) -> Error {
    Error::Document {
        message: message.into(),
        line: pos.line.max(1),
        column: pos.column.max(1),
    }
}

fn syntax(error: async_graphql_parser::Error) -> Error {
    self::error(
        error.positions().next().unwrap_or_default(),
        error.to_string(),
    )
}

fn named(ty: &Type) -> &str {
    match &ty.base {
        BaseType::Named(name) => name.as_str(),
        BaseType::List(inner) => named(inner),
    }
}

fn bounded(text: &str) -> Result<(), Error> {
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(error(Pos::default(), "document exceeds the byte limit"));
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    let mut parentheses = 0usize;
    let mut keys: Vec<Option<std::collections::BTreeSet<&str>>> = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                let block = bytes[i..].starts_with(b"\"\"\"");
                i += if block { 3 } else { 1 };
                while i < bytes.len() {
                    if block && bytes[i..].starts_with(b"\\\"\"\"") {
                        i += 4;
                        continue;
                    }
                    if !block && bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if block && bytes[i..].starts_with(b"\"\"\"") {
                        i += 3;
                        break;
                    }
                    if !block && bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'{' | b'[' | b'(' => {
                keys.push(
                    (bytes[i] == b'{' && parentheses > 0).then(std::collections::BTreeSet::new),
                );
                if bytes[i] == b'(' {
                    parentheses += 1;
                }
                depth += 1;
                if depth > MAX_DOCUMENT_DEPTH {
                    return Err(error(Pos::default(), "document exceeds the nesting limit"));
                }
                i += 1;
            }
            b'}' | b']' | b')' => {
                keys.pop();
                if bytes[i] == b')' {
                    parentheses = parentheses.saturating_sub(1);
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let name = &text[start..i];
                let mut next = i;
                loop {
                    while next < bytes.len()
                        && (bytes[next].is_ascii_whitespace() || bytes[next] == b',')
                    {
                        next += 1;
                    }
                    if next < bytes.len() && bytes[next] == b'#' {
                        while next < bytes.len() && bytes[next] != b'\n' {
                            next += 1;
                        }
                    } else {
                        break;
                    }
                }
                if bytes.get(next) == Some(&b':')
                    && let Some(Some(keys)) = keys.last_mut()
                    && !keys.insert(name)
                {
                    let prefix = &text[..start];
                    return Err(error(
                        Pos {
                            line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
                            column: prefix.rsplit('\n').next().unwrap().chars().count() + 1,
                        },
                        format!("repeated input field {name}"),
                    ));
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}
