-- Canonical room requests, durable occupancy and frozen responses for the
-- durable ordered mailbox.
--
-- A room request is a MUC join, occupant presence update, leave or relayed
-- IQ that an origin node hands to the room owner through a mailbox lane. It
-- is the parent of its mailbox entry the way a canonical message is the
-- parent of a room acceptance: the entry stores no payload.
--
-- `request_kind`: 1 join, 2 presence update, 3 leave, 4 IQ.
-- `owner_decision`: 0 undecided, 1 accepted, 2 rejected. The repository moves
-- it once, from 0, in the transaction that writes the responses.
-- `settled_at` is set by the origin in the transaction that consumes the
-- entry and hands the responses to the requester.
--
-- The source binding columns hold the authenticated principal reference and,
-- for a stream with stream management, its stream and ordinal: the values a
-- bound canonical message carries.
CREATE TABLE ingress_room_requests (
    request_key UUID PRIMARY KEY,
    request_kind SMALLINT NOT NULL,
    room_jid TEXT NOT NULL,
    occupant_jid TEXT NOT NULL,
    occupancy_generation TEXT NOT NULL,
    source_binding_version SMALLINT NOT NULL,
    source_bare_jid TEXT NOT NULL,
    source_auth_context_id UUID NOT NULL,
    source_auth_context_version NUMERIC(20,0) NOT NULL,
    source_auth_epoch NUMERIC(20,0) NOT NULL,
    source_sm_ingress_id UUID NULL,
    source_ingress_ordinal NUMERIC(20,0) NULL,
    stanza BYTEA NOT NULL,
    owner_decision SMALLINT NOT NULL DEFAULT 0,
    settled_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT ingress_room_requests_kind_check CHECK (request_kind BETWEEN 1 AND 4),
    CONSTRAINT ingress_room_requests_jid_check
        CHECK (room_jid <> '' AND octet_length(room_jid) <= 3071
            AND occupant_jid <> '' AND octet_length(occupant_jid) <= 3071
            AND occupancy_generation <> '' AND octet_length(occupancy_generation) <= 64),
    CONSTRAINT ingress_room_requests_source_binding_check
        CHECK (source_binding_version = 1
            AND source_bare_jid <> '' AND octet_length(source_bare_jid) <= 3071
            AND source_auth_context_version BETWEEN 1 AND 18446744073709551615
            AND source_auth_epoch BETWEEN 1 AND 18446744073709551615
            AND ((source_sm_ingress_id IS NULL AND source_ingress_ordinal IS NULL)
              OR (source_sm_ingress_id IS NOT NULL AND source_ingress_ordinal IS NOT NULL
                AND source_ingress_ordinal BETWEEN 1 AND 18446744073709551615))),
    -- One megabyte is the inbound frame size limit.
    CONSTRAINT ingress_room_requests_stanza_check
        CHECK (octet_length(stanza) BETWEEN 1 AND 1048576),
    CONSTRAINT ingress_room_requests_decision_check CHECK (owner_decision IN (0, 1, 2)),
    CONSTRAINT ingress_room_requests_settled_check
        CHECK (settled_at IS NULL OR owner_decision <> 0)
);
-- The collector reads settled requests in settlement order; the monitoring
-- query reads the unsettled remainder.
CREATE INDEX ingress_room_requests_settled
    ON ingress_room_requests (settled_at)
    WHERE settled_at IS NOT NULL;
CREATE INDEX ingress_room_requests_unsettled
    ON ingress_room_requests (created_at)
    WHERE settled_at IS NULL;

-- The owner's frozen response to one request, in the order the requester
-- must receive it. Rows are written with the decision, so a request has
-- responses only when it is decided.
CREATE TABLE ingress_room_responses (
    request_key UUID NOT NULL REFERENCES ingress_room_requests (request_key) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    stanza BYTEA NOT NULL,
    PRIMARY KEY (request_key, position),
    CONSTRAINT ingress_room_responses_position_check CHECK (position >= 0),
    CONSTRAINT ingress_room_responses_stanza_check
        CHECK (octet_length(stanza) BETWEEN 1 AND 1048576)
);

