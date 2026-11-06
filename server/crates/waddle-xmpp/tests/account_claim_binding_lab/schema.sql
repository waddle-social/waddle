-- Synthetic test schema only. It is never installed by a production migration.
CREATE TABLE lab_claim (
    bare_jid TEXT PRIMARY KEY,
    identity JSONB NOT NULL
);
CREATE TABLE lab_claim_history (
    bare_jid TEXT NOT NULL REFERENCES lab_claim(bare_jid),
    identity JSONB NOT NULL,
    PRIMARY KEY (bare_jid, identity)
);
CREATE TABLE lab_slot (
    full_jid TEXT PRIMARY KEY,
    singleton BOOLEAN NOT NULL DEFAULT TRUE UNIQUE CHECK (singleton),
    bare_jid TEXT NOT NULL REFERENCES lab_claim(bare_jid),
    revision BIGINT NOT NULL CHECK (revision >= 0),
    generation BIGINT NOT NULL CHECK (generation >= 0),
    state TEXT NOT NULL CHECK (state IN ('vacant', 'prepared', 'published', 'active', 'retiring')),
    binding_claim JSONB,
    connection_ref JSONB,
    binding_ref JSONB,
    CHECK ((state = 'vacant') = (binding_claim IS NULL)),
    CHECK ((state = 'vacant') = (connection_ref IS NULL)),
    CHECK ((state = 'vacant') = (binding_ref IS NULL))
);
-- Retained for the lifetime of the fixture, including after retirement.
-- The database, not a stale transaction's view, prevents either reuse.
CREATE TABLE lab_used_binding (
    full_jid TEXT NOT NULL REFERENCES lab_slot(full_jid),
    generation BIGINT NOT NULL CHECK (generation > 0),
    binding_ref JSONB NOT NULL UNIQUE,
    PRIMARY KEY (full_jid, generation)
);
CREATE TABLE lab_operation (
    operation_id UUID PRIMARY KEY,
    request JSONB NOT NULL,
    outcome JSONB NOT NULL
);
