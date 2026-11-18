CREATE SCHEMA IF NOT EXISTS rusuna;

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

CREATE OR REPLACE TRIGGER "rusuna_event_article_created" AFTER INSERT ON "public"."article" FOR EACH ROW EXECUTE FUNCTION rusuna.capture_event('article_created');

CREATE OR REPLACE TRIGGER "rusuna_event_article_renamed" AFTER UPDATE OF "title", "published" ON "public"."article" FOR EACH ROW EXECUTE FUNCTION rusuna.capture_event('article_renamed', 'title', 'published');

CREATE OR REPLACE TRIGGER "rusuna_event_audit_removed" AFTER UPDATE OR DELETE ON "audit"."entry" FOR EACH ROW EXECUTE FUNCTION rusuna.capture_event('audit_removed');
