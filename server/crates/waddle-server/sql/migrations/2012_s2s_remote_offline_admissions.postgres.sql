-- Offline storage of messages from remote senders (ADR-0020 D7; gate G18).
-- See server/docs/planning/s2s-account-traffic-2047.md.
--
-- A `chat` or `normal` message from a remote sender that no resource of a
-- local account takes is stored in the account's offline store, from a
-- contact and a stranger alike, and nothing answers the sender. One row
-- here records one such message from a sender that is not the account's
-- contact; the rows of the last day bound how many more each sender, each
-- sender domain (`sender_share`, the domain of the sender's JID, an IP
-- literal as written) and all strangers together may have stored for the
-- account. Rows older than the window are deleted by the account's next
-- admission.
--
-- The stored message itself is an ordinary row of the offline store;
-- nothing here refers to it. In `s2s_pending_subscriptions` (migration
-- 2010) `contact_domain` holds the same domain key.
CREATE TABLE s2s_remote_offline_admissions (
    id TEXT NOT NULL PRIMARY KEY,
    account_jid TEXT NOT NULL,
    sender_jid TEXT NOT NULL,
    sender_share TEXT NOT NULL,
    admitted_at_ms BIGINT NOT NULL,
    CONSTRAINT s2s_remote_offline_admissions_id CHECK (octet_length(id) BETWEEN 1 AND 64),
    CONSTRAINT s2s_remote_offline_admissions_account_jid CHECK (octet_length(account_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_remote_offline_admissions_sender_jid CHECK (octet_length(sender_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_remote_offline_admissions_sender_share CHECK (octet_length(sender_share) BETWEEN 1 AND 1023)
);

CREATE INDEX s2s_remote_offline_admissions_sender ON s2s_remote_offline_admissions (account_jid, sender_jid);

CREATE INDEX s2s_remote_offline_admissions_share ON s2s_remote_offline_admissions (account_jid, sender_share);

CREATE INDEX s2s_remote_offline_admissions_window ON s2s_remote_offline_admissions (account_jid, admitted_at_ms);
