-- The PostgreSQL baseline already creates the version 8 PubSub relations.
-- Record that version for a baseline database whose PubSub owner phase has
-- not run yet. That initializer must preserve the node relation now that
-- canonical Space bindings can refer to it.
INSERT INTO pubsub_schema_version (version)
SELECT 8 WHERE NOT EXISTS (SELECT 1 FROM pubsub_schema_version);

-- A private PubSub node bound to the Space key in the scope lifecycle ledger.
-- Retain the exact bound node: PostgreSQL refuses deletion while a binding
-- exists. Existing room affiliations imply no standing in the Space.
ALTER TABLE host_grant_scope_lifecycles
    ADD CONSTRAINT host_grant_scope_lifecycles_address
    UNIQUE (lifecycle_id, service_jid, space_node);
CREATE TABLE host_grant_space_bindings (
    lifecycle_id TEXT PRIMARY KEY,
    service_jid TEXT NOT NULL,
    space_node TEXT NOT NULL,
    access_model TEXT NOT NULL,
    federated BOOLEAN NOT NULL,
    authority_incarnation TEXT,
    CONSTRAINT host_grant_space_bindings_incarnation CHECK (
        authority_incarnation IS NULL OR authority_incarnation ~ '^[0-9a-f]{32}$'
    ),
    CONSTRAINT host_grant_space_bindings_scope
        FOREIGN KEY (lifecycle_id, service_jid, space_node)
        REFERENCES host_grant_scope_lifecycles (lifecycle_id, service_jid, space_node),
    CONSTRAINT host_grant_space_bindings_node
        FOREIGN KEY (service_jid, space_node)
        REFERENCES pubsub_nodes (owner_jid, node_name) ON DELETE RESTRICT,
    CONSTRAINT host_grant_space_bindings_access_model CHECK (access_model = 'whitelist')
);

-- Explicit XEP-0060 affiliation of a canonical bare account JID. A revoke is
-- recorded as none. Service and node come from the bound scope lifecycle.
CREATE TABLE host_grant_space_affiliations (
    lifecycle_id TEXT NOT NULL REFERENCES host_grant_space_bindings (lifecycle_id),
    subject_jid TEXT NOT NULL,
    affiliation TEXT NOT NULL,
    effective_affiliation TEXT NOT NULL,
    issuance_basis TEXT NOT NULL,
    changed_generation BIGINT NOT NULL,
    PRIMARY KEY (lifecycle_id, subject_jid),
    CONSTRAINT host_grant_space_affiliations_affiliation CHECK (
        affiliation IN ('owner', 'publisher', 'publish-only', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grant_space_affiliations_effective CHECK (
        effective_affiliation IN ('owner', 'publisher', 'publish-only', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grant_space_affiliations_issuance CHECK (
        issuance_basis IN ('local', 'trusted_home', 'unprivileged')
    ),
    CONSTRAINT host_grant_space_affiliations_operation
        FOREIGN KEY (lifecycle_id, changed_generation)
        REFERENCES host_grant_operations (lifecycle_id, generation)
);

-- Space mutations participate in the same journal and lifecycle generation
-- as room mutations. Their affiliation changes carry no room address.
ALTER TABLE host_grant_operations
    DROP CONSTRAINT host_grant_operations_kind,
    ADD CONSTRAINT host_grant_operations_kind CHECK (
        kind IN (
            'enroll', 'set_affiliation', 'retire', 'exhaust', 'restore', 'reconstruct',
            'fence', 'reopen', 'bind_space', 'set_space_affiliations'
        )
    );

CREATE TABLE host_grant_space_operation_changes (
    operation_id TEXT NOT NULL REFERENCES host_grant_operations (operation_id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    subject_jid TEXT NOT NULL,
    affiliation TEXT NOT NULL,
    previous_affiliation TEXT NOT NULL,
    issuance_basis TEXT NOT NULL,
    PRIMARY KEY (operation_id, ordinal),
    UNIQUE (operation_id, subject_jid),
    CONSTRAINT host_grant_space_operation_changes_affiliation CHECK (
        affiliation IN ('owner', 'publisher', 'publish-only', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grant_space_operation_changes_previous_affiliation CHECK (
        previous_affiliation IN ('owner', 'publisher', 'publish-only', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grant_space_operation_changes_issuance CHECK (
        issuance_basis IN ('local', 'trusted_home', 'unprivileged')
    )
);
