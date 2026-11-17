-- Pending IQ requests across server-to-server streams (ADR-0020, transport;
-- gate G8). A request is registered before it is written to a stream and
-- removed by one DELETE on its key: by the response that completes it, on
-- whichever replica that response arrives, by the request's own failure
-- before it was written, or by its expiry. Only the caller whose DELETE
-- removed the row acts on it, so a request completes at most once.
--
-- `owner_id` names the replica that registered the request. That replica
-- reads its own rows to learn which requests another replica completed,
-- and records that it is alive in `s2s_iq_correlation_owners`; expired
-- rows are pruned only once their owner has stopped recording.
--
-- The key's lengths are bounded in bytes so that the primary key's index
-- entry stays below PostgreSQL's B-tree limit, 2704 bytes with the default
-- 8 kB page: a direction of at most 8 bytes, two addresses of at most 1023
-- bytes and an id of at most 256 bytes, with their headers, stay under
-- 2400.
-- See server/docs/planning/s2s-routing-2047.md.
CREATE TABLE s2s_iq_correlations (
    direction TEXT NOT NULL,
    local_jid TEXT NOT NULL,
    remote_jid TEXT NOT NULL,
    stanza_id TEXT NOT NULL,
    request_type TEXT NOT NULL,
    payload_ns TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    PRIMARY KEY (direction, local_jid, remote_jid, stanza_id),
    CONSTRAINT s2s_iq_correlations_direction CHECK (direction IN ('outbound', 'inbound')),
    CONSTRAINT s2s_iq_correlations_request_type CHECK (request_type IN ('get', 'set')),
    CONSTRAINT s2s_iq_correlations_local_jid CHECK (octet_length(local_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_iq_correlations_remote_jid CHECK (octet_length(remote_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_iq_correlations_stanza_id CHECK (octet_length(stanza_id) BETWEEN 1 AND 256)
);

CREATE INDEX s2s_iq_correlations_expiry ON s2s_iq_correlations (expires_at_ms);
CREATE INDEX s2s_iq_correlations_owner ON s2s_iq_correlations (owner_id);

-- When each replica that registers requests last recorded that it is
-- alive. A row is replaced on every expiry sweep and deleted once the
-- replica has stopped long enough for its requests to be pruned.
CREATE TABLE s2s_iq_correlation_owners (
    owner_id TEXT PRIMARY KEY,
    seen_at_ms BIGINT NOT NULL
);
