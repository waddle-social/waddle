-- Unregistered, isolated P3 schema experiment. Equality and foreign keys do
-- not authorize a claim, validate a principal, or implement P2 custody. These
-- tables are only installed in a disposable test schema by the lab test.
CREATE TABLE p3_user_claim (
    bare_jid TEXT PRIMARY KEY,
    owner_node TEXT NOT NULL,
    owner_epoch TEXT NOT NULL,
    claim_epoch BIGINT NOT NULL CHECK (claim_epoch >= 0),
    actor_incarnation UUID NOT NULL,
    UNIQUE (bare_jid, owner_node, owner_epoch, claim_epoch, actor_incarnation)
);

CREATE TABLE p3_slot (
    slot_id UUID PRIMARY KEY,
    bare_jid TEXT NOT NULL,
    full_jid TEXT NOT NULL UNIQUE,
    revision BIGINT NOT NULL CHECK (revision >= 0),
    install_counter BIGINT NOT NULL CHECK (install_counter >= 0),
    current_resource UUID,
    current_install BIGINT,
    CHECK ((current_resource IS NULL) = (current_install IS NULL)),
    CHECK (current_install IS NULL OR current_install <= install_counter),
    UNIQUE (slot_id, bare_jid)
);

CREATE TABLE p3_resource (
    resource_id UUID PRIMARY KEY,
    slot_id UUID NOT NULL,
    bare_jid TEXT NOT NULL,
    occupancy_id UUID NOT NULL UNIQUE,
    user_owner_node TEXT NOT NULL,
    user_owner_epoch TEXT NOT NULL,
    user_claim_epoch BIGINT NOT NULL,
    actor_incarnation UUID NOT NULL,
    binding_id UUID NOT NULL,
    install_generation BIGINT NOT NULL CHECK (install_generation > 0),
    state TEXT NOT NULL CHECK (state IN ('prepared', 'published', 'active', 'detached', 'retiring')),
    sm_key UUID,
    sm_generation BIGINT,
    CHECK ((sm_key IS NULL) = (sm_generation IS NULL)),
    CHECK (sm_generation IS NULL OR sm_generation > 0),
    UNIQUE (resource_id, bare_jid),
    UNIQUE (resource_id, state),
    UNIQUE (resource_id, sm_key, sm_generation, binding_id, install_generation),
    UNIQUE (slot_id, resource_id, install_generation),
    FOREIGN KEY (slot_id, bare_jid) REFERENCES p3_slot(slot_id, bare_jid)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (bare_jid, user_owner_node, user_owner_epoch, user_claim_epoch, actor_incarnation)
        REFERENCES p3_user_claim(bare_jid, owner_node, owner_epoch, claim_epoch, actor_incarnation)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE p3_binding (
    binding_id UUID PRIMARY KEY,
    resource_id UUID NOT NULL REFERENCES p3_resource(resource_id) DEFERRABLE INITIALLY DEFERRED,
    node_id TEXT NOT NULL,
    node_epoch TEXT NOT NULL,
    physical_generation UUID NOT NULL,
    install_generation BIGINT NOT NULL CHECK (install_generation > 0),
    UNIQUE (resource_id, binding_id, install_generation)
);

-- Stable identity and payload owner outlive the one current session row.
CREATE TABLE p3_sm_identity (
    sm_key UUID PRIMARY KEY,
    previd_sha256 BYTEA NOT NULL UNIQUE CHECK (octet_length(previd_sha256) = 32),
    principal_bare_jid TEXT NOT NULL,
    auth_context_id UUID NOT NULL,
    auth_context_version BIGINT NOT NULL CHECK (auth_context_version > 0),
    principal_auth_epoch BIGINT NOT NULL CHECK (principal_auth_epoch > 0),
    lifecycle TEXT NOT NULL DEFAULT 'current' CHECK (lifecycle IN ('current', 'terminal')),
    UNIQUE (sm_key, lifecycle),
    UNIQUE (sm_key, principal_bare_jid, auth_context_id, auth_context_version, principal_auth_epoch, lifecycle)
);

-- A retained terminal identity is irreversible in this laboratory. Future GC
-- needs a separately reviewed no-reuse marker and retention contract.
CREATE FUNCTION p3_sm_identity_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.lifecycle = 'terminal' THEN
            RAISE EXCEPTION 'terminal SM identity cannot be deleted' USING ERRCODE = '23514';
        END IF;
        RETURN OLD;
    END IF;
    IF (NEW.sm_key, NEW.previd_sha256, NEW.principal_bare_jid, NEW.auth_context_id,
        NEW.auth_context_version, NEW.principal_auth_epoch)
       IS DISTINCT FROM
       (OLD.sm_key, OLD.previd_sha256, OLD.principal_bare_jid, OLD.auth_context_id,
        OLD.auth_context_version, OLD.principal_auth_epoch) THEN
        RAISE EXCEPTION 'SM identity cannot be reassigned' USING ERRCODE = '23514';
    END IF;
    IF OLD.lifecycle = 'terminal' AND NEW.lifecycle <> 'terminal' THEN
        RAISE EXCEPTION 'terminal SM identity cannot be revived' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER p3_sm_identity_immutable_before_write
    BEFORE UPDATE OR DELETE ON p3_sm_identity
    FOR EACH ROW EXECUTE FUNCTION p3_sm_identity_immutable();

CREATE TABLE p3_sm_session (
    sm_key UUID PRIMARY KEY,
    resource_id UUID NOT NULL,
    principal_bare_jid TEXT NOT NULL,
    auth_context_id UUID NOT NULL,
    auth_context_version BIGINT NOT NULL CHECK (auth_context_version > 0),
    principal_auth_epoch BIGINT NOT NULL CHECK (principal_auth_epoch > 0),
    sm_owner_node TEXT NOT NULL,
    sm_owner_epoch TEXT NOT NULL,
    sm_claim_epoch BIGINT NOT NULL CHECK (sm_claim_epoch >= 0),
    sm_generation BIGINT NOT NULL CHECK (sm_generation > 0),
    binding_id UUID NOT NULL,
    install_generation BIGINT NOT NULL CHECK (install_generation > 0),
    resource_state TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('enabling_live', 'live', 'detached', 'transfer_hydrating', 'terminal')),
    peer_acked BIGINT NOT NULL CHECK (peer_acked >= 0),
    exposed BIGINT NOT NULL CHECK (exposed >= peer_acked),
    appended BIGINT NOT NULL CHECK (appended >= exposed),
    inbound_handled BIGINT NOT NULL CHECK (inbound_handled >= 0),
    identity_lifecycle TEXT NOT NULL DEFAULT 'current' CHECK (identity_lifecycle = 'current'),
    CHECK (appended - peer_acked < 2147483648),
    CHECK ((state = 'live' AND resource_state = 'active')
        OR (state = 'detached' AND resource_state = 'detached')
        OR (state = 'enabling_live' AND resource_state IN ('prepared', 'published', 'active'))
        OR (state = 'transfer_hydrating' AND resource_state IN ('prepared', 'published'))
        OR (state = 'terminal' AND resource_state = 'retiring')),
    UNIQUE (resource_id, sm_key, sm_generation, binding_id, install_generation),
    UNIQUE (resource_id),
    CONSTRAINT p3_sm_session_current_identity_fk
    FOREIGN KEY (sm_key, principal_bare_jid, auth_context_id, auth_context_version, principal_auth_epoch, identity_lifecycle)
        REFERENCES p3_sm_identity(sm_key, principal_bare_jid, auth_context_id, auth_context_version, principal_auth_epoch, lifecycle)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (resource_id, principal_bare_jid) REFERENCES p3_resource(resource_id, bare_jid)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (resource_id, resource_state) REFERENCES p3_resource(resource_id, state)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (resource_id, binding_id, install_generation)
        REFERENCES p3_binding(resource_id, binding_id, install_generation)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (resource_id, sm_key, sm_generation, binding_id, install_generation)
        REFERENCES p3_resource(resource_id, sm_key, sm_generation, binding_id, install_generation)
        DEFERRABLE INITIALLY DEFERRED
);

