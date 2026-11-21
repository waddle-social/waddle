-- Scope locks serialize bookmark writes with preference creation. CACHE 1
-- prevents a backend from reserving a lower value for a later transaction.
CREATE SEQUENCE local_space_notification_order AS BIGINT CACHE 1 NO CYCLE;

-- Preexisting rows lack a provable relative order. A new bookmark change
-- stamps a version; an explicit subscriber renewal initializes its era.
ALTER TABLE local_space_subscriptions ADD COLUMN birth_order BIGINT CHECK (birth_order > 0);
ALTER TABLE host_grant_space_bookmark_versions
    ADD COLUMN publication_order BIGINT CHECK (publication_order > 0),
    ADD COLUMN published_at TIMESTAMPTZ,
    ADD COLUMN deliver_payloads BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN notify_retract BOOLEAN NOT NULL DEFAULT false,
    ADD CONSTRAINT host_grant_space_bookmark_notification_order CHECK (
        (publication_order IS NULL) = (published_at IS NULL)
    );
