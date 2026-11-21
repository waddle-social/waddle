-- Local preferences carry no S2S stream evidence or remote quota state.
-- A scope holds at most one preference per canonical grant subject.
CREATE TABLE local_space_subscriptions (
    lifecycle_id TEXT NOT NULL,
    subject_jid TEXT NOT NULL,
    authority_incarnation TEXT NOT NULL CHECK (authority_incarnation ~ '^[0-9a-f]{32}$'),
    grant_generation BIGINT NOT NULL CHECK (grant_generation > 0),
    subid TEXT NOT NULL CHECK (
        subid ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    ),
    PRIMARY KEY (lifecycle_id, subject_jid),
    FOREIGN KEY (lifecycle_id, subject_jid)
        REFERENCES host_grant_space_affiliations (lifecycle_id, subject_jid) ON DELETE CASCADE
);
