mod query;
mod schema;
mod writes;

use crate::{Compiled, Error, Schema, Selection, Session};
use async_graphql_parser::{Pos, types::*};
use std::collections::BTreeMap;

pub const MAX_DOCUMENT_BYTES: usize = 1_048_576;
pub const MAX_DOCUMENT_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledField {
    pub response_key: String,
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
        Ok(schema::for_role(self.document()?, &self.model, &self.writes, role).sdl)
    }

    pub fn parse_query(
        &self,
        query: &str,
        variables: &serde_json::Value,
        operation_name: Option<&str>,
        role: &str,
    ) -> Result<Vec<Selection>, Error> {
        if role == "admin" {
            return query::parse(self.document()?, query, variables, operation_name);
        }
        query::parse(
            &schema::for_role(self.document()?, &self.model, &self.writes, role),
            query,
            variables,
            operation_name,
        )
    }

    pub fn compile(
        &self,
        query: &str,
        variables: &serde_json::Value,
        operation_name: Option<&str>,
        session: &Session,
    ) -> Result<Vec<CompiledField>, Error> {
        query::check_session(self.document()?, session)?;
        self.parse_query(query, variables, operation_name, &session.role)?
            .into_iter()
            .map(|selection| {
                Ok(CompiledField {
                    response_key: selection.alias.clone(),
                    compiled: self.compile_selection(&selection, session)?,
                })
            })
            .collect()
    }

    fn document(&self) -> Result<&DocumentSchema, Error> {
        self.document
            .as_ref()
            .ok_or_else(|| error(Pos::default(), "text compilation requires Schema::from_sdl"))
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