-- Archiving and deleting the current row in one transaction permits another
-- session on the same logical resource. Retained rows continue to reference
-- the stable identity, not the replaceable current-session row.
CREATE TABLE p3_sm_tombstone (
    sm_key UUID PRIMARY KEY,
    resource_id UUID NOT NULL,
    occupancy_id UUID NOT NULL,
    binding_id UUID NOT NULL,
    install_generation BIGINT NOT NULL CHECK (install_generation > 0),
    sm_generation BIGINT NOT NULL CHECK (sm_generation > 0),
    sm_owner_node TEXT NOT NULL,
    sm_owner_epoch TEXT NOT NULL,
    sm_claim_epoch BIGINT NOT NULL CHECK (sm_claim_epoch >= 0),
    user_owner_node TEXT NOT NULL,
    user_owner_epoch TEXT NOT NULL,
    user_claim_epoch BIGINT NOT NULL CHECK (user_claim_epoch >= 0),
    actor_incarnation UUID NOT NULL,
    peer_acked BIGINT NOT NULL CHECK (peer_acked >= 0),
    exposed BIGINT NOT NULL CHECK (exposed >= peer_acked),
    appended BIGINT NOT NULL CHECK (appended >= exposed),
    inbound_handled BIGINT NOT NULL CHECK (inbound_handled >= 0),
    identity_lifecycle TEXT NOT NULL DEFAULT 'terminal' CHECK (identity_lifecycle = 'terminal'),
    CONSTRAINT p3_sm_tombstone_terminal_identity_fk
        FOREIGN KEY (sm_key, identity_lifecycle) REFERENCES p3_sm_identity(sm_key, lifecycle)
        DEFERRABLE INITIALLY DEFERRED
);

