-- Dark candidate only. Immutable journals remain bounded and are not reclaimed.
CREATE TABLE __SCHEMA__.sm_custody_quota (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    revision BIGINT NOT NULL DEFAULT 0
);
INSERT INTO __SCHEMA__.sm_custody_quota (id) VALUES (1);
-- Persistent after source collection: this is the candidate's per-source,
-- bare-recipient serialization point, not an interlock for any live writer.
-- This table has no independently enforced quota or retirement policy;
-- unbounded accumulation remains a serving and retention gate.
CREATE TABLE __SCHEMA__.sm_custody_route_guards (
    message_key UUID NOT NULL,
    recipient TEXT NOT NULL CHECK (recipient <> ''),
    PRIMARY KEY (message_key, recipient)
);
-- Candidate-only recipient-wide interlock. Unlike sm_custody_route_guards,
-- this row is shared by every source for a recipient. State 1 is an
-- irreversible route transition that prevents new candidate admissions.
-- This relation is deliberately unregistered and does not fence legacy SM
-- writers or authorize pending_delivery acceptance. It has no quota or safe
-- retirement protocol; disposable fixture grants permit direct state closure.
CREATE TABLE __SCHEMA__.sm_custody_recipient_route_guards (
    recipient TEXT PRIMARY KEY CHECK (recipient <> '' AND pg_catalog.strpos(recipient, '/') = 0),
    state SMALLINT NOT NULL DEFAULT 0 CHECK (state IN (0, 1)),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0)
);
CREATE TABLE __SCHEMA__.sm_custody_gates (
    gate_id UUID PRIMARY KEY,
    token UUID NOT NULL,
    resource TEXT NOT NULL,
    session_id TEXT NOT NULL UNIQUE,
    generation BIGINT NOT NULL CHECK (generation > 0),
    endpoint TEXT NOT NULL,
    node_id TEXT NOT NULL,
    node_incarnation TEXT NOT NULL,
    claim_epoch BIGINT NOT NULL,
    sealed BOOLEAN NOT NULL DEFAULT FALSE,
    appended BIGINT NOT NULL DEFAULT 0 CHECK (appended >= 0),
    exposed BIGINT NOT NULL DEFAULT 0 CHECK (exposed >= 0),
    peer_acked BIGINT NOT NULL DEFAULT 0 CHECK (peer_acked >= 0),
    UNIQUE (session_id, generation),
    CHECK (peer_acked <= exposed AND exposed <= appended),
    CHECK (appended - peer_acked < 2147483648)
);
CREATE TABLE __SCHEMA__.sm_custody_operations (
    operation_id UUID PRIMARY KEY,
    gate_id UUID NOT NULL REFERENCES __SCHEMA__.sm_custody_gates (gate_id),
    message_key UUID NOT NULL,
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BYTEA NOT NULL CHECK (octet_length(semantic_identity_hash) = 32),
    resource TEXT NOT NULL,
    attempt_token UUID NOT NULL UNIQUE,
    payload TEXT NOT NULL CHECK (octet_length(payload) BETWEEN 1 AND 262144),
    received_at TIMESTAMPTZ NOT NULL,
    UNIQUE (message_key, receipt_kind, semantic_identity_hash, resource),
    UNIQUE (operation_id, gate_id)
);
CREATE TABLE __SCHEMA__.sm_custody_slots (
    operation_id UUID PRIMARY KEY,
    gate_id UUID NOT NULL REFERENCES __SCHEMA__.sm_custody_gates (gate_id),
    ordinal BIGINT NOT NULL CHECK (ordinal > 0),
    UNIQUE (gate_id, ordinal),
    FOREIGN KEY (operation_id, gate_id) REFERENCES __SCHEMA__.sm_custody_operations (operation_id, gate_id)
);
-- A separate experimental frontier: the existing slot identifies an
-- obligation, while each row below identifies one actual serialized frame.
-- It is not the selected SM counter and has no exposure or ACK path. The
-- fixture role retains raw DML; arbitrary transactions can invert lock order
-- and are not a restricted writer boundary.
CREATE TABLE __SCHEMA__.sm_custody_frame_frontiers (
    gate_id UUID PRIMARY KEY REFERENCES __SCHEMA__.sm_custody_gates (gate_id),
    appended BIGINT NOT NULL CHECK (appended >= 0)
);
CREATE TABLE __SCHEMA__.sm_custody_frame_occurrences (
    occurrence_id UUID PRIMARY KEY,
    operation_id UUID NOT NULL,
    gate_id UUID NOT NULL,
    ordinal BIGINT NOT NULL CHECK (ordinal > 0),
    payload BYTEA NOT NULL CHECK (octet_length(payload) BETWEEN 1 AND 1048576),
    provenance BYTEA NOT NULL CHECK (octet_length(provenance) BETWEEN 1 AND 16384),
    received_at TIMESTAMPTZ NOT NULL,
    UNIQUE (gate_id, ordinal),
    FOREIGN KEY (operation_id, gate_id) REFERENCES __SCHEMA__.sm_custody_operations (operation_id, gate_id)
);
CREATE TABLE __SCHEMA__.sm_custody_terminals (
    operation_id UUID PRIMARY KEY REFERENCES __SCHEMA__.sm_custody_operations (operation_id),
    disposition INTEGER NOT NULL CHECK (disposition IN (1, 2))
);
-- Source completion is transfer to candidate custody, never transport or ACK.
CREATE TABLE __SCHEMA__.sm_custody_handoffs (
    operation_id UUID PRIMARY KEY REFERENCES __SCHEMA__.sm_custody_slots (operation_id)
);
-- A transfer is independent of the canonical row and includes its full payload.
CREATE TABLE __SCHEMA__.sm_custody_transfers (
    operation_id UUID PRIMARY KEY REFERENCES __SCHEMA__.sm_custody_operations (operation_id),
    resource TEXT NOT NULL,
    payload TEXT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL
);
CREATE FUNCTION __SCHEMA__.sm_custody_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN RAISE EXCEPTION 'immutable SM custody evidence'; END $$;
CREATE TRIGGER sm_custody_route_guards_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_route_guards FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE FUNCTION __SCHEMA__.sm_custody_recipient_route_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NEW.recipient IS DISTINCT FROM OLD.recipient
       OR (OLD.state = 1 AND NEW.state <> 1)
       OR NEW.revision < OLD.revision
       OR (NEW.state = OLD.state AND NEW.revision <> OLD.revision)
       OR (NEW.state = 1 AND OLD.state = 0 AND NEW.revision <> OLD.revision + 1) THEN
        RAISE EXCEPTION 'SM custody recipient route state cannot roll back';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_recipient_route_guard_update
