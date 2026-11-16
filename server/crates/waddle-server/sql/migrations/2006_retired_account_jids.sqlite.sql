-- Retired account JIDs (ADR-0020 D8): a home never issues the JID of a
-- retired account to a new holder.
--
-- Retiring an account records its JID here in the transaction that removes
-- the account. Every path that provisions an account or binds a JID to one
-- (native registration, provisioning at an identity-provider login, session
-- creation) reads this ledger in the statement or transaction that would
-- create the row, and creates nothing for a JID it holds. A row is never
-- updated or deleted.
--
-- `jid_key` is the account's bare JID in the identity profile's canonical
-- form, the key `native_users.jid_key` holds; `localpart_key` is its
-- canonical localpart, the key `users.localpart_key` holds.
CREATE TABLE retired_account_jids (
    jid_key TEXT PRIMARY KEY,
    localpart_key TEXT NOT NULL,
    retired_at_ms INTEGER NOT NULL,
    CONSTRAINT retired_account_jids_jid_key CHECK (length(CAST(jid_key AS BLOB)) BETWEEN 3 AND 2047),
    CONSTRAINT retired_account_jids_localpart_key CHECK (length(CAST(localpart_key AS BLOB)) BETWEEN 1 AND 1023)
);

CREATE INDEX idx_retired_account_jids_localpart_key ON retired_account_jids (localpart_key);
