use crate::{
    BOOL, BPCHAR, Bound, DATE, Error, FLOAT4, FLOAT8, INT2, INT4, INT8, NAME, TEXT, TIMESTAMP,
    TIMESTAMPTZ, VARCHAR, timestamp, timestamptz,
};
use chrono::{NaiveDate, SecondsFormat};
use rusuna::{Compiled, Value};
use serde_json::Value as Json;
use sqlx::postgres::{PgArguments, PgConnection, PgPool, PgTypeInfo};
use sqlx::{Arguments, AssertSqlSafe, Either, Executor, Row, SqlSafeStr, Statement};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, watch};

const SUBSCRIPTION: &str = "\"__rusuna_subscription\"";
const RESULT: &str = "\"__rusuna_result\"";

/// One compiled statement rewritten to run for many argument lists at once.
///
/// The statement reads the argument lists from one `jsonb[]` parameter, one JSON array per
/// subscriber, and runs the original statement once per element in a `LEFT JOIN LATERAL`
/// subquery. Each placeholder `$k` of the original statement becomes the `k`-th element
/// of the subscriber's array cast to the type PostgreSQL inferred for `$k`.
#[derive(Debug, Clone)]
pub struct Multiplexed {
    sql: String,
    types: Vec<PgTypeInfo>,
}

impl Multiplexed {
    /// Prepares `compiled` on `conn` to read its parameter types and rewrites it.
    pub async fn prepare(conn: &mut PgConnection, compiled: &Compiled) -> Result<Self, Error> {
        let statement = (&mut *conn)
            .prepare(AssertSqlSafe(compiled.sql.clone()).into_sql_str())
            .await?;
        let types = match statement.parameters() {
            Some(Either::Left(types)) => types.to_vec(),
            _ => Vec::new(),
        };
        if types.len() != compiled.params.len() {
            return Err(Error::Parameter {
                index: types.len().min(compiled.params.len()),
                message: format!(
                    "the statement has {} parameters and {} values",
                    types.len(),
                    compiled.params.len()
                ),
            });
        }
        Self::new(&compiled.sql, types)
    }

    /// Rewrites `sql` for the parameter types `types`, one per placeholder.
    ///
    /// The text inside single-quoted string literals and double-quoted identifiers is copied
    /// unchanged. The placeholders outside them must be exactly `$1` to `$n`, each once and
    /// in order, which is how rusuna numbers the parameters of a statement.
    pub fn new(sql: &str, types: Vec<PgTypeInfo>) -> Result<Self, Error> {
        let mut casts = Vec::new();
        for (index, ty) in types.iter().enumerate() {
            casts.push(format!(
                "(({SUBSCRIPTION}.\"args\"->>{index})::{})",
                cast(ty).ok_or_else(|| Error::Parameter {
                    index,
                    message: format!("{ty} parameters cannot be multiplexed"),
                })?
            ));
        }
        let body = replace_placeholders(sql, &casts)?;
        Ok(Self {
            sql: format!(
                "SELECT {SUBSCRIPTION}.\"id\", {RESULT}.\"value\" FROM unnest($1::jsonb[]) WITH ORDINALITY AS {SUBSCRIPTION}(\"args\", \"id\") LEFT JOIN LATERAL ({body}) AS {RESULT}(\"value\") ON true ORDER BY {SUBSCRIPTION}.\"id\""
            ),
            types,
        })
    }

    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// Checks one subscriber's parameters against the statement's types, with the rules of
    /// [`crate::fetch`], and returns them as the JSON array the statement reads.
    pub fn arguments(&self, params: &[Value]) -> Result<Json, Error> {
        if params.len() != self.types.len() {
            return Err(Error::Parameter {
                index: params.len().min(self.types.len()),
                message: format!(
                    "the statement has {} parameters and {} values",
                    self.types.len(),
                    params.len()
                ),
            });
        }
        let mut arguments = PgArguments::default();
        let mut values = Vec::new();
        for (index, (value, ty)) in params.iter().zip(&self.types).enumerate() {
            let error = |message: String| Error::Parameter { index, message };
            arguments
                .add(Bound {
                    value,
                    ty: ty.clone(),
                })
                .map_err(|e| error(e.to_string()))?;
            values.push(
                text(value, ty)
                    .ok_or_else(|| error(format!("{value:?} cannot be sent as {ty}")))?,
            );
        }
        Ok(Json::Array(values))
    }

