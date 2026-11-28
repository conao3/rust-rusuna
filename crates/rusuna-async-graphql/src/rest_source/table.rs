use super::*;
use futures_util::future::join_all;
use rusuna::RestBatch;

type Outcome = async_graphql::Result<Option<Arc<Json>>>;
pub(super) type Outcomes = HashMap<String, Outcome>;
type FieldKey = (String, String);

#[derive(Default)]
pub(super) struct Plan<'a>(BTreeMap<FieldKey, Parents<'a>>);

#[derive(Default)]
struct Parents<'a> {
    positions: HashMap<String, usize>,
    rows: Vec<Parent<'a>>,
}

struct Parent<'a> {
    row: &'a Json,
    paths: BTreeSet<String>,
}

pub(crate) async fn prepare(
    shared: &Shared,
    ctx: &ResolverContext<'_>,
    ty: &Type,
    value: &Json,
    session: &rusuna::Session,
) -> async_graphql::Result<()> {
    let mut plan = Plan::default();
    plan.add(
        &shared.catalog,
        named(ty),
        &path_key(ctx.path_node.as_ref()),
        value,
        std::slice::from_ref(&ctx.ctx.field()),
    );
    plan.run(shared, ctx, session).await
}

impl<'a> Plan<'a> {
    pub(super) fn add(
        &mut self,
        catalog: &Catalog,
        owner: &str,
        path: &str,
        value: &'a Json,
        selections: &[SelectionField<'_>],
    ) {
        match value {
            Json::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    self.add(
                        catalog,
                        owner,
                        &format!("{path}.{index}"),
                        value,
                        selections,
                    );
                }
            }
            Json::Object(row) => {
                for selection in selections.iter().flat_map(SelectionField::selection_set) {
                    let key = (owner.to_string(), selection.name().to_string());
                    let alias = selection.alias().unwrap_or(selection.name());
                    if let Some(projection) = catalog.extras.get(&key) {
                        if projection.rest {
                            let parents = self.0.entry(key).or_default();
                            let index =
                                *parents.positions.entry(path.into()).or_insert_with(|| {
                                    parents.rows.push(Parent {
                                        row: value,
                                        paths: BTreeSet::new(),
                                    });
                                    parents.rows.len() - 1
                                });
                            parents.rows[index].paths.insert(format!("{path}.{alias}"));
                        }
                    } else if let Some(child) = catalog.children.get(&key)
                        && let Some(value) = row.get(alias)
                    {
                        self.add(
                            catalog,
                            child,
                            &format!("{path}.{alias}"),
                            value,
                            &[selection],
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) async fn run(
        self,
        shared: &Shared,
        ctx: &ResolverContext<'_>,
        session: &rusuna::Session,
    ) -> async_graphql::Result<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        let state = ctx.data::<RestRequest>()?;
        let headers = ctx.data_opt::<ClientHeaders>();
        let results = join_all(self.0.into_iter().map(async |(key, parents)| {
            let field = &shared.schema.rest_catalog().fields[&key];
            let source = &shared.rest.sources[&field.source];
            let columns = field.table_columns.as_deref().unwrap();
            let tuples: Vec<_> = parents
                .rows
                .iter()
                .map(|parent| {
                    columns
                        .iter()
                        .map(|column| &parent.row[format!("{HIDDEN_PREFIX}{column}")])
                        .collect::<Vec<_>>()
                })
                .collect();
            let outcomes = match source.headers(session, headers) {
                Err(error) => vec![Err(error); tuples.len()],
                Ok(headers) => {
                    let execution = Execution {
                        state,
                        field,
                        source,
                        session,
                        headers: &headers,
                    };
                    match &field.batch {
                        Some(batch) => execution.batch(batch, &tuples).await,
                        None => execution.fetch(&key, columns, &tuples).await,
                    }
                }
            };
            parents.rows.into_iter().zip(outcomes)
        }))
        .await;
        let mut outcomes = state.table.lock().unwrap();
        for (parent, outcome) in results.into_iter().flatten() {
            for path in parent.paths {
                outcomes.insert(path, outcome.clone());
            }
        }
        Ok(())
    }
}

struct Execution<'a> {
    state: &'a RestRequest,
    field: &'a RestField,
    source: &'a RestSource,
    session: &'a rusuna::Session,
    headers: &'a HeaderMap,
}

impl Execution<'_> {
    async fn fetch(
        &self,
        key: &FieldKey,
        columns: &[String],
        tuples: &[Vec<&Json>],
    ) -> Vec<Outcome> {
        let mut positions = HashMap::new();
        let mut unique = Vec::new();
        let indices: Vec<_> = tuples
            .iter()
            .map(|tuple| {
                *positions.entry(tuple).or_insert_with(|| {
                    unique.push(tuple);
                    unique.len() - 1
                })
            })
            .collect();
        let results = join_all(unique.into_iter().map(async |tuple| {
            let value = |name: &str| {
                let index = columns.iter().position(|column| column == name).unwrap();
                Ok(tuple[index].clone())
            };
            let uri = self.source.uri(self.field, value, self.session, &[])?;
            let key = Some((
                key.clone(),
                tuple.iter().map(|&value| value.clone()).collect(),
            ));
            let response = cached_get(self.state, self.source, uri, self.headers, key).await;
            Ok(response_json(self.source, self.field, &response)?
                .map(|value| Arc::new(value.clone())))
        }))
        .await;
        indices
            .into_iter()
            .map(|index| results[index].clone())
            .collect()
    }

    async fn batch(&self, batch: &RestBatch, tuples: &[Vec<&Json>]) -> Vec<Outcome> {
        join_all(tuples.chunks(batch.max_size).map(async |chunk| {
            let keys: Vec<_> = chunk.iter().map(|tuple| tuple[0]).collect();
            match self.fetch_batch(batch, &keys).await {
                Ok(outcomes) => outcomes,
                Err(error) => vec![Err(error); chunk.len()],
            }
        }))
        .await
        .into_iter()
        .flatten()
        .collect()
    }

    async fn fetch_batch(
        &self,
        batch: &RestBatch,
        keys: &[&Json],
    ) -> async_graphql::Result<Vec<Outcome>> {
        let uri = self
            .source
            .uri(self.field, |_| unreachable!(), self.session, keys)?;
        let response = self
            .source
            .request(Method::GET, &uri, self.headers, None)
            .await;
        let Some(value) = response_json(self.source, self.field, &response)? else {
            return Ok(vec![Ok(None); keys.len()]);
        };
        let mut values = HashMap::new();
        for item in value.as_array().ok_or_else(unexpected)? {
            let key = item
                .get(&batch.key)
                .and_then(scalar_key)
                .ok_or_else(unexpected)?;
            match values.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Arc::new(item.clone()));
                }
                std::collections::hash_map::Entry::Occupied(entry) if **entry.get() == *item => {}
                _ => return Err(unexpected()),
            }
        }
        Ok(keys
            .iter()
            .map(|key| {
                let Some(key) = scalar_key(key) else {
                    return Ok(Some(Arc::new(Json::Null)));
                };
                match values.get(&key) {
                    Some(value) => Ok(Some(value.clone())),
                    None if self.field.not_found_null => Ok(None),
                    None => Err(unexpected()),
                }
            })
            .collect())
    }
}

fn scalar_key(value: &Json) -> Option<String> {
    match value {
        Json::String(value) => Some(value.clone()),
        Json::Number(value) => Some(value.to_string()),
        Json::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}
