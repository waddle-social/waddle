-- Pending IQ requests across server-to-server streams (ADR-0020, transport;
-- gate G8). A request is registered before it is written to a stream and
-- removed by one DELETE on its key: by the response that completes it, on
-- whichever replica that response arrives, by the request's own failure
-- before it was written, or by its expiry. Only the caller whose DELETE
-- removed the row acts on it, so a request completes at most once.
-- See server/docs/planning/s2s-routing-2047.md.
CREATE TABLE s2s_iq_correlations (
    direction TEXT NOT NULL,
    local_jid TEXT NOT NULL,
    remote_jid TEXT NOT NULL,
    stanza_id TEXT NOT NULL,
    request_type TEXT NOT NULL,
    payload_ns TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    PRIMARY KEY (direction, local_jid, remote_jid, stanza_id),
    CONSTRAINT s2s_iq_correlations_direction CHECK (direction IN ('outbound', 'inbound')),
    CONSTRAINT s2s_iq_correlations_request_type CHECK (request_type IN ('get', 'set')),
    CONSTRAINT s2s_iq_correlations_stanza_id CHECK (length(stanza_id) BETWEEN 1 AND 256)
);

CREATE INDEX s2s_iq_correlations_expiry ON s2s_iq_correlations (expires_at_ms);
