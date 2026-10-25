-- Run with psql -X -v ON_ERROR_STOP=1 against the primary application database.
-- Use the application role. This diagnostic never changes persistent state.
-- Results contain timestamps and protocol shape only, never message contents,
-- JIDs, node identifiers, canonical keys or semantic identity hashes.
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL statement_timeout = '10s';
SELECT current_user AS inspection_role,
       pg_is_in_recovery() AS database_is_replica,
       current_setting('transaction_read_only') AS transaction_read_only;

WITH pending AS (
    SELECT message.message_key, message.created_at, message.envelope_version,
           intent.payload_version,
           convert_from(intent.payload, 'UTF8')::jsonb -> 'intent' AS intent,
           XMLPARSE(DOCUMENT (
               convert_from(message.envelope, 'UTF8')::jsonb ->> 'message'
           )) AS message_xml
    FROM ingress_messages message
    JOIN ingress_effect_intents intent USING (message_key)
    WHERE message.terminal_at IS NULL
      AND message.created_at <= now() - interval '10 minutes'
      AND intent.kind = 12
      AND NOT EXISTS (
          SELECT 1 FROM ingress_effect_receipts receipt
          WHERE receipt.message_key = intent.message_key
            AND receipt.kind = intent.kind
            AND receipt.semantic_identity_hash = intent.semantic_identity_hash
      )
), bounded AS (
    SELECT * FROM pending ORDER BY created_at, message_key LIMIT 100
)
SELECT now() AS observed_at, pending.created_at, pending.envelope_version,
       pending.payload_version,
       (xpath('string(/*/@type)', pending.message_xml))[1]::text AS message_type,
       (xpath('count(/*/*[local-name()="body" and namespace-uri()="jabber:client"])',
              pending.message_xml))[1]::text AS body_count,
       ARRAY(
           SELECT (xpath('local-name(/*)', child))[1]::text || ' {' ||
                  (xpath('namespace-uri(/*)', child))[1]::text || '}'
           FROM unnest(xpath('/*/*', pending.message_xml)) AS children(child)
       ) AS child_shapes,
       target.node_id IS NOT NULL AS recorded_target_present,
       target.expired AS recorded_target_expired,
       claim.entity IS NOT NULL AS current_claim_present,
       (claim.node_id = pending.intent #>> '{relay_target,node_id}' AND
        claim.node_epoch = pending.intent #>> '{relay_target,node_epoch}')
           AS current_owner_matches_recorded_target,
       current_owner.expired AS current_owner_expired,
       (SELECT count(*) FROM ingress_origin_aliases alias
        WHERE alias.message_key = pending.message_key) AS origin_alias_count,
       (SELECT count(*) FROM ingress_delivery_receipts receipt
        WHERE receipt.message_key = pending.message_key) AS delivery_receipt_count,
       (SELECT count(*) FROM ingress_effect_descendants descendant
        WHERE descendant.message_key = pending.message_key) AS descendant_count
FROM bounded pending
LEFT JOIN clustering_nodes target
  ON target.node_id = pending.intent #>> '{relay_target,node_id}'
 AND target.node_epoch = pending.intent #>> '{relay_target,node_epoch}'
LEFT JOIN clustering_claims claim
  ON claim.entity_type = 'room_actor'
 AND claim.entity = 'room_actor:' || (pending.intent ->> 'room')
LEFT JOIN clustering_nodes current_owner
  ON current_owner.node_id = claim.node_id
 AND current_owner.node_epoch = claim.node_epoch
ORDER BY pending.created_at, pending.message_key;

COMMIT;
