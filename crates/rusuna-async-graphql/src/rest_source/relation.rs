use super::*;
use rusuna::{Compiled, RestRelation};
use rusuna_sqlx::{Multiplexed, SubscriptionOptions};
use sqlx::PgConnection;

type Prepared = HashMap<String, Arc<OnceCell<Multiplexed>>>;
use remote::Outcome;
pub(super) type Outcomes = HashMap<String, Outcome>;

struct Row<'a> {
    value: &'a Json,
    path: String,
}

pub(super) async fn collect(
    shared: &Shared,
    ctx: &ResolverContext<'_>,
    ty: &Type,
    value: &Json,
    session: &rusuna::Session,
    mut connection: Option<&mut PgConnection>,
) -> async_graphql::Result<Outcomes> {
    let mut outcomes = Outcomes::new();
    let catalog = shared.schema.rest_catalog();
    if (catalog.relations.is_empty() && shared.rest.remote_relations.is_empty())
        || !catalog.objects.contains_key(named(ty))
    {
        return Ok(outcomes);
    }
    let mut rows = Vec::new();
    collect_rows(ty, value, path_key(ctx.path_node.as_ref()), &mut rows);
    let mut pending = vec![(named(ty), rows, vec![ctx.ctx.field()])];
    while let Some((parent, rows, selections)) = pending.pop() {
        if rows.is_empty() {
            continue;
        }
        let columns = &catalog.objects[parent];
        let mut fields = BTreeMap::<_, Vec<_>>::new();
        for field in selections.iter().flat_map(SelectionField::selection_set) {
            fields
                .entry(field.alias().unwrap_or(field.name()))
                .or_default()
                .push(field);
        }
        for (alias, fields) in fields {
            let name = fields[0].name();
            let field_key = (parent.into(), name.into());
            if let Some(relation) = catalog.relations.get(&field_key) {
                let mut keys = HashMap::new();
                let mut batch_keys = Vec::new();
                let mut paths = Vec::new();
                for row in &rows {
                    let key: Vec<_> = relation
                        .columns
                        .iter()
                        .map(|column| row.value.get(&columns[column].key).unwrap_or(&Json::Null))
                        .collect();
                    if key.iter().any(|value| value.is_null()) {
                        continue;
                    }
                    let index = *keys.entry(key.clone()).or_insert_with(|| {
                        batch_keys.push(key);
                        batch_keys.len() - 1
                    });
                    paths.push((format!("{}.{alias}", row.path), index));
                }
                if batch_keys.is_empty() {
                    continue;
                }
                let results = resolve(
                    shared,
                    ctx,
                    parent,
                    &fields,
                    &batch_keys,
                    session,
                    connection.as_deref_mut(),
                )
                .await;
                if let Ok((results, true)) = &results {
                    let mut plan = table::Plan::default();
                    for (path, index) in &paths {
                        if let Ok(value) = &results[*index] {
                            plan.add(&shared.catalog, &relation.target, path, value, &fields);
                        }
                    }
                    plan.run(shared, ctx, session).await?;
                }
                for (path, index) in paths {
                    outcomes.insert(
                        path,
                        match &results {
                            Ok((results, _)) => results[index].clone(),
                            Err(error) => Err(error.clone()),
                        },
                    );
                }
            } else if let Some(relation) = shared.rest.remote_relations.get(&field_key) {
                let spec = &catalog.remote_relations[&field_key];
                let mut keys = HashMap::new();
                let mut batch_keys = Vec::new();
                let mut paths = Vec::new();
                for row in &rows {
                    let key: Vec<_> = spec
                        .arguments
                        .values()
                        .map(|template| {
                            remote_argument(template, |name| {
                                row.value.get(&columns[name].key).unwrap_or(&Json::Null)
                            })
                        })
                        .collect();
                    let index = *keys.entry(key.clone()).or_insert_with(|| {
                        batch_keys.push(key);
                        batch_keys.len() - 1
                    });
                    paths.push((format!("{}.{alias}", row.path), index));
                }
                let results = relation.resolve(ctx, &fields, &batch_keys).await;
                for (path, index) in paths {
                    outcomes.insert(path, results[index].clone());
                }
            } else if let Some(column) = columns.get(name) {
                let child = named(&column.ty);
                if catalog.objects.contains_key(child) {
                    let mut children = Vec::new();
                    for row in &rows {
                        if let Some(value) = row.value.get(&column.key) {
                            collect_rows(
                                &column.ty,
                                value,
                                format!("{}.{alias}", row.path),
                                &mut children,
                            );
                        }
                    }
                    pending.push((child, children, fields));
                }
            }
        }
    }
    Ok(outcomes)
}