BEFORE UPDATE ON __SCHEMA__.sm_custody_recipient_route_guards
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_recipient_route_guard();
CREATE TRIGGER sm_custody_recipient_route_guard_no_delete
BEFORE DELETE OR TRUNCATE ON __SCHEMA__.sm_custody_recipient_route_guards
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE TRIGGER sm_custody_operations_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_operations FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE TRIGGER sm_custody_slots_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_slots FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE TRIGGER sm_custody_frame_occurrences_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_frame_occurrences FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE TRIGGER sm_custody_frame_frontiers_no_delete BEFORE DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_frame_frontiers FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE FUNCTION __SCHEMA__.sm_custody_frame_frontier_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NEW.gate_id IS DISTINCT FROM OLD.gate_id OR NEW.appended <> OLD.appended + 1 THEN
        RAISE EXCEPTION 'SM frame frontier must advance exactly once';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_frame_frontier_guard BEFORE UPDATE ON __SCHEMA__.sm_custody_frame_frontiers
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_frame_frontier_guard();
CREATE TRIGGER sm_custody_terminals_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_terminals FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE TRIGGER sm_custody_handoffs_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_handoffs FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE TRIGGER sm_custody_transfers_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON __SCHEMA__.sm_custody_transfers FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
CREATE FUNCTION __SCHEMA__.sm_custody_gate_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF (NEW.gate_id, NEW.token, NEW.resource, NEW.session_id, NEW.generation, NEW.endpoint,
        NEW.node_id, NEW.node_incarnation, NEW.claim_epoch)
        IS DISTINCT FROM (OLD.gate_id, OLD.token, OLD.resource, OLD.session_id, OLD.generation,
        OLD.endpoint, OLD.node_id, OLD.node_incarnation, OLD.claim_epoch)
        OR (OLD.sealed AND NOT NEW.sealed)
        OR NEW.appended < OLD.appended OR NEW.exposed < OLD.exposed OR NEW.peer_acked < OLD.peer_acked THEN
        RAISE EXCEPTION 'SM custody identity or frontier cannot roll back';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_gate_guard BEFORE UPDATE ON __SCHEMA__.sm_custody_gates
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_gate_guard();
CREATE TRIGGER sm_custody_gate_no_delete BEFORE DELETE OR TRUNCATE ON __SCHEMA__.sm_custody_gates
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.sm_custody_immutable();
-- Single-row raw candidate inserts follow the same source -> recipient ->
-- gate order. Arbitrary multi-row/raw transactions are not a trusted writer
-- boundary: their row order or previously held locks can still invert it.
CREATE FUNCTION __SCHEMA__.sm_custody_lock_source(source UUID, resource TEXT, require_open BOOLEAN) RETURNS void
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE bare TEXT; route_state SMALLINT;
BEGIN
    IF pg_catalog.strpos(resource, '/') < 2 THEN
        RAISE EXCEPTION 'SM custody resource is not a full JID';
    END IF;
    bare := pg_catalog.split_part(resource, '/', 1);
    PERFORM 1 FROM __SCHEMA__.ingress_messages WHERE message_key = source FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody canonical source is absent'; END IF;
    INSERT INTO __SCHEMA__.sm_custody_route_guards (message_key, recipient)
      VALUES (source, bare) ON CONFLICT DO NOTHING;
    PERFORM 1 FROM __SCHEMA__.sm_custody_route_guards
      WHERE message_key = source AND recipient = bare FOR UPDATE;
    INSERT INTO __SCHEMA__.sm_custody_recipient_route_guards (recipient)
      VALUES (bare) ON CONFLICT DO NOTHING;
    SELECT state INTO route_state FROM __SCHEMA__.sm_custody_recipient_route_guards
      WHERE recipient = bare FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody recipient route guard is absent'; END IF;
    IF require_open AND route_state <> 0 THEN
        RAISE EXCEPTION 'SM custody recipient route is closed';
    END IF;