    /// Runs the statement for the argument lists `arguments` and returns one result per list,
    /// in order.
    pub async fn fetch(
        &self,
        conn: &mut PgConnection,
        arguments: &[Json],
    ) -> Result<Vec<Json>, Error> {
        let rows = sqlx::query(AssertSqlSafe(self.sql.clone()))
            .bind(arguments.to_vec())
            .fetch_all(&mut *conn)
            .await?;
        let mut results = vec![Json::Null; arguments.len()];
        for row in rows {
            let id: i64 = row.try_get(0)?;
            let value: Option<Json> = row.try_get(1)?;
            if let Some(slot) = usize::try_from(id - 1)
                .ok()
                .and_then(|i| results.get_mut(i))
            {
                *slot = value.unwrap_or(Json::Null);
            }
        }
        Ok(results)
    }
}

fn cast(ty: &PgTypeInfo) -> Option<&'static str> {
    Some(match ty.oid()?.0 {
        BOOL => "bool",
        NAME => "name",
        INT8 => "int8",
        INT2 => "int2",
        INT4 => "int4",
        TEXT => "text",
        FLOAT4 => "float4",
        FLOAT8 => "float8",
        BPCHAR => "bpchar",
        VARCHAR => "varchar",
        DATE => "date",
        TIMESTAMP => "timestamp",
        TIMESTAMPTZ => "timestamptz",
        _ => return None,
    })
}

fn text(value: &Value, ty: &PgTypeInfo) -> Option<Json> {
    let text = match (value, ty.oid()?.0) {
        (Value::Null, _) => return Some(Json::Null),
        (Value::Bool(v), BOOL) => v.to_string(),
        (Value::Int(v), INT2 | INT4 | INT8) => v.to_string(),
        (Value::Int(v), FLOAT4) => (*v as f32).to_string(),
        (Value::Int(v), FLOAT8) => (*v as f64).to_string(),
        (Value::Float(v), FLOAT4) => (*v as f32).to_string(),
        (Value::Float(v), FLOAT8) => v.to_string(),
        (Value::Text(v), TEXT | VARCHAR | BPCHAR | NAME) => v.clone(),
        (Value::Text(v), TIMESTAMPTZ) => {
            timestamptz(v)?.to_rfc3339_opts(SecondsFormat::AutoSi, true)
        }
        (Value::Text(v), TIMESTAMP) => timestamp(v)?.format("%Y-%m-%dT%H:%M:%S%.f").to_string(),
        (Value::Text(v), DATE) => NaiveDate::parse_from_str(v, "%Y-%m-%d")
            .ok()?
            .format("%Y-%m-%d")
            .to_string(),
        _ => return None,
    };
    Some(Json::String(text))
}

