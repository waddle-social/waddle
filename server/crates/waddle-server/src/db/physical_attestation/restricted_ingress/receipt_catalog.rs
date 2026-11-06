//! Exact PG17 catalog checks for the separately provisioned receipt journal.
//! Nothing in this module installs grants or authorizes a serving selection.

use sqlx::postgres::types::Oid;
use sqlx::{PgConnection, Row};

use super::receipt_authority::ReceiptOperationPins;
use super::{RestrictedIngressProfileError, Result};
use crate::db::migrations::waddle::V1030_RECEIPT_OPERATION_JOURNAL_POSTGRES;

const LOOKUP_CANDIDATE: &str =
    include_str!("../../migrations/owner/p21_receipt_journal_lookup_candidate.sql");

// pg_node_tree's pinned PG17 representation of the migration's sole CHECK.
// Unknown platforms/representations fail closed rather than deparsing an
// attacker-controlled expression while deciding whether it is safe.
const HASH_CHECK: &str = "{OPEXPR :opno 96 :opfuncid 65 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({FUNCEXPR :funcid 720 :funcresulttype 23 :funcretset false :funcvariadic false :funcformat 0 :funccollid 0 :inputcollid 0 :args ({VAR :varno 1 :varattno 4 :vartype 17 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varnosyn 1 :varattnosyn 4 :location -1}) :location -1} {CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 32 0 0 0 0 0 0 0 ]}) :location -1}";

