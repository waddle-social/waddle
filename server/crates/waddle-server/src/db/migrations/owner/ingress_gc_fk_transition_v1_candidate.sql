-- DARK LABORATORY ONLY: unregistered v1 FK transition. Do not install at startup.
-- Requires an independently attested primary and roles, all old writers/GC and
-- privileged DDL drained, and custody writers quiescent for the entire experiment.
-- The epoch barrier alone does not drain detached in-memory SM appends.
-- Run as the table owner inside one READ COMMITTED transaction. An error rolls
-- back both the catalog change and the surrounding ACL/API transition. Old GC
-- relies on CASCADE and MUST NOT run after this transition.
SET LOCAL lock_timeout = '100ms';
LOCK TABLE ONLY __SCHEMA__.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE;
LOCK TABLE ONLY
    __SCHEMA__.ingress_archive_dispatch,
    __SCHEMA__.ingress_carbon_receipts,
    __SCHEMA__.ingress_deliveries,
    __SCHEMA__.ingress_delivery_receipts,
    __SCHEMA__.ingress_effect_descendants,
    __SCHEMA__.ingress_effect_intents,
    __SCHEMA__.ingress_effect_receipts,
    __SCHEMA__.ingress_messages,
    __SCHEMA__.ingress_origin_aliases,
    __SCHEMA__.ingress_owner_rejections,
    __SCHEMA__.ingress_send_attempts,
    __SCHEMA__.ingress_sm_refs,
    __SCHEMA__.muc_invite_claims IN ACCESS EXCLUSIVE MODE;

