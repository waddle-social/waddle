-- The node a direct request is addressed to when it is not the owner of the
-- recipient's account.
--
-- With the account coordinator selected, delivery to a full JID reads the
-- resource's current binding from the coordinator. A binding hosted by
-- another node is reached by a direct request addressed to that node: its
-- lane names the node as its target, and the request records the node
-- incarnation and the exact binding, so the hosting node hands the stanza
-- only to the connection that carries that binding.
--
-- `target_node_id`, `target_node_epoch`: the hosting node incarnation.
-- `target_binding`: the binding, in its serialized identity form.
-- All three are set together or not at all. A request without a target is
-- decided by the node that holds the account's claim.
ALTER TABLE ingress_direct_requests
    ADD COLUMN IF NOT EXISTS target_node_id TEXT NULL,
    ADD COLUMN IF NOT EXISTS target_node_epoch TEXT NULL,
    ADD COLUMN IF NOT EXISTS target_binding TEXT NULL;
ALTER TABLE ingress_direct_requests
    DROP CONSTRAINT IF EXISTS ingress_direct_requests_target_check;
ALTER TABLE ingress_direct_requests
    ADD CONSTRAINT ingress_direct_requests_target_check
        CHECK ((target_node_id IS NULL AND target_node_epoch IS NULL
                AND target_binding IS NULL)
            OR (target_node_id IS NOT NULL AND target_node_epoch IS NOT NULL
                AND target_binding IS NOT NULL
                AND target_node_id <> '' AND octet_length(target_node_id) <= 255
                AND target_node_epoch <> '' AND octet_length(target_node_epoch) <= 255
                AND octet_length(target_binding) BETWEEN 2 AND 2048));
