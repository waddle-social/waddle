ALTER TABLE host_grant_operations
    DROP CONSTRAINT host_grant_operations_kind,
    ADD CONSTRAINT host_grant_operations_kind CHECK (
        kind IN (
            'enroll', 'set_affiliation', 'retire', 'exhaust', 'restore', 'reconstruct',
            'fence', 'reopen', 'bind_space', 'set_space_affiliations', 'configure_space'
        )
    );

CREATE FUNCTION guard_bound_space_configuration_write() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE
    bound_lifecycle TEXT;
BEGIN
    IF OLD IS NOT DISTINCT FROM NEW THEN
        RETURN NEW;
    END IF;
    -- Binding takes a conflicting lock on this same node before inserting
    -- the binding. A legacy configuration write must check after taking it.
    EXECUTE format(
        'SELECT lifecycle_id FROM %I.host_grant_space_bindings WHERE service_jid = $1 AND space_node = $2',
        TG_TABLE_SCHEMA
    ) INTO bound_lifecycle USING OLD.owner_jid, OLD.node_name;
    IF bound_lifecycle IS NOT NULL AND
        bound_lifecycle IS DISTINCT FROM current_setting('waddle.space_configuration_lifecycle', true)
    THEN
        RAISE EXCEPTION 'bound Space configuration requires the canonical writer'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER guard_bound_space_configuration_write
BEFORE UPDATE ON pubsub_nodes
FOR EACH ROW EXECUTE FUNCTION guard_bound_space_configuration_write();
