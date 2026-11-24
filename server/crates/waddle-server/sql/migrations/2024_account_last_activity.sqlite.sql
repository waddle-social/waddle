-- Last logout of an account (XEP-0012 "Offline User Query").
--
-- One row per account records when the account last became unavailable
-- and the status text of that unavailable presence. A replica upserts the
-- row when the account sends unavailable presence or its last session
-- ends; any replica answers a last activity query from it, so the answer
-- is the same on every node and survives a restart. An account that never
-- logged out has no row. Retiring the account deletes its row.
--
-- `account_jid` is the account's bare JID; `logged_out_at_ms` is
-- milliseconds since the Unix epoch on the clock of the writing process.
CREATE TABLE account_last_activity (
    account_jid TEXT PRIMARY KEY,
    logged_out_at_ms INTEGER NOT NULL,
    status TEXT,
    CONSTRAINT account_last_activity_account_jid CHECK (length(CAST(account_jid AS BLOB)) BETWEEN 3 AND 2047)
);