-- Synthetic bytes represent retained rows only; this table does not implement
-- P2 canonical custody or a production SM replay queue.
CREATE TABLE p3_sm_retained (
    sm_key UUID NOT NULL REFERENCES p3_sm_identity(sm_key),
    ordinal BIGINT NOT NULL CHECK (ordinal > 0),
    payload BYTEA NOT NULL,
    PRIMARY KEY (sm_key, ordinal)
);

ALTER TABLE p3_resource ADD CONSTRAINT p3_resource_binding_exact
    FOREIGN KEY (resource_id, binding_id, install_generation)
    REFERENCES p3_binding(resource_id, binding_id, install_generation)
    DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE p3_resource ADD CONSTRAINT p3_resource_sm_exact
    FOREIGN KEY (resource_id, sm_key, sm_generation, binding_id, install_generation)
    REFERENCES p3_sm_session(resource_id, sm_key, sm_generation, binding_id, install_generation)
    DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE p3_slot ADD CONSTRAINT p3_slot_resource_exact
    FOREIGN KEY (slot_id, current_resource, current_install)
    REFERENCES p3_resource(slot_id, resource_id, install_generation)
    DEFERRABLE INITIALLY DEFERRED;

-- Enrol the exact payload before a worker can start. Both a writer and a
-- reconciler must lock this same row before checking and changing its state.
-- The lab shows cooperative serialization only: direct table DML is not fenced.
CREATE TABLE p3_operation (
    operation_id UUID PRIMARY KEY,
    payload_sha256 BYTEA NOT NULL CHECK (octet_length(payload_sha256) = 32),
    state TEXT NOT NULL CHECK (state IN ('pending', 'committed', 'canceled')),
    result_revision BIGINT,
    CHECK ((state = 'committed') = (result_revision IS NOT NULL)),
    CHECK (result_revision IS NULL OR result_revision >= 0)
);
