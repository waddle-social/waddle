-- Direct requests for the durable ordered mailbox: a stanza that an origin
-- node hands to the node owning the recipient's account, addressed to a bare
-- or a full JID.
--
-- A direct request is the parent of its mailbox entry the way a room request
-- is: the entry stores no payload. The request stores what the ordered relay
-- envelope would have carried.
--
-- `sender_entity` is the claim entity the origin presented as the sender.
-- `payload` is the relay payload in the relay's codec.
-- `message_key` is set for a copy that an ingress transaction owns, so the
-- request leaves with its canonical message.
-- `owner_decision`: 0 undecided, 1 accepted, 2 rejected. The repository moves
-- it once, from 0.
-- `refusal`: why a rejected request was rejected: 1 nothing took the stanza,
-- 2 the recipient's session is hosted by another node.
-- `settled_at` is set by the origin in the transaction that consumes the
-- entry.
CREATE TABLE ingress_direct_requests (
    request_key UUID PRIMARY KEY,
    recipient_jid TEXT NOT NULL,
    sender_entity TEXT NOT NULL,
    payload BYTEA NOT NULL,
    message_key UUID NULL REFERENCES ingress_messages (message_key) ON DELETE CASCADE,
    owner_decision SMALLINT NOT NULL DEFAULT 0,
    refusal SMALLINT NULL,
    settled_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT ingress_direct_requests_jid_check
        CHECK (recipient_jid <> '' AND octet_length(recipient_jid) <= 3071
            AND sender_entity <> '' AND octet_length(sender_entity) <= 3200),
    -- The inbound frame size limit is one megabyte; the payload adds the
    -- relay's field names, the recipient and an append obligation.
    CONSTRAINT ingress_direct_requests_payload_check
        CHECK (octet_length(payload) BETWEEN 1 AND 1114112),
    CONSTRAINT ingress_direct_requests_decision_check CHECK (owner_decision IN (0, 1, 2)),
    CONSTRAINT ingress_direct_requests_refusal_check
        CHECK ((owner_decision = 2 AND refusal IN (1, 2))
            OR (owner_decision <> 2 AND refusal IS NULL)),
    CONSTRAINT ingress_direct_requests_settled_check
        CHECK (settled_at IS NULL OR owner_decision <> 0)
);
-- The collector reads settled requests in settlement order; the monitoring
-- query reads the unsettled remainder.
CREATE INDEX ingress_direct_requests_settled
    ON ingress_direct_requests (settled_at)
    WHERE settled_at IS NOT NULL;
CREATE INDEX ingress_direct_requests_unsettled
    ON ingress_direct_requests (created_at)
    WHERE settled_at IS NULL;
CREATE INDEX ingress_direct_requests_message
    ON ingress_direct_requests (message_key)
    WHERE message_key IS NOT NULL;

-- A mailbox entry refers to exactly one parent: a canonical message for a
-- room acceptance (operation kind 1), a room request for a join (2), a
-- presence update (3), a leave (4) or an IQ (5), or a direct request (6).
-- Each parent has at most one entry across every lane.
ALTER TABLE ingress_mailbox_entries
    ADD COLUMN direct_key UUID NULL
        REFERENCES ingress_direct_requests (request_key) ON DELETE CASCADE,
    DROP CONSTRAINT ingress_mailbox_entries_operation_check,
    DROP CONSTRAINT ingress_mailbox_entries_parent_check,
    ADD CONSTRAINT ingress_mailbox_entries_operation_check
        CHECK (operation_kind BETWEEN 1 AND 6),
    ADD CONSTRAINT ingress_mailbox_entries_parent_check
        CHECK ((operation_kind = 1 AND message_key IS NOT NULL
                AND request_key IS NULL AND direct_key IS NULL)
            OR (operation_kind BETWEEN 2 AND 5 AND message_key IS NULL
                AND request_key IS NOT NULL AND direct_key IS NULL)
            OR (operation_kind = 6 AND message_key IS NULL
                AND request_key IS NULL AND direct_key IS NOT NULL));
CREATE UNIQUE INDEX ingress_mailbox_entries_direct_operation
    ON ingress_mailbox_entries (direct_key)
    WHERE direct_key IS NOT NULL;

CREATE TRIGGER ingress_direct_requests_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_direct_requests
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_direct_requests_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_direct_requests
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_direct_requests ENABLE ALWAYS TRIGGER ingress_direct_requests_epoch_guard_dml;
ALTER TABLE ingress_direct_requests ENABLE ALWAYS TRIGGER ingress_direct_requests_epoch_guard_truncate;
INSERT INTO ingress_epoch_guard_manifest (table_name)
VALUES ('ingress_direct_requests');
GRANT SELECT ON TABLE ingress_direct_requests TO pg_monitor;
