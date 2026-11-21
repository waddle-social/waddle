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
