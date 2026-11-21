-- Host-grant authority (ADR-0020 D5): the canonical grant store.
--
-- PostgreSQL is the canonical authority for an enrolled scope's grants and
-- SpiceDB a fenced projection of them. The tables carry no trigger, function
-- or foreign key: the runtime role's privileges (the owner phase's grant
-- matrix) make the lifecycle ledger, the room bindings and the operation
-- journal append-only for it, and the store's transactions serialise every
-- change of a scope on its `host_grant_scopes` row.
-- See server/docs/planning/host-grant-authority-2047.md.

-- The authority incarnation every scope and projection marker is bound to.
-- The owner phase alone changes it.
CREATE TABLE host_grant_authority (
    singleton BOOLEAN PRIMARY KEY DEFAULT true,
    incarnation TEXT NOT NULL,
    CONSTRAINT host_grant_authority_singleton CHECK (singleton),
    CONSTRAINT host_grant_authority_incarnation CHECK (incarnation ~ '^[0-9a-f]{32}$')
);

INSERT INTO host_grant_authority (singleton, incarnation)
VALUES (true, replace(gen_random_uuid()::text, '-', ''));

-- One row per scope lifecycle, never deleted. A scope key holds one
-- lifecycle: a lifecycle identity is never reused and a key is never
-- enrolled twice. The row is written twice at most, both times by the schema
-- owner: when the scope is enrolled, and when it is retired, which stamps
-- `retired_at_ms`. The runtime role only reads the table, so it can neither
-- enroll nor retire a scope.
CREATE TABLE host_grant_scope_lifecycles (
    lifecycle_id TEXT PRIMARY KEY,
    service_jid TEXT NOT NULL,
    space_node TEXT NOT NULL,
    enrolled_at_ms BIGINT NOT NULL,
    retired_at_ms BIGINT,
    CONSTRAINT host_grant_scope_lifecycles_id CHECK (lifecycle_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT host_grant_scope_lifecycles_key UNIQUE (service_jid, space_node)
);

-- The latest state of a scope. Its row lock is the scope's authority guard:
-- a mutation or activation holds it exclusively and an acceptance transaction
-- holds it shared until it commits.
CREATE TABLE host_grant_scopes (
    lifecycle_id TEXT PRIMARY KEY,
    authority_incarnation TEXT NOT NULL,
    policy_version INTEGER NOT NULL,
    generation BIGINT NOT NULL,
    operation_id TEXT NOT NULL,
    projected_generation BIGINT NOT NULL,
    admission_state TEXT NOT NULL,
    closed_reason TEXT,
    updated_at_ms BIGINT NOT NULL,
    CONSTRAINT host_grant_scopes_policy_version CHECK (policy_version > 0),
    CONSTRAINT host_grant_scopes_generation CHECK (generation > 0),
    CONSTRAINT host_grant_scopes_projected CHECK (
        projected_generation >= 0 AND projected_generation <= generation
    ),
    CONSTRAINT host_grant_scopes_state CHECK (admission_state IN ('closed', 'active')),
    CONSTRAINT host_grant_scopes_reason CHECK (
        (admission_state = 'active' AND closed_reason IS NULL AND projected_generation > 0)
        OR (admission_state = 'closed'
            AND closed_reason IN ('enrolled', 'retired', 'exhausted'))
    )
);

-- The rooms a scope manages. A room is bound once and its binding is not
-- changed; `channel_id` is the room's authorization object.
CREATE TABLE host_grant_rooms (
    room_jid TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    lifecycle_id TEXT NOT NULL,
    access_model TEXT NOT NULL,
    bound_at_ms BIGINT NOT NULL,
    CONSTRAINT host_grant_rooms_channel UNIQUE (channel_id),
    CONSTRAINT host_grant_rooms_access_model CHECK (access_model = 'members_only')
);

CREATE INDEX idx_host_grant_rooms_lifecycle ON host_grant_rooms (lifecycle_id);

-- The latest explicit affiliation of each subject in each room. `affiliation`
-- is the recorded intent; `effective_affiliation` is what admission honours.
-- A revoke is a row whose affiliation is `none`, never a deleted row.
CREATE TABLE host_grants (
    lifecycle_id TEXT NOT NULL,
    room_jid TEXT NOT NULL,
    subject_jid TEXT NOT NULL,
    affiliation TEXT NOT NULL,
    effective_affiliation TEXT NOT NULL,
    changed_generation BIGINT NOT NULL,
    operation_id TEXT NOT NULL,
    issuance_basis TEXT NOT NULL,
    PRIMARY KEY (lifecycle_id, room_jid, subject_jid),
    CONSTRAINT host_grants_affiliation CHECK (
        affiliation IN ('owner', 'admin', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grants_effective_affiliation CHECK (
        effective_affiliation IN ('owner', 'admin', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grants_changed_generation CHECK (changed_generation > 0),
    CONSTRAINT host_grants_issuance_basis CHECK (
        issuance_basis IN ('local', 'trusted_home', 'unprivileged')
    )
);

-- The immutable outcome of each operation. One operation holds each
-- generation of a lifecycle.
CREATE TABLE host_grant_operations (
    operation_id TEXT PRIMARY KEY,
    lifecycle_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    fingerprint TEXT NOT NULL,
    kind TEXT NOT NULL,
    initiator_jid TEXT,
    authority_incarnation TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    recorded_at_ms BIGINT NOT NULL,
    CONSTRAINT host_grant_operations_generation UNIQUE (lifecycle_id, generation),
    CONSTRAINT host_grant_operations_generation_positive CHECK (generation > 0),
    CONSTRAINT host_grant_operations_fingerprint CHECK (fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT host_grant_operations_digest CHECK (projection_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT host_grant_operations_kind CHECK (
        kind IN ('enroll', 'set_affiliation', 'retire', 'exhaust')
    )
);

-- The grant changes an operation recorded, in order.
CREATE TABLE host_grant_operation_changes (
    operation_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    room_jid TEXT NOT NULL,
    subject_jid TEXT NOT NULL,
    affiliation TEXT NOT NULL,
    previous_affiliation TEXT NOT NULL,
    issuance_basis TEXT NOT NULL,
    PRIMARY KEY (operation_id, ordinal),
    CONSTRAINT host_grant_operation_changes_ordinal CHECK (ordinal >= 0),
    CONSTRAINT host_grant_operation_changes_affiliation CHECK (
        affiliation IN ('owner', 'admin', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grant_operation_changes_previous CHECK (
        previous_affiliation IN ('owner', 'admin', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grant_operation_changes_basis CHECK (
        issuance_basis IN ('local', 'trusted_home', 'unprivileged')
    )
);
