-- Durable ordered mailbox: one row per logical lane and one row per entry.
--
-- A lane is identified by the canonical encoding of its logical identity
-- (origin, recipient, room lane). The digest indexes that encoding because a
-- maximal identity exceeds the B-tree row limit; every resolution compares the
-- full encoding, so two identities sharing a digest cannot merge. Claim epochs
-- and the target node are fence columns beside the identity: a successor owner
-- resumes the same lane and sequence space.
--
-- `last_sequence` is the gap-free allocation counter and `last_dispositioned`
-- the exclusive consumption cursor. Both hold the whole unsigned 64-bit range,
-- which BIGINT cannot.
CREATE TABLE ingress_mailbox_lanes (
    lane_id UUID PRIMARY KEY,
    digest_version SMALLINT NOT NULL,
    digest BYTEA NOT NULL,
    encoding_version SMALLINT NOT NULL,
    encoding BYTEA NOT NULL,
    last_sequence NUMERIC(20,0) NOT NULL DEFAULT 0,
    last_dispositioned NUMERIC(20,0) NOT NULL DEFAULT 0,
    origin_epoch BIGINT NOT NULL,
    target_epoch BIGINT NOT NULL,
    target_node_id TEXT NOT NULL,
    target_node_epoch TEXT NOT NULL,
    CONSTRAINT ingress_mailbox_lanes_digest_key UNIQUE (digest_version, digest),
    CONSTRAINT ingress_mailbox_lanes_digest_check
        CHECK (digest_version = 1 AND octet_length(digest) = 32),
    CONSTRAINT ingress_mailbox_lanes_encoding_check
        CHECK (encoding_version = 1 AND octet_length(encoding) BETWEEN 3 AND 8192
            AND get_byte(encoding, 0) = 1),
    CONSTRAINT ingress_mailbox_lanes_sequence_check
        CHECK (last_sequence BETWEEN 0 AND 18446744073709551615
            AND last_dispositioned BETWEEN 0 AND last_sequence),
    CONSTRAINT ingress_mailbox_lanes_fence_check
        CHECK (origin_epoch >= 0 AND target_epoch >= 0
            AND target_node_id <> '' AND octet_length(target_node_id) <= 1024
            AND target_node_epoch <> '' AND octet_length(target_node_epoch) <= 1024)
);
-- Target-first polling reads only lanes that still owe a disposition.
CREATE INDEX ingress_mailbox_lanes_backlog
    ON ingress_mailbox_lanes (target_node_id, lane_id)
    WHERE last_dispositioned < last_sequence;

-- An entry refers to its canonical ingress message and stores no payload.
-- The operation identity is unique across every lane, so a retry planned for
-- another owner or arriving on another lane finds the first responsibility.
-- The parent cascade removes an entry only together with its canonical
-- message; retention keeps a parent whose entry is not terminal.
--
-- `disposition`: 0 pending, 1 owner accepted, 2 owner rejected, 3 consumed
-- after acceptance, 4 consumed after rejection. A dead-lettered entry stays
-- pending and keeps its place at the lane head until an operator repairs it.
--
-- `request_language` is the stanza's top-level `xml:lang`. The canonical
-- message does not retain it, and the semantic digest that binds the request
-- to its canonical message is computed with it, so a target that works from
-- the entry alone needs it to prove the same request.
CREATE TABLE ingress_mailbox_entries (
    lane_id UUID NOT NULL REFERENCES ingress_mailbox_lanes (lane_id) ON DELETE RESTRICT,
    sequence NUMERIC(20,0) NOT NULL,
    operation_kind SMALLINT NOT NULL,
    message_key UUID NOT NULL REFERENCES ingress_messages (message_key) ON DELETE CASCADE,
    disposition SMALLINT NOT NULL DEFAULT 0,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NULL,
    dead_lettered_at TIMESTAMPTZ NULL,
    dead_letter_reason SMALLINT NULL,
    request_language TEXT NULL,
    appended_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (lane_id, sequence),
    CONSTRAINT ingress_mailbox_entries_operation_key UNIQUE (message_key, operation_kind),
    CONSTRAINT ingress_mailbox_entries_sequence_check
        CHECK (sequence BETWEEN 1 AND 18446744073709551615),
    CONSTRAINT ingress_mailbox_entries_operation_check CHECK (operation_kind = 1),
    CONSTRAINT ingress_mailbox_entries_disposition_check CHECK (disposition BETWEEN 0 AND 4),
    CONSTRAINT ingress_mailbox_entries_attempt_check CHECK (attempt_count >= 0),
    CONSTRAINT ingress_mailbox_entries_language_check
        CHECK (request_language IS NULL
            OR (request_language <> '' AND octet_length(request_language) <= 64)),
    CONSTRAINT ingress_mailbox_entries_dead_letter_check
        CHECK ((dead_lettered_at IS NULL AND dead_letter_reason IS NULL)
            OR (dead_lettered_at IS NOT NULL AND dead_letter_reason IS NOT NULL
                AND dead_letter_reason BETWEEN 1 AND 3 AND disposition = 0))
);
CREATE INDEX ingress_mailbox_entries_nonterminal
    ON ingress_mailbox_entries (message_key)
    WHERE disposition < 3;

CREATE TRIGGER ingress_mailbox_lanes_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_mailbox_lanes
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_mailbox_lanes_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_mailbox_lanes
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_mailbox_lanes ENABLE ALWAYS TRIGGER ingress_mailbox_lanes_epoch_guard_dml;
ALTER TABLE ingress_mailbox_lanes ENABLE ALWAYS TRIGGER ingress_mailbox_lanes_epoch_guard_truncate;
CREATE TRIGGER ingress_mailbox_entries_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_mailbox_entries
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_mailbox_entries_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_mailbox_entries
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_mailbox_entries ENABLE ALWAYS TRIGGER ingress_mailbox_entries_epoch_guard_dml;
ALTER TABLE ingress_mailbox_entries ENABLE ALWAYS TRIGGER ingress_mailbox_entries_epoch_guard_truncate;
INSERT INTO ingress_epoch_guard_manifest (table_name)
VALUES ('ingress_mailbox_lanes'), ('ingress_mailbox_entries');
GRANT SELECT ON TABLE ingress_mailbox_lanes, ingress_mailbox_entries TO pg_monitor;
