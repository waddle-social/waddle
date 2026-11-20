-- A room can have at most one current bookmark version in its enrolled Space.
-- Keep retractions so an older queued event cannot match a later absence.
ALTER TABLE host_grant_rooms ADD CONSTRAINT host_grant_rooms_scope_address
    UNIQUE (lifecycle_id, room_jid);

CREATE TABLE host_grant_space_bookmark_versions (
    lifecycle_id TEXT NOT NULL REFERENCES host_grant_space_bindings (lifecycle_id),
    room_jid TEXT NOT NULL,
    authority_incarnation TEXT NOT NULL CHECK (authority_incarnation ~ '^[0-9a-f]{32}$'),
    operation_token TEXT NOT NULL CHECK (operation_token ~ '^[0-9a-f]{32}$'),
    present BOOLEAN NOT NULL,
    PRIMARY KEY (lifecycle_id, room_jid),
    FOREIGN KEY (lifecycle_id, room_jid) REFERENCES host_grant_rooms (lifecycle_id, room_jid)
);

-- Bind and item writes serialize on the PubSub node row. A legacy write
-- which has already reached storage finishes before the binding can commit.
CREATE FUNCTION guard_bound_space_item_write() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_owner TEXT;
    target_node TEXT;
    bound_lifecycle TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_owner := OLD.owner_jid;
        target_node := OLD.node_name;
    ELSE
        target_owner := NEW.owner_jid;
        target_node := NEW.node_name;
    END IF;
    IF TG_OP = 'UPDATE' AND
       (OLD.owner_jid, OLD.node_name) IS DISTINCT FROM (NEW.owner_jid, NEW.node_name)
    THEN
        RAISE EXCEPTION 'PubSub item node identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    EXECUTE format('SELECT 1 FROM %I.pubsub_nodes WHERE owner_jid = $1 AND node_name = $2 FOR KEY SHARE',
                   TG_TABLE_SCHEMA) USING target_owner, target_node;
    EXECUTE format('SELECT lifecycle_id FROM %I.host_grant_space_bindings WHERE service_jid = $1 AND space_node = $2',
                   TG_TABLE_SCHEMA) INTO bound_lifecycle USING target_owner, target_node;
    IF bound_lifecycle IS NOT NULL AND
       bound_lifecycle IS DISTINCT FROM current_setting('waddle.space_bookmark_lifecycle', true)
    THEN
        RAISE EXCEPTION 'bound Space items require the canonical writer' USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER guard_bound_space_item_write
BEFORE INSERT OR UPDATE OR DELETE ON pubsub_items
FOR EACH ROW EXECUTE FUNCTION guard_bound_space_item_write();