pub(super) async fn verify(
    connection: &mut PgConnection,
    schema: &str,
    runtime: &str,
    api: &str,
    pins: &ReceiptOperationPins,
) -> Result<()> {
    let quoted_schema: String = sqlx::query_scalar("SELECT pg_catalog.quote_ident($1)")
        .bind(schema)
        .fetch_one(&mut *connection)
        .await?;
    let source = V1030_RECEIPT_OPERATION_JOURNAL_POSTGRES
        .split_once("AS $p21_body$")
        .and_then(|(_, tail)| tail.split_once("$p21_body$").map(|(body, _)| body))
        .ok_or(RestrictedIngressProfileError::Mismatch("journal source"))?
        .replace("%1$I", &quoted_schema);

    let function = sqlx::query(
        r#"
        SELECT p.prosrc = $5 AND p.oid = $6 AND p.proowner = a.oid
          AND a.oid = $7 AND r.oid = $8 AND n.oid = $9 AND n.nspowner = $10
          AND l.lanname = 'plpgsql' AND p.prokind = 'f' AND p.pronargs = 4
          AND p.proargtypes = '2950 2950 23 17'::pg_catalog.oidvector
          AND p.proargnames = ARRAY['p_operation_id','p_message_key','p_receipt_kind','p_semantic_identity_hash']::text[]
          AND p.prorettype = 'pg_catalog.bool'::pg_catalog.regtype AND NOT p.proretset
          AND p.pronargdefaults = 0 AND p.proargdefaults IS NULL AND p.proargmodes IS NULL
          AND p.proallargtypes IS NULL AND p.provariadic = 0 AND p.prosqlbody IS NULL
          AND p.probin IS NULL AND p.protrftypes IS NULL AND p.prosupport = 0
          AND p.prosecdef AND p.provolatile = 'v' AND p.proparallel = 'u'
          AND NOT p.proleakproof AND NOT p.proisstrict AND p.procost = 100 AND p.prorows = 0
          AND p.proconfig = ARRAY['search_path=pg_catalog, pg_temp']::text[]
          AND pg_catalog.has_function_privilege(r.oid,p.oid,'EXECUTE')
          AND NOT pg_catalog.has_function_privilege(r.oid,p.oid,'EXECUTE WITH GRANT OPTION')
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) acl
              WHERE acl.privilege_type <> 'EXECUTE'
                 OR acl.grantee NOT IN (p.proowner,r.oid)
                 OR (acl.grantee = r.oid AND (acl.is_grantable OR acl.grantor <> p.proowner)))
          AND (SELECT count(*) FROM pg_catalog.pg_proc other
               WHERE other.pronamespace=n.oid AND other.proname=p.proname) = 1 AS exact
        FROM pg_catalog.pg_proc p
        JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
        JOIN pg_catalog.pg_language l ON l.oid=p.prolang
        JOIN pg_catalog.pg_roles r ON r.rolname=$2
        JOIN pg_catalog.pg_roles a ON a.rolname=$3
        WHERE n.nspname=$1 AND p.proname=$4
        "#,
    )
    .bind(schema)
    .bind(runtime)
    .bind(api)
    .bind("waddle_p21_record_effect_receipt")
    .bind(source)
    .bind(Oid(pins.journal_function_oid))
    .bind(Oid(pins.api_owner_role_oid))
    .bind(Oid(pins.runtime_role_oid))
    .bind(Oid(pins.schema_oid))
    .bind(Oid(pins.schema_owner_role_oid))
    .fetch_optional(&mut *connection)
    .await?;
    if !matches!(function, Some(row) if row.try_get::<bool, _>("exact")?) {
        return Err(RestrictedIngressProfileError::Mismatch("journal function"));
    }

    let lookup_source = LOOKUP_CANDIDATE
        .split_once("AS $p21_lookup$")
        .and_then(|(_, tail)| tail.split_once("$p21_lookup$").map(|(body, _)| body))
        .ok_or(RestrictedIngressProfileError::Mismatch(
            "journal lookup source",
        ))?
        .replace("__SCHEMA__", &quoted_schema);
    let lookup = sqlx::query(
        r#"
        SELECT p.prosrc = $5 AND p.oid = $6 AND p.proowner = a.oid
          AND a.oid = $7 AND r.oid = $8 AND n.oid = $9 AND n.nspowner = $10
          AND l.lanname = 'sql' AND p.prokind = 'f' AND p.pronargs = 1
          AND p.proargtypes = '2950'::pg_catalog.oidvector
          AND p.proargnames = ARRAY['p_operation_id','message_key','receipt_kind',
              'semantic_identity_hash','inserted']::text[]
          AND p.proallargtypes = ARRAY[
              'pg_catalog.uuid'::pg_catalog.regtype::oid,
              'pg_catalog.uuid'::pg_catalog.regtype::oid,
              'pg_catalog.int4'::pg_catalog.regtype::oid,
              'pg_catalog.bytea'::pg_catalog.regtype::oid,
              'pg_catalog.bool'::pg_catalog.regtype::oid]
          AND p.proargmodes = ARRAY['i','t','t','t','t']::"char"[]
          AND p.prorettype = 'pg_catalog.record'::pg_catalog.regtype AND p.proretset
          AND p.pronargdefaults = 0 AND p.proargdefaults IS NULL
          AND p.provariadic = 0 AND p.prosqlbody IS NULL AND p.probin IS NULL
          AND p.protrftypes IS NULL AND p.prosupport = 0
          AND p.prosecdef AND p.provolatile = 's' AND p.proparallel = 'u'
          AND NOT p.proleakproof AND NOT p.proisstrict AND p.procost = 100 AND p.prorows = 1
          AND p.proconfig = ARRAY['search_path=pg_catalog, pg_temp']::text[]
          AND pg_catalog.has_function_privilege(r.oid,p.oid,'EXECUTE')
          AND NOT pg_catalog.has_function_privilege(r.oid,p.oid,'EXECUTE WITH GRANT OPTION')
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) acl
              WHERE acl.privilege_type <> 'EXECUTE'
                 OR acl.grantee NOT IN (p.proowner,r.oid)
                 OR (acl.grantee = r.oid AND (acl.is_grantable OR acl.grantor <> p.proowner)))
          AND (SELECT count(*) FROM pg_catalog.pg_proc other
               WHERE other.pronamespace=n.oid AND other.proname=p.proname) = 1 AS exact
        FROM pg_catalog.pg_proc p
        JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
        JOIN pg_catalog.pg_language l ON l.oid=p.prolang
        JOIN pg_catalog.pg_roles r ON r.rolname=$2
        JOIN pg_catalog.pg_roles a ON a.rolname=$3
        WHERE n.nspname=$1 AND p.proname=$4
        "#,
    )
    .bind(schema)
    .bind(runtime)
    .bind(api)
    .bind("waddle_p21_lookup_receipt_operation")
    .bind(lookup_source)
    .bind(Oid(pins.journal_lookup_function_oid))
    .bind(Oid(pins.api_owner_role_oid))
    .bind(Oid(pins.runtime_role_oid))
    .bind(Oid(pins.schema_oid))
    .bind(Oid(pins.schema_owner_role_oid))
    .fetch_optional(&mut *connection)
    .await?;
    if !matches!(lookup, Some(row) if row.try_get::<bool, _>("exact")?) {
        return Err(RestrictedIngressProfileError::Mismatch(
            "journal lookup function",
        ));
    }

    let table: bool = sqlx::query_scalar(
        r#"
        SELECT c.oid=$4 AND n.oid=$5 AND n.nspowner=$6 AND c.relowner=n.nspowner
          AND c.relkind='r' AND c.relpersistence='p'
          AND c.relam=(SELECT oid FROM pg_catalog.pg_am WHERE amname='heap')
          AND NOT c.relispartition
          AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relhasrules
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid=c.oid OR i.inhparent=c.oid)
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy pol WHERE pol.polrelid=c.oid)
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid=c.oid)
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class=c.oid)
          AND (SELECT count(*) FROM pg_catalog.pg_attribute att WHERE att.attrelid=c.oid AND att.attnum>0)=5
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute att
            WHERE att.attrelid=c.oid AND att.attnum>0 AND (
              att.attisdropped OR NOT att.attnotnull OR att.atthasdef OR att.atthasmissing
              OR att.attgenerated<>'' OR att.attidentity<>'' OR att.attndims<>0
              OR att.attcollation<>0 OR att.attacl IS NOT NULL OR att.attoptions IS NOT NULL
              OR att.attfdwoptions IS NOT NULL OR att.atttypmod<>-1
              OR (att.attnum,att.attname,att.atttypid) NOT IN (
                (1,'operation_id','pg_catalog.uuid'::pg_catalog.regtype),
                (2,'message_key','pg_catalog.uuid'::pg_catalog.regtype),
                (3,'receipt_kind','pg_catalog.int4'::pg_catalog.regtype),
                (4,'semantic_identity_hash','pg_catalog.bytea'::pg_catalog.regtype),
                (5,'inserted','pg_catalog.bool'::pg_catalog.regtype))))
          AND (SELECT count(*) FROM pg_catalog.pg_constraint con WHERE con.conrelid=c.oid)=2
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con
            WHERE con.conrelid=c.oid AND con.conname='p21_receipt_operation_results_hash_size'
              AND con.contype='c' AND con.convalidated AND NOT con.condeferrable
              AND con.conkey=ARRAY[4]::smallint[] AND con.conbin::text=$9)
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con
            WHERE con.conrelid=c.oid AND con.conname='p21_receipt_operation_results_pkey'
              AND con.contype='p' AND con.convalidated AND NOT con.condeferrable
              AND con.conkey=ARRAY[1]::smallint[])
          AND (SELECT count(*) FROM pg_catalog.pg_index i WHERE i.indrelid=c.oid)=1
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i
            JOIN pg_catalog.pg_class idx ON idx.oid=i.indexrelid
            JOIN pg_catalog.pg_am am ON am.oid=idx.relam
            WHERE i.indrelid=c.oid AND idx.relname='p21_receipt_operation_results_pkey'
              AND idx.relkind='i' AND idx.relowner=n.nspowner AND am.amname='btree'
              AND i.indisprimary AND i.indisunique AND i.indisvalid AND i.indisready AND i.indislive
              AND i.indimmediate AND NOT i.indisexclusion AND NOT i.indnullsnotdistinct
              AND i.indnatts=1 AND i.indnkeyatts=1 AND i.indkey::text='1'
              AND i.indclass::text='10065' AND i.indcollation::text='0' AND i.indoption::text='0'
              AND i.indexprs IS NULL AND i.indpred IS NULL)
          AND NOT pg_catalog.has_table_privilege(r.oid,c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
          AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'SELECT,INSERT,UPDATE')
          AND pg_catalog.has_table_privilege(a.oid,c.oid,'SELECT')
          AND pg_catalog.has_table_privilege(a.oid,c.oid,'INSERT')
          AND NOT pg_catalog.has_table_privilege(a.oid,c.oid,'UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl
            WHERE acl.grantee<>c.relowner AND (acl.grantor<>c.relowner OR acl.is_grantable OR NOT (
              (acl.grantee=a.oid AND acl.privilege_type IN ('SELECT','INSERT'))))) AS exact
        FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
        JOIN pg_catalog.pg_roles r ON r.rolname=$2 AND r.oid=$7
        JOIN pg_catalog.pg_roles a ON a.rolname=$3 AND a.oid=$8
        WHERE n.nspname=$1 AND c.relname='p21_receipt_operation_results'
        "#,
    )
    .bind(schema)
    .bind(runtime)
    .bind(api)
    .bind(Oid(pins.journal_relation_oid))
    .bind(Oid(pins.schema_oid))
    .bind(Oid(pins.schema_owner_role_oid))
    .bind(Oid(pins.runtime_role_oid))
    .bind(Oid(pins.api_owner_role_oid))
    .bind(HASH_CHECK)
    .fetch_optional(&mut *connection)
    .await?
    .unwrap_or(false);
    if !table {
        return Err(RestrictedIngressProfileError::Mismatch("journal table/ACL"));
    }
    Ok(())
}
