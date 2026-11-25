-- XMPP OAuth authorization grants (XEP-0493, XEP-0494).
--
-- One row per authorization an account owner approved for a client. The
-- bearer credential the client received is the session `session_id`; the
-- row records what the owner may later see and revoke: the grant's opaque
-- identifier (the XEP-0494 client id, which is not the credential), its
-- scope, and when it was made. Its last use is the session's
-- `last_used_at`. Deleting the session, by revocation, expiry or the
-- account's retirement, deletes the grant with it.
--
-- `created_at_ms` is milliseconds since the Unix epoch on the clock of the
-- writing process; `scope` is the space-delimited RFC 6749 scope value.
CREATE TABLE xmpp_oauth_grants (
    grant_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions (id) ON DELETE CASCADE,
    user_jid TEXT NOT NULL,
    scope TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    CONSTRAINT xmpp_oauth_grants_grant_id CHECK (length(CAST(grant_id AS BLOB)) BETWEEN 1 AND 128)
);

CREATE INDEX idx_xmpp_oauth_grants_user_jid ON xmpp_oauth_grants (user_jid);
