-- The Spaces accounts at other servers may reach over server-to-server
-- streams (ADR-0020 D10; XEP-0503 over XEP-0060): discover the node, read
-- its items and subscribe to it, each still decided for the asker.
--
-- A row exists only while a Space's `federated` setting is on. No row
-- exists for a Space when this runs, so every Space that exists is not
-- federated; an owner turns the setting on for one Space at a time.
--
-- `node_created_at_ms` is the creation time of the PubSub node the setting
-- was turned on for. The setting counts only for that node: a Space that is
-- deleted and created again under the same name is not federated, whatever
-- row its predecessor left. The names are bounded in bytes so that the
-- primary key's index entry stays below PostgreSQL's B-tree limit.
-- See server/docs/planning/s2s-routing-2047.md.
CREATE TABLE s2s_federated_spaces (
    service_jid TEXT NOT NULL,
    node_name TEXT NOT NULL,
    node_created_at_ms INTEGER NOT NULL,
    enabled_at_ms INTEGER NOT NULL,
    PRIMARY KEY (service_jid, node_name),
    CONSTRAINT s2s_federated_spaces_service_jid CHECK (length(CAST(service_jid AS BLOB)) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_federated_spaces_node_name CHECK (length(CAST(node_name AS BLOB)) BETWEEN 1 AND 1023)
);
