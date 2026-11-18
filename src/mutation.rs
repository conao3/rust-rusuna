use crate::*;

pub const PERMISSION_CHECK_SQLSTATE: &str = "22P02";
pub const PERMISSION_CHECK_MARKER: &str = "rusuna: permission check failed";

pub fn is_permission_check_error(sqlstate: &str, message: &str) -> bool {
    sqlstate == PERMISSION_CHECK_SQLSTATE && message.contains(PERMISSION_CHECK_MARKER)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WritePermission {
    pub role: String,
    pub kind: WriteKind,
    pub columns: Vec<String>,
    pub filter: Filter<Operand>,
    pub check: Filter<Operand>,
    pub presets: Vec<(String, Operand)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryKey {
    pub columns: Vec<String>,
    pub constraint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OnConflict {
    pub constraint: String,
    pub update_columns: Vec<String>,
    pub filter: Option<Filter>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MutationArguments {
    pub target: String,
    pub on_conflict: Option<OnConflict>,
    pub kind: WriteKind,
    pub single: bool,
    pub by_key: bool,
    pub keys: Vec<(String, Value)>,
    pub objects: Vec<Vec<(String, Value)>>,
    pub set: Vec<(String, Value)>,
    pub increment: Vec<(String, Value)>,
    pub filter: Option<Filter>,
}

impl MutationArguments {
    pub fn new(target: impl Into<String>, kind: WriteKind) -> Self {
        Self {
            target: target.into(),
            on_conflict: None,
            kind,
            single: false,
            by_key: false,
            keys: Vec::new(),
            objects: Vec::new(),
            set: Vec::new(),
            increment: Vec::new(),
            filter: None,
        }
    }
}

fn kind_model(kind: WriteKind) -> model::WriteKind {
    match kind {
        WriteKind::Insert => model::WriteKind::Insert,
        WriteKind::Update => model::WriteKind::Update,
        WriteKind::Delete => model::WriteKind::Delete,
    }
}

fn values_model(values: &[(String, Value)]) -> Vec<(String, model::Value)> {
    values
        .iter()
        .map(|(name, value)| (name.clone(), value_model(value)))
        .collect()
}

pub(crate) fn catalog(tables: &[Table]) -> Result<model::WriteCatalog, Error> {
    Ok(model::WriteCatalog {
        keys: tables
            .iter()
            .filter_map(|t| {
                t.primary_key.as_ref().map(|key| model::PrimaryKey {
                    target: t.name.clone(),
                    columns: key.columns.clone(),
                    constraint: key.constraint.clone(),
                })
            })
            .collect(),
        permissions: tables
            .iter()
            .flat_map(|t| t.write_permissions.iter().map(move |p| (t, p)))
            .map(|(t, p)| {
                Ok(model::WritePermission {
                    target: t.name.clone(),
                    role: p.role.clone(),
                    kind: kind_model(p.kind),
                    columns: p.columns.clone(),
                    conds: permission_filter_model(&p.filter, 0)?,
                    check: permission_filter_model(&p.check, 0)?,
                    presets: p
                        .presets
                        .iter()
                        .map(|(f, v)| (f.clone(), operand_model(v)))
                        .collect(),
                })
            })
            .collect::<Result<_, Error>>()?,
    })
}

fn result(compiled: Result<model::Compiled, model::Error>) -> Result<Compiled, Error> {
    compiled
        .map(|c| Compiled {
            sql: c.sql,
            params: c.params.into_iter().map(value).collect(),
        })
        .map_err(error_from_model)
}

impl Schema {
    pub fn compile_mutation(
        &self,
        input: &MutationArguments,
        returning: &[Selection],
        session: &Session,
    ) -> Result<Compiled, Error> {
        let input = model::WriteInput {
            target: input.target.clone(),
            conflict: input
                .on_conflict
                .as_ref()
                .map(|c| {
                    Ok(model::ConflictInput {
                        constraint: c.constraint.clone(),
                        columns: c.update_columns.clone(),
                        conds: c.filter.as_ref().map(|f| filter_model(f, 0)).transpose()?,
                    })
                })
                .transpose()?,
            kind: kind_model(input.kind),
            single: input.single,
            by_key: input.by_key,
            keys: values_model(&input.keys),
            objects: input.objects.iter().map(|o| values_model(o)).collect(),
            sets: values_model(&input.set),
            increments: values_model(&input.increment),
            conds: input
                .filter
                .as_ref()
                .map(|f| filter_model(f, 0))
                .transpose()?,
            children: returning
                .iter()
                .map(|s| selection_model(s, 0))
                .collect::<Result<_, _>>()?,
        };
        result(model::run_write(
            self.model.clone(),
            self.writes.clone(),
            session_model(session),
            input,
        ))
    }
}

pub(crate) fn compile_primary(
    schema: &Schema,
    target: &str,
    values: &[(String, Value)],
    children: &[Selection],
    session: &Session,
) -> Result<Compiled, Error> {
    result(model::run_primary_key(
        schema.model.clone(),
        schema.writes.clone(),
        session_model(session),
        target,
        values_model(values),
        children
            .iter()
            .map(|s| selection_model(s, 0))
            .collect::<Result<_, _>>()?,
    ))
}