fn replace_placeholders(sql: &str, replacements: &[String]) -> Result<String, Error> {
    let invalid = |message: String| Error::Statement(message);
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.char_indices().peekable();
    let mut next = 1;
    while let Some((_, c)) = chars.next() {
        match c {
            '\'' | '"' => {
                out.push(c);
                loop {
                    let Some((_, d)) = chars.next() else {
                        return Err(invalid("the statement has an unterminated quote".into()));
                    };
                    out.push(d);
                    if d == c {
                        if chars.peek().is_some_and(|(_, e)| *e == c) {
                            chars.next();
                            out.push(c);
                        } else {
                            break;
                        }
                    }
                }
            }
            '$' => {
                let mut digits = String::new();
                while let Some((_, d)) = chars.peek().filter(|(_, d)| d.is_ascii_digit()) {
                    digits.push(*d);
                    chars.next();
                }
                if digits != next.to_string() {
                    return Err(invalid(format!(
                        "expected the placeholder ${next}, found ${digits}"
                    )));
                }
                let replacement = replacements.get(next - 1).ok_or_else(|| {
                    invalid(format!(
                        "the statement has more than {} placeholders",
                        replacements.len()
                    ))
                })?;
                out.push_str(replacement);
                next += 1;
            }
            _ => out.push(c),
        }
    }
    if next - 1 != replacements.len() {
        return Err(invalid(format!(
            "the statement has {} placeholders and {} parameters",
            next - 1,
            replacements.len()
        )));
    }
    Ok(out)
}

/// Settings of [`Subscriptions`].
#[derive(Debug, Clone, Copy)]
pub struct SubscriptionOptions {
    /// The time between two runs of a group's statement. Defaults to one second.
    pub interval: Duration,
    /// The largest number of subscriptions one statement reads. Defaults to 100.
    pub batch_size: usize,
}

impl Default for SubscriptionOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            batch_size: 100,
        }
    }
}

/// Counters of a [`Subscriptions`] engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubscriptionStats {
    /// Groups with at least one subscription.
    pub groups: usize,
    /// Active subscriptions.
    pub subscriptions: usize,
    /// Runs of a group, each executing one statement per batch of subscriptions.
    pub polls: u64,
    /// Multiplexed statements executed.
    pub statements: u64,
}

/// The result of a subscription: the JSON of its root field, or the error of its group's
/// statement.
pub type Outcome = Result<Json, Arc<Error>>;

type Key = (String, String);

struct Member {
    arguments: Json,
    last: Option<Result<Json, String>>,
    sender: watch::Sender<Option<Outcome>>,
}

struct Group {
    generation: u64,
    multiplexed: Arc<Multiplexed>,
    members: BTreeMap<u64, Member>,
    wake: Arc<Notify>,
}

struct Inner {
    pool: PgPool,
    options: SubscriptionOptions,
    groups: Mutex<HashMap<Key, Group>>,
    ids: AtomicU64,
    polls: AtomicU64,
    statements: AtomicU64,
}

/// Polls subscribed statements and reports changed results.
///
/// Subscriptions with the same role and the same SQL text form a group. The statements
/// rusuna compiles for inputs that differ only in values have the same text, so these are
/// the subscriptions whose variables or session values differ. A group runs one
/// [`Multiplexed`] statement per batch of [`SubscriptionOptions::batch_size`]
/// subscriptions every [`SubscriptionOptions::interval`], and sends a subscription its
/// result when it differs from the last result sent to it. A group starts with its first
/// subscription and stops when its last subscription is dropped.
#[derive(Clone)]
pub struct Subscriptions {
    inner: Arc<Inner>,
}

impl Subscriptions {
    pub fn new(pool: PgPool, options: SubscriptionOptions) -> Self {
        assert!(options.batch_size > 0, "the batch size must be positive");
        Self {
            inner: Arc::new(Inner {
                pool,
                options,
                groups: Mutex::new(HashMap::new()),
                ids: AtomicU64::new(0),
                polls: AtomicU64::new(0),
                statements: AtomicU64::new(0),
            }),
        }
    }

    pub fn options(&self) -> SubscriptionOptions {
        self.inner.options
    }

    pub fn stats(&self) -> SubscriptionStats {
        let groups = self.inner.groups.lock().unwrap();
        SubscriptionStats {
            groups: groups.len(),
            subscriptions: groups.values().map(|g| g.members.len()).sum(),
            polls: self.inner.polls.load(Ordering::SeqCst),
            statements: self.inner.statements.load(Ordering::SeqCst),
        }
    }

