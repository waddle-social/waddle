-- XMPP OAuth authorization grants (XEP-0493, XEP-0494).
--
-- One row per authorization an account owner approved for a client. The
-- bearer credential the client received is the session `session_id`; the
-- row records what the owner may later see and revoke: the grant's opaque
-- identifier (the XEP-0494 client id, which is not the credential), its
-- scope, and when it was made. Its last use is the session's
-- `last_used_at`. A grant whose session is gone is not listed and grants
-- nothing; revocation deletes the grant, and the session janitor deletes
-- the rows whose session expired or was deleted.
--
-- `session_id` is deliberately not a foreign key: a constraint referencing
-- `sessions` adds a trigger to that table, and the pinned restricted
-- ingress profile fixes the trigger set of `sessions`.
--
-- `created_at_ms` is milliseconds since the Unix epoch on the clock of the
-- writing process; `scope` is the space-delimited RFC 6749 scope value.
CREATE TABLE xmpp_oauth_grants (
    grant_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE,
    user_jid TEXT NOT NULL,
    scope TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    CONSTRAINT xmpp_oauth_grants_grant_id CHECK (octet_length(grant_id) BETWEEN 1 AND 128)
);

CREATE INDEX idx_xmpp_oauth_grants_user_jid ON xmpp_oauth_grants (user_jid);
