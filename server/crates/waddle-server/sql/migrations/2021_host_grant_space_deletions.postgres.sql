ALTER TABLE host_grant_operations
    DROP CONSTRAINT host_grant_operations_kind,
    ADD CONSTRAINT host_grant_operations_kind CHECK (
        kind IN (
            'enroll', 'set_affiliation', 'retire', 'exhaust', 'restore', 'reconstruct',
            'fence', 'reopen', 'bind_space', 'set_space_affiliations', 'configure_space',
            'delete_space'
        )
    );

-- A deleted Space retains its address reservation and its shared room scope.
CREATE TABLE host_grant_space_deletions (
    lifecycle_id TEXT PRIMARY KEY REFERENCES host_grant_space_bindings (lifecycle_id),
    operation_id TEXT NOT NULL UNIQUE REFERENCES host_grant_operations (operation_id)
);


-- Persist the exact subscription audience independently of mutable preferences.
CREATE TABLE host_grant_space_deletion_audience (
    lifecycle_id TEXT NOT NULL REFERENCES host_grant_space_deletions (lifecycle_id),
    subject_jid TEXT NOT NULL,
    delivery_kind TEXT NOT NULL,
    grant_generation BIGINT NOT NULL CHECK (grant_generation > 0),
    subid TEXT,
    strength TEXT,
    trust_profile TEXT,
    snapshot_generation TEXT,
    PRIMARY KEY (lifecycle_id, subject_jid, delivery_kind),
    CHECK (delivery_kind IN ('local', 'remote')),
    CHECK (
        (delivery_kind = 'local' AND subid IS NOT NULL AND strength IS NULL
            AND trust_profile IS NULL AND snapshot_generation IS NULL)
        OR (delivery_kind = 'remote' AND subid IS NULL AND strength IS NOT NULL
            AND strength IN ('pkix', 'dialback') AND snapshot_generation IS NOT NULL
            AND (trust_profile IS NULL OR trust_profile IN ('public_webpki', 'private_ca')))
    )
);

-- Mutable delivery progress is separate from immutable audience membership.
CREATE TABLE host_grant_space_deletion_jobs (
    lifecycle_id TEXT NOT NULL,
    subject_jid TEXT NOT NULL,
    delivery_kind TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    due_at_ms BIGINT NOT NULL,
    attempt_token UUID,
    lease_until_ms BIGINT,
    PRIMARY KEY (lifecycle_id, subject_jid, delivery_kind),
    FOREIGN KEY (lifecycle_id, subject_jid, delivery_kind)
        REFERENCES host_grant_space_deletion_audience (lifecycle_id, subject_jid, delivery_kind),
    CHECK (state IN ('pending', 'leased', 'staging', 'custody', 'ambiguous', 'refused')),
    CHECK ((state IN ('leased', 'staging')) = (attempt_token IS NOT NULL)),
    CHECK ((attempt_token IS NULL) = (lease_until_ms IS NULL))
);

CREATE INDEX host_grant_space_deletion_jobs_due
    ON host_grant_space_deletion_jobs (delivery_kind, due_at_ms)
    WHERE state = 'pending';
CREATE INDEX host_grant_space_deletion_jobs_expiry
    ON host_grant_space_deletion_jobs (delivery_kind, lease_until_ms)
    WHERE state IN ('leased', 'staging');