    /// Subscribes to the result of `compiled`, the statement of one query root field
    /// compiled for a session of `role`.
    ///
    /// The parameters are checked against the statement's types before the subscription
    /// joins its group, so an invalid value fails this call alone. The first result arrives
    /// from the group's next run, which the new subscription starts at once.
    pub async fn subscribe(&self, role: &str, compiled: &Compiled) -> Result<Subscription, Error> {
        let key = (role.to_string(), compiled.sql.clone());
        let existing = self
            .inner
            .groups
            .lock()
            .unwrap()
            .get(&key)
            .map(|group| group.multiplexed.clone());
        let multiplexed = match existing {
            Some(multiplexed) => multiplexed,
            None => {
                let mut conn = self.inner.pool.acquire().await?;
                Arc::new(Multiplexed::prepare(&mut conn, compiled).await?)
            }
        };
        let arguments = multiplexed.arguments(&compiled.params)?;
        let id = self.inner.ids.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = watch::channel(None);
        let member = Member {
            arguments,
            last: None,
            sender,
        };
        let mut groups = self.inner.groups.lock().unwrap();
        let group = groups.entry(key.clone()).or_insert_with(|| {
            let group = Group {
                generation: id,
                multiplexed,
                members: BTreeMap::new(),
                wake: Arc::new(Notify::new()),
            };
            tokio::spawn(poll(
                self.inner.clone(),
                key.clone(),
                group.generation,
                group.wake.clone(),
            ));
            group
        });
        group.members.insert(id, member);
        group.wake.notify_one();
        Ok(Subscription {
            inner: self.inner.clone(),
            key,
            id,
            receiver,
        })
    }
}

/// One subscription of [`Subscriptions`]. Dropping it leaves its group.
pub struct Subscription {
    inner: Arc<Inner>,
    key: Key,
    id: u64,
    receiver: watch::Receiver<Option<Outcome>>,
}

