-- Dark candidate only. The owner transition installs this fixed exact-key
-- read API after transferring the record function to the API owner. The runtime
-- receives EXECUTE on this function and no direct journal table privileges.
-- A UUID key narrows each read; it does not authorize a caller with the runtime
-- credential who knows another operation ID to read its historical tuple.
CREATE FUNCTION __SCHEMA__.waddle_p21_lookup_receipt_operation(
    p_operation_id UUID
)
RETURNS TABLE (
    message_key UUID,
    receipt_kind INTEGER,
    semantic_identity_hash BYTEA,
    inserted BOOLEAN
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
ROWS 1
AS $p21_lookup$
    SELECT journal.message_key,
           journal.receipt_kind,
           journal.semantic_identity_hash,
           journal.inserted
      FROM __SCHEMA__.p21_receipt_operation_results AS journal
     WHERE journal.operation_id = p_operation_id
$p21_lookup$;

REVOKE ALL ON FUNCTION __SCHEMA__.waddle_p21_lookup_receipt_operation(UUID) FROM PUBLIC;
DO $p21_lookup_acl$
DECLARE
    target_schema TEXT := __SCHEMA_LITERAL__;
    unexpected_grantee BOOLEAN;
BEGIN
    SELECT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_proc proc
        JOIN pg_catalog.pg_namespace ns ON ns.oid = proc.pronamespace
        CROSS JOIN LATERAL pg_catalog.aclexplode(
            COALESCE(proc.proacl, pg_catalog.acldefault('f', proc.proowner))
        ) acl
        WHERE ns.nspname = target_schema
          AND proc.proname = 'waddle_p21_lookup_receipt_operation'
          AND acl.grantee <> proc.proowner
    ) INTO unexpected_grantee;
    IF unexpected_grantee THEN
        RAISE EXCEPTION 'waddle: receipt lookup has unexpected default ACL grantees';
    END IF;
END
$p21_lookup_acl$;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_p21_lookup_receipt_operation(UUID) TO __RUNTIME_ROLE__;