fn remote_argument<'a>(template: &Template, field: impl Fn(&str) -> &'a Json) -> Json {
    if let [TemplatePart::Field(name)] = template.0.as_slice() {
        return field(name).clone();
    }
    let mut text = String::new();
    for part in &template.0 {
        match part {
            TemplatePart::Literal(literal) => text.push_str(literal),
            TemplatePart::Field(name) => {
                let value = field(name);
                if value.is_null() {
                    return Json::Null;
                }
                text.push_str(&as_text(value.clone()));
            }
            TemplatePart::Session(_) => unreachable!(),
        }
    }
    Json::String(text)
}

fn collect_rows<'a>(ty: &Type, value: &'a Json, path: String, rows: &mut Vec<Row<'a>>) {
    match &ty.base {
        BaseType::Named(_) if value.is_object() => rows.push(Row { value, path }),
        BaseType::List(item) => {
            if let Some(values) = value.as_array() {
                for (index, value) in values.iter().enumerate() {
                    collect_rows(item, value, format!("{path}.{index}"), rows);
                }
            }
        }
        _ => {}
    }
}

async fn resolve(
    shared: &Shared,
    ctx: &ResolverContext<'_>,
    parent: &str,
    fields: &[SelectionField<'_>],
    keys: &[Vec<&Json>],
    session: &rusuna::Session,
    connection: Option<&mut PgConnection>,
) -> async_graphql::Result<(Vec<Outcome>, bool)> {
    let mut document = Document {
        catalog: &shared.catalog,
        text: String::new(),
        definitions: Vec::new(),
        variables: serde_json::Map::new(),
        omit_null: false,
        has_table_rest: false,
    };
    let relation =
        &shared.schema.rest_catalog().relations[&(parent.into(), fields[0].name().into())];
    for field in fields {
        document.field(parent, field, Some(&relation.target), None)?;
        document.text.push(' ');
    }
    let has_table_rest = document.has_table_rest;
    let (text, variables) = document.finish("query");
    let query = shared
        .schema
        .rest_relation_query(parent, &text, &variables, session)
        .map_err(|error| invalid(error.to_string()))?;
    let mut results = Vec::with_capacity(keys.len());
    let mut compiled = None::<Compiled>;
    let mut params = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        match query.compile(key) {
            Ok(statement) => {
                if let Some(compiled) = &compiled {
                    if compiled.sql != statement.sql {
                        return Err(coded(
                            "REST relationship keys produced different SQL",
                            "unexpected",
                        ));
                    }
                } else {
                    compiled = Some(statement.clone());
                }
                params.push((index, statement.params));
                results.push(Ok(Arc::new(Json::Null)));
            }
            Err(error) => results.push(Err(invalid(error.to_string()))),
        }
    }
    if let Some(compiled) = compiled {
        let mut acquired;
        let connection = match connection {
            Some(connection) => connection,
            None => {
                acquired = ctx
                    .data::<PgPool>()?
                    .acquire()
                    .await
                    .map_err(|error| execution_error(&error.into()))?;
                &mut acquired
            }
        };
        shared
            .rest
            .multiplexed
            .fetch(connection, &compiled, relation, &params, &mut results)
            .await?;
    }
    Ok((results, has_table_rest))
}

#[derive(Default)]
pub(super) struct MultiplexedCache(SyncMutex<Prepared>);

impl MultiplexedCache {
    async fn fetch(
        &self,
        connection: &mut PgConnection,
        compiled: &Compiled,
        relation: &RestRelation,
        params: &[(usize, Vec<rusuna::Value>)],
        results: &mut [Outcome],
    ) -> async_graphql::Result<()> {
        let cached = self
            .0
            .lock()
            .unwrap()
            .entry(compiled.sql.clone())
            .or_default()
            .clone();
        let prepared = cached
            .get_or_try_init(|| Multiplexed::prepare(connection, compiled))
            .await
            .map_err(|error| execution_error(&error))?;
        let mut positions = Vec::new();
        let mut arguments = Vec::new();
        for (index, params) in params {
            match prepared.arguments(params) {
                Ok(args) => {
                    positions.push(*index);
                    arguments.push(args);
                }
                Err(error) => results[*index] = Err(invalid(error.to_string())),
            }
        }
        let size = SubscriptionOptions::default().batch_size;
        for (positions, arguments) in positions.chunks(size).zip(arguments.chunks(size)) {
            let values = prepared
                .fetch(connection, arguments)
                .await
                .map_err(|error| execution_error(&error))?;
            for (&index, value) in positions.iter().zip(values) {
                let value = if relation.list {
                    value
                } else {
                    match value {
                        Json::Array(values) => values.into_iter().next().unwrap_or(Json::Null),
                        value => value,
                    }
                };
                results[index] = Ok(Arc::new(value));
            }
        }
        Ok(())
    }
}