END $$;
CREATE FUNCTION __SCHEMA__.sm_custody_admission_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE closed BOOLEAN; gate_resource TEXT; retained_rows BIGINT; retained_bytes BIGINT;
BEGIN
    PERFORM __SCHEMA__.sm_custody_lock_source(NEW.message_key, NEW.resource, TRUE);
    SELECT sealed, resource INTO closed, gate_resource FROM __SCHEMA__.sm_custody_gates WHERE gate_id = NEW.gate_id FOR UPDATE;
    IF closed IS DISTINCT FROM FALSE OR gate_resource IS DISTINCT FROM NEW.resource THEN
        RAISE EXCEPTION 'SM custody gate is closed or resource differs';
    END IF;
    -- Produce a new parent tuple even when the retention field is already
    -- NULL. A stale REPEATABLE READ deleter must abort instead of evaluating
    -- the deletion guard against a snapshot that predates this admission.
    UPDATE __SCHEMA__.ingress_messages SET retention_eligible_at = NULL
      WHERE message_key = NEW.message_key;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody canonical source is absent'; END IF;
    -- Updating, rather than merely locking, forces a stale REPEATABLE READ
    -- writer to abort after a concurrent admission commits. At READ COMMITTED
    -- the following count sees the latest committed journal under this lock.
    UPDATE __SCHEMA__.sm_custody_quota SET revision = revision + 1 WHERE id = 1;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody quota row is absent'; END IF;
    SELECT count(*), coalesce(sum(octet_length(payload)), 0) INTO retained_rows, retained_bytes
      FROM __SCHEMA__.sm_custody_operations;
    IF retained_rows >= 1024 OR retained_bytes + octet_length(NEW.payload) > 16777216 THEN
        RAISE EXCEPTION 'SM custody retained quota exceeded';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_admission_guard BEFORE INSERT ON __SCHEMA__.sm_custody_operations
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_admission_guard();
-- The candidate has no occurrence settlement yet. Prevent the older
-- obligation-only terminal path from settling or releasing a source with
-- retained occurrence evidence. These guards do not fence legacy writers.
CREATE FUNCTION __SCHEMA__.sm_custody_frame_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE source_key UUID; source_resource TEXT; closed BOOLEAN; retained_rows BIGINT; retained_bytes BIGINT;
BEGIN
    SELECT message_key, resource INTO source_key, source_resource FROM __SCHEMA__.sm_custody_operations
      WHERE operation_id = NEW.operation_id AND gate_id = NEW.gate_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM frame obligation is absent'; END IF;
    PERFORM __SCHEMA__.sm_custody_lock_source(source_key, source_resource, TRUE);
    SELECT sealed INTO closed FROM __SCHEMA__.sm_custody_gates
      WHERE gate_id = NEW.gate_id AND resource = source_resource FOR UPDATE;
    IF closed IS DISTINCT FROM FALSE OR EXISTS (
        SELECT 1 FROM __SCHEMA__.sm_custody_terminals WHERE operation_id = NEW.operation_id
    ) OR NOT EXISTS (
        SELECT 1 FROM __SCHEMA__.sm_custody_frame_frontiers
        WHERE gate_id = NEW.gate_id AND appended = NEW.ordinal
    ) OR EXISTS (
        SELECT 1 FROM __SCHEMA__.sm_custody_slots WHERE gate_id = NEW.gate_id
    ) THEN RAISE EXCEPTION 'SM frame gate, obligation or ordinal is invalid'; END IF;
    -- A stale REPEATABLE READ contender for an old-style slot must abort
    -- instead of reading a pre-occurrence snapshot after waiting for the gate.
    UPDATE __SCHEMA__.sm_custody_gates SET appended = appended WHERE gate_id = NEW.gate_id;
    UPDATE __SCHEMA__.ingress_messages SET retention_eligible_at = NULL WHERE message_key = source_key;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM frame source is absent'; END IF;
    UPDATE __SCHEMA__.sm_custody_quota SET revision = revision + 1 WHERE id = 1;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM frame quota is absent'; END IF;
    SELECT count(*), coalesce(sum(octet_length(payload)), 0) INTO retained_rows, retained_bytes
      FROM __SCHEMA__.sm_custody_frame_occurrences;
    IF retained_rows >= 1024 OR retained_bytes + octet_length(NEW.payload) > 16777216 THEN
        RAISE EXCEPTION 'SM frame retained quota exceeded';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_frame_guard BEFORE INSERT ON __SCHEMA__.sm_custody_frame_occurrences
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_frame_guard();
CREATE FUNCTION __SCHEMA__.sm_custody_slot_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE closed BOOLEAN; source_key UUID; source_resource TEXT;
BEGIN
    SELECT message_key, resource INTO source_key, source_resource FROM __SCHEMA__.sm_custody_operations
      WHERE operation_id = NEW.operation_id AND gate_id = NEW.gate_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody slot has no operation'; END IF;
    PERFORM __SCHEMA__.sm_custody_lock_source(source_key, source_resource, TRUE);
    SELECT sealed INTO closed FROM __SCHEMA__.sm_custody_gates WHERE gate_id = NEW.gate_id FOR UPDATE;
    IF closed IS DISTINCT FROM FALSE OR NOT EXISTS (
        SELECT 1 FROM __SCHEMA__.sm_custody_operations o WHERE o.operation_id = NEW.operation_id
        AND o.gate_id = NEW.gate_id AND o.message_key = source_key AND o.resource = source_resource AND NOT EXISTS (
            SELECT 1 FROM __SCHEMA__.sm_custody_terminals t WHERE t.operation_id = o.operation_id)) OR EXISTS (
        SELECT 1 FROM __SCHEMA__.sm_custody_frame_frontiers WHERE gate_id = NEW.gate_id
    ) THEN
        RAISE EXCEPTION 'SM custody slot is no longer admissible';
    END IF;
    UPDATE __SCHEMA__.sm_custody_gates SET appended = appended WHERE gate_id = NEW.gate_id;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_slot_guard BEFORE INSERT ON __SCHEMA__.sm_custody_slots
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_slot_guard();
-- A raw marker must identify this operation's completed source attempt, not
-- a completed attempt from another resource, owner, or lease generation. The
-- completed tuple's 32-bit xmin must match the current XID's low bits. This is
-- only a structural hint: raw DML can no-op UPDATE it, and wrap can reuse
-- those bits. The disposable role is not a restricted writer boundary.
CREATE FUNCTION __SCHEMA__.sm_custody_handoff_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE source_key UUID; source_resource TEXT; closed BOOLEAN;
BEGIN
    SELECT o.message_key, o.resource INTO source_key, source_resource FROM __SCHEMA__.sm_custody_operations o
      WHERE o.operation_id = NEW.operation_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody handoff has no operation'; END IF;
    PERFORM __SCHEMA__.sm_custody_lock_source(source_key, source_resource, TRUE);
    SELECT g.sealed INTO closed FROM __SCHEMA__.sm_custody_operations o
      JOIN __SCHEMA__.sm_custody_gates g ON g.gate_id = o.gate_id
      JOIN __SCHEMA__.sm_custody_slots s ON (s.operation_id, s.gate_id) = (o.operation_id, o.gate_id)
      WHERE o.operation_id = NEW.operation_id AND o.message_key = source_key AND o.resource = source_resource FOR UPDATE OF g;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody handoff has no exact slot'; END IF;
    IF closed OR EXISTS (SELECT 1 FROM __SCHEMA__.sm_custody_terminals t
        WHERE t.operation_id = NEW.operation_id) THEN
        RAISE EXCEPTION 'SM custody handoff gate is closed';
    END IF;
    PERFORM 1 FROM __SCHEMA__.ingress_send_attempts a
      JOIN __SCHEMA__.sm_custody_operations o ON a.message_key = o.message_key
        AND a.kind = o.receipt_kind AND a.semantic_identity_hash = o.semantic_identity_hash
        AND a.recipient = o.resource AND a.lease_token = o.attempt_token::text
      JOIN __SCHEMA__.sm_custody_gates g ON g.gate_id = o.gate_id
        AND a.node_id = g.node_id AND a.node_incarnation = g.node_incarnation
      WHERE o.operation_id = NEW.operation_id AND a.state = 2
        AND a.xmin = ((pg_catalog.pg_current_xact_id()::text::numeric % 4294967296)::text)::xid
      FOR UPDATE OF a;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody handoff attempt differs'; END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_handoff_guard BEFORE INSERT ON __SCHEMA__.sm_custody_handoffs
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_handoff_guard();
-- A transfer is an exact copy, so its immutable byte total cannot exceed the
-- already bounded admission journal. Serialize raw writers at the same gate.
CREATE FUNCTION __SCHEMA__.sm_custody_transfer_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE source_key UUID; source_resource TEXT;
BEGIN
    SELECT message_key, resource INTO source_key, source_resource FROM __SCHEMA__.sm_custody_operations
      WHERE operation_id = NEW.operation_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody transfer has no operation'; END IF;
    PERFORM __SCHEMA__.sm_custody_lock_source(source_key, source_resource, FALSE);
    PERFORM 1 FROM __SCHEMA__.sm_custody_gates g JOIN __SCHEMA__.sm_custody_operations o
      ON o.gate_id = g.gate_id WHERE o.operation_id = NEW.operation_id
        AND o.message_key = source_key AND o.resource = source_resource FOR UPDATE OF g;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody transfer gate is absent'; END IF;
    IF NOT EXISTS (SELECT 1 FROM __SCHEMA__.sm_custody_operations o
        JOIN __SCHEMA__.sm_custody_slots s ON (s.operation_id, s.gate_id) = (o.operation_id, o.gate_id)
        JOIN __SCHEMA__.sm_custody_handoffs h ON h.operation_id = s.operation_id
        WHERE o.operation_id = NEW.operation_id AND
          (o.resource, o.payload, o.received_at) = (NEW.resource, NEW.payload, NEW.received_at)) THEN
        RAISE EXCEPTION 'SM custody transfer differs from source-marked admission';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_transfer_guard BEFORE INSERT ON __SCHEMA__.sm_custody_transfers
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_transfer_guard();
CREATE FUNCTION __SCHEMA__.sm_custody_terminal_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE source_key UUID; source_resource TEXT;
BEGIN
    SELECT message_key, resource INTO source_key, source_resource FROM __SCHEMA__.sm_custody_operations
      WHERE operation_id = NEW.operation_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody terminal has no operation'; END IF;
    PERFORM __SCHEMA__.sm_custody_lock_source(source_key, source_resource, FALSE);
    PERFORM 1 FROM __SCHEMA__.sm_custody_gates g JOIN __SCHEMA__.sm_custody_operations o
      ON o.gate_id = g.gate_id WHERE o.operation_id = NEW.operation_id
        AND o.message_key = source_key AND o.resource = source_resource FOR UPDATE OF g;
    IF NOT FOUND THEN RAISE EXCEPTION 'SM custody terminal gate is absent'; END IF;
    IF EXISTS (SELECT 1 FROM __SCHEMA__.sm_custody_frame_occurrences
               WHERE operation_id = NEW.operation_id) THEN
        RAISE EXCEPTION 'SM frame occurrence remains unsettled';
    END IF;
    IF NEW.disposition = 2 AND NOT EXISTS (
        SELECT 1 FROM __SCHEMA__.sm_custody_transfers x
        JOIN __SCHEMA__.sm_custody_handoffs h ON h.operation_id = x.operation_id
        JOIN __SCHEMA__.sm_custody_slots s ON s.operation_id = h.operation_id
        JOIN __SCHEMA__.sm_custody_operations o ON (o.operation_id, o.gate_id) = (s.operation_id, s.gate_id)
        WHERE x.operation_id = NEW.operation_id
          AND (x.resource, x.payload, x.received_at) = (o.resource, o.payload, o.received_at)
    ) THEN RAISE EXCEPTION 'SM custody transfer terminal has no marked copy'; END IF;
    -- Terminal and frame occurrence contend on the same tuple version. A
    -- stale REPEATABLE READ inserter must abort after waiting, not miss a
    -- committed terminal in its older snapshot.
    UPDATE __SCHEMA__.sm_custody_gates SET appended = appended WHERE gate_id = (
        SELECT gate_id FROM __SCHEMA__.sm_custody_operations WHERE operation_id = NEW.operation_id
    );
    RETURN NEW;
END $$;
CREATE TRIGGER sm_custody_terminal_guard BEFORE INSERT ON __SCHEMA__.sm_custody_terminals
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_terminal_guard();
CREATE FUNCTION __SCHEMA__.sm_custody_parent_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM __SCHEMA__.sm_custody_operations o WHERE o.message_key = OLD.message_key
        AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.sm_custody_terminals t WHERE t.operation_id = o.operation_id)) THEN
        RAISE EXCEPTION 'pending SM custody prevents canonical deletion';
    END IF;
    RETURN OLD;
END $$;
CREATE TRIGGER sm_custody_parent_guard BEFORE DELETE ON __SCHEMA__.ingress_messages
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.sm_custody_parent_guard();
