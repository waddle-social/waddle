-- A protected preference is valid only for the authority and exact
-- affiliation generation under which the subscriber registered it.
-- An unqualified legacy preference carries none of these fields.
ALTER TABLE s2s_space_subscriptions
    ADD COLUMN grant_lifecycle_id TEXT,
    ADD COLUMN grant_authority_incarnation TEXT,
    ADD COLUMN grant_generation BIGINT,
    ADD CONSTRAINT s2s_space_subscriptions_grant CHECK (
        (grant_lifecycle_id IS NULL AND grant_authority_incarnation IS NULL AND grant_generation IS NULL)
        OR (
            grant_lifecycle_id IS NOT NULL AND grant_lifecycle_id ~ '^[0-9a-f]{32}$'
            AND grant_authority_incarnation IS NOT NULL AND grant_authority_incarnation ~ '^[0-9a-f]{32}$'
            AND grant_generation IS NOT NULL AND grant_generation > 0
        )
    );
