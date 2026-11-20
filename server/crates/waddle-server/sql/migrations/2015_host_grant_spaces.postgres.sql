-- A private PubSub node bound to the Space key in the scope lifecycle ledger.
-- The node creation stamp distinguishes a node from a later node with the
-- same address. Bindings and affiliations are created only by the canonical
-- authority. Existing room affiliations imply no standing in the Space.
CREATE TABLE host_grant_space_bindings (
    lifecycle_id TEXT PRIMARY KEY REFERENCES host_grant_scope_lifecycles (lifecycle_id),
    node_created_at_ms BIGINT NOT NULL,
    access_model TEXT NOT NULL,
    federated BOOLEAN NOT NULL,
    CONSTRAINT host_grant_space_bindings_access_model CHECK (access_model = 'whitelist')
);

-- Explicit XEP-0060 affiliation of a canonical bare account JID. A revoke is
-- recorded as none. Service and node come from the bound scope lifecycle.
CREATE TABLE host_grant_space_affiliations (
    lifecycle_id TEXT NOT NULL REFERENCES host_grant_space_bindings (lifecycle_id),
    subject_jid TEXT NOT NULL,
    affiliation TEXT NOT NULL,
    changed_generation BIGINT NOT NULL,
    PRIMARY KEY (lifecycle_id, subject_jid),
    CONSTRAINT host_grant_space_affiliations_affiliation CHECK (
        affiliation IN ('owner', 'publisher', 'publish-only', 'member', 'none', 'outcast')
    ),
    CONSTRAINT host_grant_space_affiliations_generation CHECK (changed_generation > 0)
);
