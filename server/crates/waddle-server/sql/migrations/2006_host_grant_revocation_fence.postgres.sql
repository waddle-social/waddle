-- Host-grant revocation fence (ADR-0020 D5): which replicas decide for
-- enrolled scopes, and how far each has taken a scope's changes.
--
-- A revoke takes effect at its PostgreSQL commit. Its acknowledgement also
-- waits until every replica that holds a live lease here has recorded the
-- revoke's generation, or the lease has run out. A replica decides nothing
-- for a managed room without a lease it renewed recently, so a replica that
-- stops renewing stops deciding before a revoke stops waiting for it.
-- The tables carry no trigger, function or foreign key, and the runtime
-- role may not delete from them: an expired lease is a row that no longer
-- counts, and the owner phase removes old rows.
-- See server/docs/planning/host-grant-authority-2047.md.

-- One row per registration of a replica. A replica that let its lease run
-- out registers again under a new identity.
CREATE TABLE host_grant_fence_replicas (
    replica_id TEXT PRIMARY KEY,
    registered_at_ms BIGINT NOT NULL,
    lease_expires_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT host_grant_fence_replicas_id CHECK (replica_id ~ '^[0-9a-f]{32}$')
);

CREATE INDEX host_grant_fence_replicas_lease ON host_grant_fence_replicas (lease_expires_at);

-- The latest generation of a scope that a replica has installed: from that
-- generation on it decides nothing for the scope from an older read.
CREATE TABLE host_grant_fence_acks (
    replica_id TEXT NOT NULL,
    lifecycle_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    acknowledged_at_ms BIGINT NOT NULL,
    PRIMARY KEY (replica_id, lifecycle_id),
    CONSTRAINT host_grant_fence_acks_replica CHECK (replica_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT host_grant_fence_acks_lifecycle CHECK (lifecycle_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT host_grant_fence_acks_generation CHECK (generation > 0)
);
