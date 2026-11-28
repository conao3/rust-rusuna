//! Executes statements compiled by [`rusuna`] with sqlx on PostgreSQL.
//!
//! [`execute`] runs the root fields of a query one by one and the root fields of a
//! mutation in document order on one transaction, and returns the JSON object of the
//! GraphQL `data`. [`fetch`] runs one compiled statement on a connection.
//! [`Error::PermissionCheck`] reports a row that fails a write permission check.
//!
//! [`Subscriptions`] polls subscribed query statements, runs the subscriptions that differ
//! only in values as one [`Multiplexed`] statement and reports changed results.

mod subscription;

pub use subscription::{
    Multiplexed, Outcome, Subscription, SubscriptionOptions, SubscriptionStats, Subscriptions,
};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusuna::{Compiled, CompiledField, Value};
use serde_json::{Map, Value as Json};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::{PgArgumentBuffer, PgArguments, PgConnection, PgPool, PgTypeInfo, Postgres};
use sqlx::types::{BigDecimal, Uuid};
use sqlx::{Arguments, AssertSqlSafe, Either, Encode, Executor, Row, SqlSafeStr, Statement, Type};
use std::fmt;

pub const PERMISSION_CHECK_MESSAGE: &str =
    "check constraint of an insert/update permission has failed";

#[derive(Debug)]
pub enum Error {
    PermissionCheck,
    Parameter { index: usize, message: String },
    Statement(String),
    Database(sqlx::Error),
}

impl Error {
    pub fn is_permission_check(&self) -> bool {
        matches!(self, Self::PermissionCheck)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionCheck => f.write_str(PERMISSION_CHECK_MESSAGE),
            Self::Parameter { index, message } => {
                write!(f, "parameter ${}: {message}", index + 1)
            }
            Self::Statement(message) => write!(f, "the statement cannot be multiplexed: {message}"),
            Self::Database(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        if let Some(db) = error.as_database_error()
            && let Some(code) = db.code()
            && rusuna::is_permission_check_error(&code, db.message())
        {
            return Self::PermissionCheck;
        }
        Self::Database(error)
    }
}

const BOOL: u32 = 16;
const NAME: u32 = 19;
const INT8: u32 = 20;
const INT2: u32 = 21;
const INT4: u32 = 23;
const TEXT: u32 = 25;
const FLOAT4: u32 = 700;
const FLOAT8: u32 = 701;
const BPCHAR: u32 = 1042;
const VARCHAR: u32 = 1043;
const DATE: u32 = 1082;
const TIMESTAMP: u32 = 1114;
const TIMESTAMPTZ: u32 = 1184;
const NUMERIC: u32 = 1700;
const UUID: u32 = 2950;

struct Bound<'a> {
    value: &'a Value,
    ty: PgTypeInfo,
}

impl Type<Postgres> for Bound<'_> {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("text")
    }
}

fn mismatch(value: &Value, ty: &PgTypeInfo) -> BoxDynError {
    format!("{value:?} cannot be sent as {ty}").into()
}

fn timestamp(text: &str) -> Option<NaiveDateTime> {
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
}

fn timestamptz(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(value) = DateTime::parse_from_rfc3339(text) {
        return Some(value.with_timezone(&Utc));
    }
    [
        "%Y-%m-%dT%H:%M:%S%.f%#z",
        "%Y-%m-%d %H:%M:%S%.f%#z",
        "%Y-%m-%dT%H:%M%#z",
        "%Y-%m-%d %H:%M%#z",
    ]
    .iter()
    .find_map(|format| DateTime::parse_from_str(text, format).ok())
    .map(|value| value.with_timezone(&Utc))
}