-- One row per occupying resource of a room, written only by the room owner
-- in the transaction that decides a join, presence update or leave. A node
-- that takes the room's claim seats these rows before it decides any entry
-- for the room.
--
-- `role`: 0 none, 1 visitor, 2 participant, 3 moderator.
-- `affiliation`: 0 outcast, 1 none, 2 member, 3 admin, 4 owner.
-- `presence` is the occupant's last available presence as the room
-- broadcasts it, at most 16 KiB.
-- `room_epoch` is the room claim epoch under which the row was last written.
CREATE TABLE ingress_room_occupants (
    room_jid TEXT NOT NULL,
    occupant_jid TEXT NOT NULL,
    nick TEXT NOT NULL,
    role SMALLINT NOT NULL,
    affiliation SMALLINT NOT NULL,
    occupancy_generation TEXT NOT NULL,
    join_request UUID NULL REFERENCES ingress_room_requests (request_key) ON DELETE SET NULL,
    presence BYTEA NULL,
    room_epoch BIGINT NOT NULL,
    PRIMARY KEY (room_jid, occupant_jid),
    CONSTRAINT ingress_room_occupants_jid_check
        CHECK (room_jid <> '' AND octet_length(room_jid) <= 3071
            AND occupant_jid <> '' AND octet_length(occupant_jid) <= 3071
            AND nick <> '' AND octet_length(nick) <= 1023
            AND occupancy_generation <> '' AND octet_length(occupancy_generation) <= 64),
    CONSTRAINT ingress_room_occupants_role_check
        CHECK (role BETWEEN 0 AND 3 AND affiliation BETWEEN 0 AND 4),
    CONSTRAINT ingress_room_occupants_presence_check
        CHECK (presence IS NULL OR octet_length(presence) BETWEEN 1 AND 16384),
    CONSTRAINT ingress_room_occupants_epoch_check CHECK (room_epoch >= 0)
);
CREATE INDEX ingress_room_occupants_join_request
    ON ingress_room_occupants (join_request)
    WHERE join_request IS NOT NULL;

-- A mailbox entry refers to exactly one parent: a canonical message for a
-- room acceptance (operation kind 1), a room request for a join (2), a
-- presence update (3), a leave (4) or an IQ (5). Each parent has at most one
-- entry across every lane.
ALTER TABLE ingress_mailbox_entries
    ALTER COLUMN message_key DROP NOT NULL,
    ADD COLUMN request_key UUID NULL
        REFERENCES ingress_room_requests (request_key) ON DELETE CASCADE,
    DROP CONSTRAINT ingress_mailbox_entries_operation_key,
    DROP CONSTRAINT ingress_mailbox_entries_operation_check,
    ADD CONSTRAINT ingress_mailbox_entries_operation_check
        CHECK (operation_kind BETWEEN 1 AND 5),
    ADD CONSTRAINT ingress_mailbox_entries_parent_check
        CHECK ((operation_kind = 1 AND message_key IS NOT NULL AND request_key IS NULL)
            OR (operation_kind BETWEEN 2 AND 5
                AND message_key IS NULL AND request_key IS NOT NULL));
CREATE UNIQUE INDEX ingress_mailbox_entries_message_operation
    ON ingress_mailbox_entries (message_key)
    WHERE message_key IS NOT NULL;
CREATE UNIQUE INDEX ingress_mailbox_entries_request_operation
    ON ingress_mailbox_entries (request_key)
    WHERE request_key IS NOT NULL;

CREATE TRIGGER ingress_room_requests_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_room_requests
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_room_requests_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_room_requests
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_room_requests ENABLE ALWAYS TRIGGER ingress_room_requests_epoch_guard_dml;
ALTER TABLE ingress_room_requests ENABLE ALWAYS TRIGGER ingress_room_requests_epoch_guard_truncate;
CREATE TRIGGER ingress_room_responses_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_room_responses
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_room_responses_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_room_responses
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_room_responses ENABLE ALWAYS TRIGGER ingress_room_responses_epoch_guard_dml;
ALTER TABLE ingress_room_responses ENABLE ALWAYS TRIGGER ingress_room_responses_epoch_guard_truncate;
CREATE TRIGGER ingress_room_occupants_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_room_occupants
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_room_occupants_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_room_occupants
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_room_occupants ENABLE ALWAYS TRIGGER ingress_room_occupants_epoch_guard_dml;
ALTER TABLE ingress_room_occupants ENABLE ALWAYS TRIGGER ingress_room_occupants_epoch_guard_truncate;
INSERT INTO ingress_epoch_guard_manifest (table_name)
VALUES ('ingress_room_requests'), ('ingress_room_responses'), ('ingress_room_occupants');
GRANT SELECT ON TABLE ingress_room_requests, ingress_room_responses, ingress_room_occupants
    TO pg_monitor;
