-- Account traffic between remote senders and local accounts (ADR-0020 D7;
-- gate G18). See server/docs/planning/s2s-account-traffic-2047.md.
--
-- `s2s_pending_subscriptions` records, per local account and remote bare
-- JID, the last presence subscription request the remote sender sent that
-- the account has neither approved nor denied (RFC 6121 §3.1.3), as the
-- serialized stanza, so that it is delivered again on the first available
-- presence of each new session. `contact_domain` serves the bound per
-- remote domain. The addresses are bounded in bytes so that the primary
-- key's index entry stays below PostgreSQL's B-tree limit.
--
-- `s2s_remote_pubsub_subscriptions` records the XEP-0060 nodes of remote
-- services a local account subscribed to: written when the account's
-- `<subscribe/>` leaves, removed when its `<unsubscribe/>` leaves. Event
-- notifications from a remote service reach the account only for a node
-- recorded here, or from the personal eventing service of a contact whose
-- presence the account is subscribed to.
CREATE TABLE s2s_pending_subscriptions (
    account_jid TEXT NOT NULL,
    contact_jid TEXT NOT NULL,
    contact_domain TEXT NOT NULL,
    stanza TEXT NOT NULL,
    received_at_ms BIGINT NOT NULL,
    PRIMARY KEY (account_jid, contact_jid),
    CONSTRAINT s2s_pending_subscriptions_account_jid CHECK (octet_length(account_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_pending_subscriptions_contact_jid CHECK (octet_length(contact_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_pending_subscriptions_stanza CHECK (octet_length(stanza) BETWEEN 1 AND 16384)
);

CREATE INDEX s2s_pending_subscriptions_domain ON s2s_pending_subscriptions (account_jid, contact_domain);

CREATE TABLE s2s_remote_pubsub_subscriptions (
    account_jid TEXT NOT NULL,
    service_jid TEXT NOT NULL,
    node TEXT NOT NULL,
    subscribed_at_ms BIGINT NOT NULL,
    PRIMARY KEY (account_jid, service_jid, node),
    CONSTRAINT s2s_remote_pubsub_subscriptions_account_jid CHECK (octet_length(account_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_remote_pubsub_subscriptions_service_jid CHECK (octet_length(service_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_remote_pubsub_subscriptions_node CHECK (octet_length(node) BETWEEN 1 AND 256)
);