-- Fail closed on an unknown FK graph, including foreign-schema descendants.
-- The catalog must be protected from concurrent privileged DDL for the whole
-- transaction; this fragment is not an owner/lineage attestation mechanism.
DO $fk_preflight$
BEGIN
    IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'waddle gc v1: requires READ COMMITTED';
    END IF;
    IF EXISTS (
      WITH expected(child, name, parent, child_keys, parent_keys, delete_action, is_deferrable, valid_contract) AS (
      VALUES
        ('ingress_archive_dispatch', 'ingress_archive_dispatch_message_key_kind_semantic_identit_fkey', 'ingress_effect_intents', ARRAY[3,4,5]::smallint[], ARRAY[1,3,4]::smallint[], 'c'::"char", false, true),
        ('ingress_carbon_receipts', 'ingress_carbon_receipts_message_key_kind_semantic_identity_fkey', 'ingress_effect_intents', ARRAY[1,2,3]::smallint[], ARRAY[1,3,4]::smallint[], 'c'::"char", false, true),
        ('ingress_delivery_receipts', 'ingress_delivery_receipts_message_key_kind_semantic_identi_fkey', 'ingress_effect_intents', ARRAY[1,2,3]::smallint[], ARRAY[1,3,4]::smallint[], 'c'::"char", false, true),
        ('ingress_effect_descendants', 'ingress_effect_descendants_message_key_kind_semantic_ident_fkey', 'ingress_effect_intents', ARRAY[1,2,3]::smallint[], ARRAY[1,3,4]::smallint[], 'c'::"char", false, true),
        ('ingress_effect_receipts', 'ingress_effect_receipts_message_key_kind_semantic_identity_fkey', 'ingress_effect_intents', ARRAY[1,2,3]::smallint[], ARRAY[1,3,4]::smallint[], 'c'::"char", false, true),
        ('ingress_send_attempts', 'ingress_send_attempts_message_key_kind_semantic_identity_h_fkey', 'ingress_effect_intents', ARRAY[1,2,3]::smallint[], ARRAY[1,3,4]::smallint[], 'c'::"char", false, true),
        ('ingress_deliveries', 'ingress_deliveries_message_key_fkey', 'ingress_messages', ARRAY[2]::smallint[], ARRAY[1]::smallint[], 'a'::"char", false, true),
        ('ingress_effect_intents', 'ingress_effect_intents_message_key_fkey', 'ingress_messages', ARRAY[1]::smallint[], ARRAY[1]::smallint[], 'c'::"char", false, true),
        ('ingress_origin_aliases', 'ingress_origin_aliases_message_key_fkey', 'ingress_messages', ARRAY[6]::smallint[], ARRAY[1]::smallint[], 'a'::"char", false, true),
        ('ingress_owner_rejections', 'ingress_owner_rejections_child_message_key_fkey', 'ingress_messages', ARRAY[2]::smallint[], ARRAY[1]::smallint[], 'c'::"char", false, true),
        ('ingress_owner_rejections', 'ingress_owner_rejections_source_message_key_fkey', 'ingress_messages', ARRAY[1]::smallint[], ARRAY[1]::smallint[], 'r'::"char", false, true),
        ('ingress_sm_refs', 'ingress_sm_refs_message_key_fkey', 'ingress_messages', ARRAY[3]::smallint[], ARRAY[1]::smallint[], 'a'::"char", false, true),
        ('muc_invite_claims', 'muc_invite_claims_message_key_fkey', 'ingress_messages', ARRAY[1]::smallint[], ARRAY[1]::smallint[], 'c'::"char", false, true)
      ), actual AS (
        SELECT child.relname::text AS child, con.conname::text AS name,
               parent.relname::text AS parent, con.conkey AS child_keys,
               con.confkey AS parent_keys, con.confdeltype AS delete_action,
               con.condeferrable AS is_deferrable,
               (con.confupdtype = 'a' AND con.confmatchtype = 's'
                AND con.convalidated AND NOT con.condeferred
                AND (SELECT COUNT(*) = 4 FROM pg_catalog.pg_trigger t
                     WHERE t.tgconstraint = con.oid AND t.tgisinternal AND t.tgenabled = 'O')) AS valid_contract
        FROM pg_catalog.pg_constraint con
        JOIN pg_catalog.pg_class child ON child.oid = con.conrelid
        JOIN pg_catalog.pg_namespace cn ON cn.oid = child.relnamespace
        JOIN pg_catalog.pg_class parent ON parent.oid = con.confrelid
        WHERE con.contype = 'f'
          AND parent.relnamespace = pg_catalog.to_regnamespace('__SCHEMA_PATH__')
          AND parent.relname IN (
            'ingress_messages', 'ingress_effect_intents', 'ingress_origin_aliases',
            'ingress_sm_refs', 'ingress_deliveries', 'ingress_owner_rejections',
            'ingress_archive_dispatch', 'ingress_carbon_receipts',
            'ingress_delivery_receipts', 'ingress_effect_descendants',
            'ingress_effect_receipts', 'ingress_send_attempts', 'muc_invite_claims'
          )
          AND cn.oid = pg_catalog.to_regnamespace('__SCHEMA_PATH__')
      )
      (SELECT * FROM expected EXCEPT SELECT * FROM actual)
      UNION ALL
      (SELECT * FROM actual EXCEPT SELECT * FROM expected)
    ) OR EXISTS (
      SELECT 1 FROM pg_catalog.pg_constraint con
      JOIN pg_catalog.pg_class parent ON parent.oid = con.confrelid
      JOIN pg_catalog.pg_class child ON child.oid = con.conrelid
      WHERE con.contype = 'f'
        AND parent.relnamespace = pg_catalog.to_regnamespace('__SCHEMA_PATH__')
        AND parent.relname IN (
          'ingress_messages', 'ingress_effect_intents', 'ingress_origin_aliases',
          'ingress_sm_refs', 'ingress_deliveries', 'ingress_owner_rejections',
          'ingress_archive_dispatch', 'ingress_carbon_receipts',
          'ingress_delivery_receipts', 'ingress_effect_descendants',
          'ingress_effect_receipts', 'ingress_send_attempts', 'muc_invite_claims')
        AND child.relnamespace <> parent.relnamespace
    ) THEN
        RAISE EXCEPTION 'waddle gc v1: unexpected canonical FK graph';
    END IF;