impl Subscription {
    /// Waits for the next result that differs from the last one returned.
    pub async fn next(&mut self) -> Option<Outcome> {
        loop {
            self.receiver.changed().await.ok()?;
            if let Some(outcome) = self.receiver.borrow_and_update().clone() {
                return Some(outcome);
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut groups = self.inner.groups.lock().unwrap();
        if let Some(group) = groups.get_mut(&self.key) {
            group.members.remove(&self.id);
            if group.members.is_empty() {
                let wake = group.wake.clone();
                groups.remove(&self.key);
                wake.notify_one();
            }
        }
    }
}

async fn poll(inner: Arc<Inner>, key: Key, generation: u64, wake: Arc<Notify>) {
    wake.notified().await;
    loop {
        let Some((multiplexed, batches)) = snapshot(&inner, &key, generation) else {
            return;
        };
        inner.polls.fetch_add(1, Ordering::SeqCst);
        let mut outcomes = Vec::new();
        for batch in batches {
            let (ids, arguments): (Vec<u64>, Vec<Json>) = batch.into_iter().unzip();
            let result = match inner.pool.acquire().await {
                Ok(mut conn) => {
                    let result = multiplexed.fetch(&mut conn, &arguments).await;
                    inner.statements.fetch_add(1, Ordering::SeqCst);
                    result
                }
                Err(error) => Err(error.into()),
            };
            match result {
                Ok(values) => outcomes.extend(ids.into_iter().zip(values.into_iter().map(Ok))),
                Err(error) => {
                    let error = Arc::new(error);
                    outcomes.extend(ids.into_iter().map(|id| (id, Err(error.clone()))));
                }
            }
        }
        deliver(&inner, &key, generation, outcomes);
        let interval = inner.options.interval;
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = wake.notified() => {}
        }
    }
}

type Batches = Vec<Vec<(u64, Json)>>;

fn snapshot(inner: &Inner, key: &Key, generation: u64) -> Option<(Arc<Multiplexed>, Batches)> {
    let groups = inner.groups.lock().unwrap();
    let group = groups.get(key).filter(|g| g.generation == generation)?;
    let members: Vec<(u64, Json)> = group
        .members
        .iter()
        .map(|(id, member)| (*id, member.arguments.clone()))
        .collect();
    Some((
        group.multiplexed.clone(),
        members
            .chunks(inner.options.batch_size)
            .map(<[_]>::to_vec)
            .collect(),
    ))
}

fn deliver(inner: &Inner, key: &Key, generation: u64, outcomes: Vec<(u64, Outcome)>) {
    let mut groups = inner.groups.lock().unwrap();
    let Some(group) = groups.get_mut(key).filter(|g| g.generation == generation) else {
        return;
    };
    for (id, outcome) in outcomes {
        let Some(member) = group.members.get_mut(&id) else {
            continue;
        };
        let comparable = match &outcome {
            Ok(value) => Ok(value.clone()),
            Err(error) => Err(error.to_string()),
        };
        if member.last.as_ref() != Some(&comparable) {
            member.last = Some(comparable);
            member.sender.send_replace(Some(outcome));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replaced(sql: &str, n: usize) -> Result<String, Error> {
        let replacements: Vec<String> = (0..n).map(|i| format!("<{i}>")).collect();
        replace_placeholders(sql, &replacements)
    }

    #[test]
    fn placeholders_outside_quotes_are_replaced_in_order() {
        assert_eq!(
            replaced(r#"SELECT "a$1""" , 'b''$2' , $1 , "$2" , $2"#, 2).unwrap(),
            r#"SELECT "a$1""" , 'b''$2' , <0> , "$2" , <1>"#
        );
        assert!(replaced("SELECT $10", 10).is_err());
        assert!(replaced("SELECT $2 , $1", 2).is_err());
        assert!(replaced("SELECT $1 , $1", 2).is_err());
        assert!(replaced("SELECT $1", 2).is_err());
        assert!(replaced("SELECT $1 , $2", 1).is_err());
        assert!(replaced("SELECT 'a", 0).is_err());
        assert!(replaced("SELECT $", 0).is_err());
    }

    #[test]
    fn names_with_dollar_signs_stay_in_the_multiplexed_statement() {
        let schema = rusuna::Schema::from_sdl(
            r#"
            type t @table(name: "a$1", schema: "s$2") {
                id: Int! @column(name: "x$1")
                label: String! @column(name: "$2'$3")
            }
            type Query { t: [t!]! @list }
            "#,
        )
        .unwrap();
        let compiled = schema
            .compile(
                "{ t(where: {id: {_eq: 1}, label: {_eq: \"$1\"}}) { id label } }",
                &serde_json::json!({}),
                None,
                &rusuna::Session::new("admin"),
            )
            .unwrap()
            .remove(0)
            .compiled;
        let types = vec![
            PgTypeInfo::with_oid(sqlx::postgres::types::Oid(INT4)),
            PgTypeInfo::with_oid(sqlx::postgres::types::Oid(TEXT)),
        ];
        let multiplexed = Multiplexed::new(&compiled.sql, types).unwrap();
        let expected = compiled
            .sql
            .replacen(
                " $1 ",
                " ((\"__rusuna_subscription\".\"args\"->>0)::int4) ",
                1,
            )
            .replacen(
                " $2 ",
                " ((\"__rusuna_subscription\".\"args\"->>1)::text) ",
                1,
            );
        assert!(
            multiplexed.sql().contains(&expected),
            "{}",
            multiplexed.sql()
        );
        assert!(compiled.sql.contains("\"a$1\""));
        assert!(compiled.sql.contains("\"$2'$3\""));
        assert_eq!(
            multiplexed
                .arguments(&[rusuna::Value::Int(1), rusuna::Value::Text("$1".into())])
                .unwrap(),
            serde_json::json!(["1", "$1"])
        );
    }
}
