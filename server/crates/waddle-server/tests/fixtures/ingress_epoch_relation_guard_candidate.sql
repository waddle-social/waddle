-- DARK EXPERIMENT ONLY. Not a registered migration and not safe to install
-- online: the existing migration runner can already hold protected-table DDL
-- locks. The owner must quiesce writers before replacing either function.
-- Existing statement triggers continue to invoke these same function OIDs.
CREATE OR REPLACE FUNCTION waddle_ingress_epoch_guard() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY INVOKER SET search_path = pg_catalog AS $$
DECLARE
    live_epoch BIGINT;
    tx_epoch TEXT;
    tx_xid TEXT;
    epoch_relation OID;
BEGIN
    -- A transaction snapshot acquired before the barrier remains stale under
    -- REPEATABLE READ or SERIALIZABLE, even inside a VOLATILE function.
    IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'waddle: ingress guarded writes require READ COMMITTED';
    END IF;

    -- PostgreSQL has already locked the DML target when this statement
    -- trigger runs. Waiting for a first epoch lock here could deadlock with
    -- an epoch-first owner waiting for protected-table DDL. Require a lock
    -- already held on the actual schema-bound epoch relation. This proves
    -- current possession, not that the caller obtained it before other locks.
    epoch_relation := pg_catalog.to_regclass(pg_catalog.format(
        '%I.ingress_protocol_epoch', TG_TABLE_SCHEMA
    ));
    IF epoch_relation IS NULL OR NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_locks
        WHERE locktype = 'relation' AND relation = epoch_relation
          AND pid = pg_catalog.pg_backend_pid()
          AND mode = 'AccessShareLock' AND granted
    ) THEN
        RAISE EXCEPTION 'waddle: guarded writes require an epoch ACCESS SHARE prelock';
    END IF;

    -- The target trigger's schema determines the authoritative relation, not
    -- search_path or a caller-controlled temporary relation. Keep LOCK and
    -- SELECT separate: this VOLATILE function's second command sees commits
    -- which completed while the first command waited for a transition.
    EXECUTE pg_catalog.format(
        'LOCK TABLE ONLY %I.ingress_protocol_epoch IN ACCESS SHARE MODE',
        TG_TABLE_SCHEMA
    );
    EXECUTE pg_catalog.format(
        'SELECT epoch FROM %I.ingress_protocol_epoch WHERE id = 1',
        TG_TABLE_SCHEMA
    ) INTO live_epoch;
    IF live_epoch IS NULL THEN
        RAISE EXCEPTION 'waddle: ingress_protocol_epoch row missing; refusing % on %.%',
            TG_OP, TG_TABLE_SCHEMA, TG_TABLE_NAME;
    END IF;
    tx_epoch := pg_catalog.current_setting('waddle.protocol_epoch', true);
    tx_xid := pg_catalog.current_setting('waddle.protocol_epoch_xid', true);
    IF tx_epoch IS NULL OR tx_epoch <> live_epoch::text
       OR tx_xid IS NULL OR tx_xid <> pg_catalog.pg_current_xact_id()::text THEN
        RAISE EXCEPTION 'waddle: % on %.% requires a transaction-local epoch proof for %',
            TG_OP, TG_TABLE_SCHEMA, TG_TABLE_NAME, live_epoch;
    END IF;
    RETURN NULL;
END $$;

CREATE OR REPLACE FUNCTION waddle_ingress_protocol_epoch_guard() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY INVOKER SET search_path = pg_catalog AS $$
BEGIN
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
            RAISE EXCEPTION 'waddle: ingress epoch transitions require READ COMMITTED';
        END IF;
        -- UPDATE has already acquired ROW EXCLUSIVE on the epoch relation by
        -- the time this row trigger runs. Never try to upgrade here: a queued
        -- writer could deadlock with the upgrade. Check a previously granted
        -- lock on this exact relation in this backend's transaction instead.
        IF NOT EXISTS (
            SELECT 1 FROM pg_catalog.pg_locks
            WHERE locktype = 'relation' AND relation = TG_RELID
              AND pid = pg_catalog.pg_backend_pid()
              AND mode = 'AccessExclusiveLock' AND granted
        ) THEN
            RAISE EXCEPTION 'waddle: ingress epoch transition requires ACCESS EXCLUSIVE first';
        END IF;
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.epoch <> 0 AND (NEW.activated_at IS NULL OR NEW.lineage_uuid IS NULL) THEN
            RAISE EXCEPTION 'waddle: nonzero ingress protocol epochs require activation metadata';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE' THEN
        -- This GUC catches accidental duplicate transitions by a trusted
        -- operator. It is not protection against a malicious owner, who can
        -- reset the GUC or disable the trigger.
        IF pg_catalog.current_setting('waddle.protocol_epoch_advance_xid', true)
               = pg_catalog.pg_current_xact_id()::text THEN
            RAISE EXCEPTION 'waddle: only one ingress protocol epoch advance per transaction';
        END IF;
        IF NEW.epoch <> OLD.epoch + 1 THEN
            RAISE EXCEPTION 'waddle: ingress protocol epoch must advance exactly one step';
        END IF;
        IF NEW.activated_at IS NULL OR NEW.lineage_uuid IS NULL THEN
            RAISE EXCEPTION 'waddle: nonzero ingress protocol epochs require activation metadata';
        END IF;
        PERFORM pg_catalog.set_config(
            'waddle.protocol_epoch_advance_xid', pg_catalog.pg_current_xact_id()::text, true
        );
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'waddle: refusing % on ingress_protocol_epoch', TG_OP;
END $$;
