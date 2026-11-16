-- The subscriptions accounts at other servers hold to local Spaces (ADR-0020
-- D10; XEP-0060 §6.1), one row per Space node and subscribed JID, beside
-- the PubSub service's own subscription record.
--
-- A row is written only when the remote account itself subscribed over an
-- inbound S2S stream, and an event notification leaves for a JID at another
-- server only while such a row exists: a subscription an owner recorded for
-- a remote JID sends nothing. The row records what the stream that carried
-- the request proved (`strength`, and `trust_profile` when the remote
-- domain presented a certificate) and the S2S configuration snapshot it was
-- admitted under. A notification names the subscriber without a request of
-- its own and is decided on that evidence. Peer admission and trusted-home
-- status are not stored: they are evaluated at each decision.
--
-- `node_created_at_ms` binds the row to the PubSub node it was made for, as
-- in `s2s_federated_spaces`: a row left by a deleted Space counts for
-- nothing. `account_jid` and `subscriber_domain` serve the bounds per
-- account and per remote domain. The addresses are bounded in bytes so that
-- the primary key's index entry stays below PostgreSQL's B-tree limit.
-- See server/docs/planning/s2s-routing-2047.md.
CREATE TABLE s2s_space_subscriptions (
    service_jid TEXT NOT NULL,
    node_name TEXT NOT NULL,
    subscriber_jid TEXT NOT NULL,
    account_jid TEXT NOT NULL,
    subscriber_domain TEXT NOT NULL,
    node_created_at_ms BIGINT NOT NULL,
    strength TEXT NOT NULL,
    trust_profile TEXT,
    snapshot_generation TEXT NOT NULL,
    subscribed_at_ms BIGINT NOT NULL,
    PRIMARY KEY (service_jid, node_name, subscriber_jid),
    CONSTRAINT s2s_space_subscriptions_strength CHECK (strength IN ('pkix', 'dialback')),
    CONSTRAINT s2s_space_subscriptions_trust_profile CHECK (trust_profile IS NULL OR trust_profile IN ('public_webpki', 'private_ca')),
    CONSTRAINT s2s_space_subscriptions_service_jid CHECK (octet_length(service_jid) BETWEEN 1 AND 255),
    CONSTRAINT s2s_space_subscriptions_node_name CHECK (octet_length(node_name) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_space_subscriptions_subscriber_jid CHECK (octet_length(subscriber_jid) BETWEEN 1 AND 1023)
);

CREATE INDEX s2s_space_subscriptions_account ON s2s_space_subscriptions (account_jid);
CREATE INDEX s2s_space_subscriptions_domain ON s2s_space_subscriptions (subscriber_domain);
