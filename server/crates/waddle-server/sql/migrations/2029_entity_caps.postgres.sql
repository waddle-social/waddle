-- Verified XEP-0115 entity capabilities, shared by every replica.
--
-- A resource's `<c hash node ver/>` names a capability set; the replica that
-- holds its socket asks it for disco#info and, when the recomputed
-- verification string equals `ver` (XEP-0115 section 5.4), records the
-- answer here. XEP-0163 notification filtering runs on whichever replica
-- processed the publish, which need not hold the contact's socket, so the
-- answer must be readable from any replica.
--
-- Entries are content-addressed by `(hash, ver)` and only written after
-- verification, so replicas can never disagree and a write is idempotent.
-- `info` is the disco#info `<query/>` element (identities and features),
-- serialized and bounded; the writer also bounds the number of rows and
-- prunes the oldest by `verified_at_ms`.
CREATE TABLE entity_caps (
    hash TEXT NOT NULL,
    ver TEXT NOT NULL,
    info TEXT NOT NULL,
    verified_at_ms BIGINT NOT NULL,
    PRIMARY KEY (hash, ver),
    CONSTRAINT entity_caps_hash CHECK (octet_length(hash) BETWEEN 1 AND 32),
    CONSTRAINT entity_caps_ver CHECK (octet_length(ver) BETWEEN 1 AND 256),
    CONSTRAINT entity_caps_info CHECK (octet_length(info) BETWEEN 1 AND 65536)
);

CREATE INDEX idx_entity_caps_verified_at ON entity_caps (verified_at_ms);
