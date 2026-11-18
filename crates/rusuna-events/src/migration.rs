use crate::{Error, EventTrigger, Operation, validate_name};
use std::collections::HashSet;
use std::fmt::Write;

const BASE: &str = r#"CREATE SCHEMA IF NOT EXISTS rusuna;

CREATE TABLE IF NOT EXISTS rusuna.event_log (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind text NOT NULL CHECK (kind IN ('row', 'cron', 'scheduled')),
    trigger_name text NOT NULL,
    schema_name text,
    table_name text,
    operation text CHECK (operation IN ('INSERT', 'UPDATE', 'DELETE')),
    old jsonb,
    new jsonb,
    payload jsonb,
    scheduled_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered', 'failed')),
    tries integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz,
    locked_until timestamptz,
    completed_at timestamptz
);

CREATE INDEX IF NOT EXISTS event_log_pending ON rusuna.event_log (created_at) WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS event_log_completed ON rusuna.event_log (completed_at) WHERE status <> 'pending';

CREATE UNIQUE INDEX IF NOT EXISTS event_log_cron ON rusuna.event_log (trigger_name, scheduled_at) WHERE kind = 'cron';

CREATE TABLE IF NOT EXISTS rusuna.event_invocation (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id uuid NOT NULL REFERENCES rusuna.event_log (id) ON DELETE CASCADE,
    attempt integer NOT NULL,
    succeeded boolean NOT NULL,
    status integer,
    error text,
    started_at timestamptz NOT NULL,
    finished_at timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS event_invocation_event ON rusuna.event_invocation (event_id);

CREATE OR REPLACE FUNCTION rusuna.capture_event() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $function$
DECLARE
    old_row jsonb;
    new_row jsonb;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        old_row := to_jsonb(OLD);
    END IF;
    IF TG_OP <> 'DELETE' THEN
        new_row := to_jsonb(NEW);
    END IF;
    IF TG_OP = 'UPDATE' AND TG_NARGS > 1 AND NOT EXISTS (
        SELECT FROM unnest(TG_ARGV[1:TG_NARGS - 1]) AS c(name)
        WHERE old_row -> c.name IS DISTINCT FROM new_row -> c.name
    ) THEN
        RETURN NULL;
    END IF;
    INSERT INTO rusuna.event_log (kind, trigger_name, schema_name, table_name, operation, old, new)
    VALUES ('row', TG_ARGV[0], TG_TABLE_SCHEMA, TG_TABLE_NAME, TG_OP, old_row, new_row);
    PERFORM pg_notify('rusuna_events', TG_ARGV[0]);
    RETURN NULL;
END
$function$;
"#;

fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn validate(triggers: &[EventTrigger]) -> Result<(), Error> {
    let mut names = HashSet::new();
    for trigger in triggers {
        validate_name(&trigger.name)?;
        if !names.insert(&trigger.name) {
            return Err(Error::Definition(format!(
                "the trigger {} is declared twice",
                trigger.name
            )));
        }
        let invalid = |message: &str| Error::Definition(format!("{}: {message}", trigger.name));
        if trigger.table.is_empty() || trigger.schema.is_empty() {
            return Err(invalid("the table and the schema must not be empty"));
        }
        if trigger.operations.is_empty() {
            return Err(invalid("the trigger has no operation"));
        }
        let operations: HashSet<_> = trigger.operations.iter().collect();
        if operations.len() != trigger.operations.len() {
            return Err(invalid("an operation is listed twice"));
        }
        if !trigger.columns.is_empty() && !operations.contains(&Operation::Update) {
            return Err(invalid("columns apply to update triggers only"));
        }
        let columns: HashSet<_> = trigger.columns.iter().collect();
        if columns.len() != trigger.columns.len() || columns.contains(&String::new()) {
            return Err(invalid("the columns must be distinct and not empty"));
        }
    }
    Ok(())
}

/// Returns the SQL that creates the event tables, the trigger function and one row trigger
/// per declaration.
///
/// The script is idempotent: it uses `CREATE … IF NOT EXISTS`, `CREATE OR REPLACE
/// FUNCTION` and `CREATE OR REPLACE TRIGGER`, so it requires PostgreSQL 14 or later.
/// Applying it again with changed declarations replaces the trigger of each declared name
/// on its table; it does not drop the triggers of removed declarations. The trigger of the
/// declaration `name` is called `rusuna_event_<name>`. The script contains no transaction
/// statements, so it can run inside a migration tool's transaction.
pub fn migration(triggers: &[EventTrigger]) -> Result<String, Error> {
    validate(triggers)?;
    let mut sql = BASE.to_string();
    for trigger in triggers {
        let events = [Operation::Insert, Operation::Update, Operation::Delete]
            .into_iter()
            .filter(|op| trigger.operations.contains(op))
            .map(|op| {
                if op == Operation::Update && !trigger.columns.is_empty() {
                    let columns: Vec<_> = trigger.columns.iter().map(|c| identifier(c)).collect();
                    format!("UPDATE OF {}", columns.join(", "))
                } else {
                    op.as_str().into()
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let arguments = std::iter::once(&trigger.name)
            .chain(&trigger.columns)
            .map(|a| literal(a))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            sql,
            "\nCREATE OR REPLACE TRIGGER {} AFTER {events} ON {}.{} FOR EACH ROW EXECUTE FUNCTION rusuna.capture_event({arguments});\n",
            identifier(&format!("rusuna_event_{}", trigger.name)),
            identifier(&trigger.schema),
            identifier(&trigger.table),
        )
        .unwrap();
    }
    Ok(sql)
}

/// Applies [`migration`] on `pool` in one transaction, serialized with other callers by an
/// advisory lock.
pub async fn migrate(pool: &sqlx::PgPool, triggers: &[EventTrigger]) -> Result<(), Error> {
    let sql = migration(triggers)?;
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rusuna_events_migration'))")
        .execute(&mut *transaction)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_quote_names_and_list_update_columns() {
        let sql = migration(&[EventTrigger::new("a", "we\"ird")
            .schema("app")
            .operations([Operation::Update, Operation::Insert])
            .columns(["it's"])])
        .unwrap();
        assert!(sql.ends_with(
            "\nCREATE OR REPLACE TRIGGER \"rusuna_event_a\" AFTER INSERT OR UPDATE OF \"it's\" ON \"app\".\"we\"\"ird\" FOR EACH ROW EXECUTE FUNCTION rusuna.capture_event('a', 'it''s');\n"
        ));
    }

    #[test]
    fn invalid_declarations_are_rejected() {
        let cases = [
            vec![EventTrigger::new("a", "t"), EventTrigger::new("a", "u")],
            vec![EventTrigger::new("a b", "t")],
            vec![EventTrigger::new("a", "t").operations([])],
            vec![EventTrigger::new("a", "t").operations([Operation::Insert, Operation::Insert])],
            vec![
                EventTrigger::new("a", "t")
                    .operations([Operation::Insert])
                    .columns(["c"]),
            ],
            vec![EventTrigger::new("a", "t").columns(["c", "c"])],
            vec![EventTrigger::new("a", "")],
        ];
        for triggers in cases {
            assert!(migration(&triggers).is_err(), "{triggers:?}");
        }
    }
}
