-- Grant-backed Space preferences are PostgreSQL-only.
ALTER TABLE s2s_space_subscriptions ADD COLUMN grant_lifecycle_id TEXT
    CHECK (grant_lifecycle_id IS NULL);
ALTER TABLE s2s_space_subscriptions ADD COLUMN grant_authority_incarnation TEXT
    CHECK (grant_authority_incarnation IS NULL);
ALTER TABLE s2s_space_subscriptions ADD COLUMN grant_generation BIGINT
    CHECK (grant_generation IS NULL);