END
$fk_preflight$;

ALTER TABLE __SCHEMA__.ingress_archive_dispatch
    DROP CONSTRAINT ingress_archive_dispatch_message_key_kind_semantic_identit_fkey,
    ADD CONSTRAINT ingress_archive_dispatch_message_key_kind_semantic_identit_fkey FOREIGN KEY (message_key, kind, semantic_identity_hash)
    REFERENCES __SCHEMA__.ingress_effect_intents(message_key, kind, semantic_identity_hash)
    ON DELETE NO ACTION NOT DEFERRABLE;
ALTER TABLE __SCHEMA__.ingress_carbon_receipts
    DROP CONSTRAINT ingress_carbon_receipts_message_key_kind_semantic_identity_fkey,
    ADD CONSTRAINT ingress_carbon_receipts_message_key_kind_semantic_identity_fkey FOREIGN KEY (message_key, kind, semantic_identity_hash)
    REFERENCES __SCHEMA__.ingress_effect_intents(message_key, kind, semantic_identity_hash)
    ON DELETE NO ACTION NOT DEFERRABLE;
ALTER TABLE __SCHEMA__.ingress_delivery_receipts
    DROP CONSTRAINT ingress_delivery_receipts_message_key_kind_semantic_identi_fkey,
    ADD CONSTRAINT ingress_delivery_receipts_message_key_kind_semantic_identi_fkey FOREIGN KEY (message_key, kind, semantic_identity_hash)
    REFERENCES __SCHEMA__.ingress_effect_intents(message_key, kind, semantic_identity_hash)
    ON DELETE NO ACTION NOT DEFERRABLE;
ALTER TABLE __SCHEMA__.ingress_effect_descendants
    DROP CONSTRAINT ingress_effect_descendants_message_key_kind_semantic_ident_fkey,
    ADD CONSTRAINT ingress_effect_descendants_message_key_kind_semantic_ident_fkey FOREIGN KEY (message_key, kind, semantic_identity_hash)
    REFERENCES __SCHEMA__.ingress_effect_intents(message_key, kind, semantic_identity_hash)
    ON DELETE NO ACTION NOT DEFERRABLE;
ALTER TABLE __SCHEMA__.ingress_effect_receipts
    DROP CONSTRAINT ingress_effect_receipts_message_key_kind_semantic_identity_fkey,
    ADD CONSTRAINT ingress_effect_receipts_message_key_kind_semantic_identity_fkey FOREIGN KEY (message_key, kind, semantic_identity_hash)
    REFERENCES __SCHEMA__.ingress_effect_intents(message_key, kind, semantic_identity_hash)
    ON DELETE NO ACTION NOT DEFERRABLE;
ALTER TABLE __SCHEMA__.ingress_send_attempts
    DROP CONSTRAINT ingress_send_attempts_message_key_kind_semantic_identity_h_fkey,
    ADD CONSTRAINT ingress_send_attempts_message_key_kind_semantic_identity_h_fkey FOREIGN KEY (message_key, kind, semantic_identity_hash)
    REFERENCES __SCHEMA__.ingress_effect_intents(message_key, kind, semantic_identity_hash)
    ON DELETE NO ACTION NOT DEFERRABLE;
ALTER TABLE __SCHEMA__.ingress_effect_intents
    DROP CONSTRAINT ingress_effect_intents_message_key_fkey,
    ADD CONSTRAINT ingress_effect_intents_message_key_fkey FOREIGN KEY (message_key)
    REFERENCES __SCHEMA__.ingress_messages(message_key) ON DELETE NO ACTION NOT DEFERRABLE;
ALTER TABLE __SCHEMA__.ingress_owner_rejections
    DROP CONSTRAINT ingress_owner_rejections_child_message_key_fkey,
    ADD CONSTRAINT waddle_ingress_owner_rejections_child_gc_v1_fkey FOREIGN KEY (child_message_key)
    REFERENCES __SCHEMA__.ingress_messages(message_key)
    ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE;
