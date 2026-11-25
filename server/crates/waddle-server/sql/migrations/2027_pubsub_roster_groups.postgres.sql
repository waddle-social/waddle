-- PubSub schema version 9: `pubsub_nodes.roster_groups_allowed`, the JSON
-- array behind XEP-0060 `pubsub#roster_groups_allowed`. The PostgreSQL
-- baseline creates version 8, and migration 2016 recorded that version for
-- a database whose PubSub owner phase has not run yet. Bring the relation
-- to version 9 in place: Space bindings now reference `pubsub_nodes`, so the
-- owner phase cannot drop and recreate it.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pubsub_schema_version WHERE version NOT IN (8, 9)) THEN
        RAISE EXCEPTION 'upgrade PubSub to schema version 8 before adding roster groups';
    END IF;
END
$$;

ALTER TABLE pubsub_nodes ADD COLUMN IF NOT EXISTS roster_groups_allowed text;

UPDATE pubsub_schema_version SET version = 9 WHERE version = 8;
INSERT INTO pubsub_schema_version (version)
SELECT 9 WHERE NOT EXISTS (SELECT 1 FROM pubsub_schema_version);