impl Encode<'_, Postgres> for Bound<'_> {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        match (self.value, self.ty.oid().map(|oid| oid.0).unwrap_or(0)) {
            (Value::Null, _) => Ok(IsNull::Yes),
            (Value::Bool(v), BOOL) => <bool as Encode<Postgres>>::encode_by_ref(v, buf),
            (Value::Int(v), INT2) => {
                <i16 as Encode<Postgres>>::encode_by_ref(&i16::try_from(*v)?, buf)
            }
            (Value::Int(v), INT4) => {
                <i32 as Encode<Postgres>>::encode_by_ref(&i32::try_from(*v)?, buf)
            }
            (Value::Int(v), INT8) => <i64 as Encode<Postgres>>::encode_by_ref(v, buf),
            (Value::Int(v), FLOAT4) => <f32 as Encode<Postgres>>::encode_by_ref(&(*v as f32), buf),
            (Value::Int(v), FLOAT8) => <f64 as Encode<Postgres>>::encode_by_ref(&(*v as f64), buf),
            (Value::Float(v), FLOAT4) => {
                <f32 as Encode<Postgres>>::encode_by_ref(&(*v as f32), buf)
            }
            (Value::Float(v), FLOAT8) => <f64 as Encode<Postgres>>::encode_by_ref(v, buf),
            (Value::Int(v), NUMERIC) => BigDecimal::from(*v).encode_by_ref(buf),
            (Value::Float(v), NUMERIC) => v.to_string().parse::<BigDecimal>()?.encode_by_ref(buf),
            (Value::Text(v), UUID) => Uuid::parse_str(v)?.encode_by_ref(buf),
            (Value::Text(v), TEXT | VARCHAR | BPCHAR | NAME) => {
                <&str as Encode<Postgres>>::encode_by_ref(&v.as_str(), buf)
            }
            (Value::Text(v), TIMESTAMPTZ) => {
                let value = timestamptz(v).ok_or_else(|| mismatch(self.value, &self.ty))?;
                <DateTime<Utc> as Encode<Postgres>>::encode_by_ref(&value, buf)
            }
            (Value::Text(v), TIMESTAMP) => {
                let value = timestamp(v).ok_or_else(|| mismatch(self.value, &self.ty))?;
                <NaiveDateTime as Encode<Postgres>>::encode_by_ref(&value, buf)
            }
            (Value::Text(v), DATE) => {
                let value = NaiveDate::parse_from_str(v, "%Y-%m-%d")
                    .map_err(|_| mismatch(self.value, &self.ty))?;
                <NaiveDate as Encode<Postgres>>::encode_by_ref(&value, buf)
            }
            _ => Err(mismatch(self.value, &self.ty)),
        }
    }

    fn produces(&self) -> Option<PgTypeInfo> {
        Some(self.ty.clone())
    }
}

pub async fn fetch(conn: &mut PgConnection, compiled: &Compiled) -> Result<Json, Error> {
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
    let mut arguments = PgArguments::default();
    for (index, (value, ty)) in compiled.params.iter().zip(types).enumerate() {
        arguments
            .add(Bound { value, ty })
            .map_err(|error| Error::Parameter {
                index,
                message: error.to_string(),
            })?;
    }
    let row = statement
        .query_with(arguments)
        .fetch_one(&mut *conn)
        .await?;
    Ok(row.try_get::<Option<Json>, _>(0)?.unwrap_or(Json::Null))
}

pub async fn execute(pool: &PgPool, fields: &[CompiledField]) -> Result<Map<String, Json>, Error> {
    let mut data = Map::new();
    if !fields.iter().any(|field| field.mutation) {
        let mut conn = pool.acquire().await?;
        for field in fields {
            data.insert(
                field.response_key.clone(),
                fetch(&mut conn, &field.compiled).await?,
            );
        }
        return Ok(data);
    }
    let mut transaction = pool.begin().await?;
    for field in fields {
        match fetch(&mut transaction, &field.compiled).await {
            Ok(value) => {
                data.insert(field.response_key.clone(), value);
            }
            Err(error) => {
                transaction.rollback().await?;
                return Err(error);
            }
        }
    }
    transaction.commit().await?;
    Ok(data)
}
