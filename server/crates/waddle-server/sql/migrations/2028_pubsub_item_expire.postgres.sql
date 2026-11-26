-- PubSub schema version 10: `pubsub_nodes.item_expire`, the number of
-- seconds after which the service purges a node's items (XEP-0060 §16.4.4
-- `pubsub#item_expire`; NULL is `max`, no expiry). The PostgreSQL baseline
-- creates version 8 and migration 2027 brings it to 9. Bring the relation
-- to version 10 in place: Space bindings reference `pubsub_nodes`, so the
-- owner phase cannot drop and recreate it.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pubsub_schema_version WHERE version NOT IN (9, 10)) THEN
        RAISE EXCEPTION 'upgrade PubSub to schema version 9 before adding item expiry';
    END IF;
END
$$;

ALTER TABLE pubsub_nodes ADD COLUMN IF NOT EXISTS item_expire bigint;

UPDATE pubsub_schema_version SET version = 10 WHERE version = 9;
INSERT INTO pubsub_schema_version (version)
SELECT 10 WHERE NOT EXISTS (SELECT 1 FROM pubsub_schema_version);
