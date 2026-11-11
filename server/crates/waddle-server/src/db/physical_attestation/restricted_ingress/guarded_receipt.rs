//! Fixed, independently pinned singleton and receipt catalog profile.
//! This verifier does not select a store or establish the external catalog
//! mutation and old-backend cutoff premise.

use sqlx::postgres::types::Oid;
use sqlx::PgConnection;

use super::{
    catalog_preflight, receipt_catalog, ReceiptOperationPins, RestrictedIngressProfileError, Result,
};
use crate::db::physical_attestation::quote_identifier;

const CANDIDATE: &str = include_str!("../../migrations/owner/p21_guarded_receipt_candidate.sql");
const SLOT_CATALOG: &str = include_str!("singleton_catalog.sql");
const SLOT_CONSTRAINTS: &str = include_str!("singleton_constraints.sql");

/// Every object identity comes from the independent provisioner, not from the
/// runtime session whose profile is being verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardedReceiptOperationPins {
    pub receipt: ReceiptOperationPins,
    pub singleton_slot_relation_oid: u32,
    pub singleton_slot_primary_index_oid: u32,
    pub singleton_lease_relation_oid: u32,
    pub singleton_lease_primary_index_oid: u32,
    pub singleton_lease_generation_index_oid: u32,
    pub singleton_lease_identity_index_oid: u32,
    pub guarded_receipt_function_oid: u32,
}

impl GuardedReceiptOperationPins {
    fn valid(&self) -> bool {
        let values = [
            self.singleton_slot_relation_oid,
            self.singleton_slot_primary_index_oid,
            self.singleton_lease_relation_oid,
            self.singleton_lease_primary_index_oid,
            self.singleton_lease_generation_index_oid,
            self.singleton_lease_identity_index_oid,
        ];
        self.receipt.valid()
            && self.guarded_receipt_function_oid != 0
            && ![
                self.receipt.prepare_function_oid,
                self.receipt.effect_receipt_function_oid,
                self.receipt.policy_discard_function_oid,
                self.receipt.journal_function_oid,
                self.receipt.journal_lookup_function_oid,
            ]
            .contains(&self.guarded_receipt_function_oid)
            && values.iter().all(|oid| *oid != 0)
            && values
                .iter()
                .enumerate()
                .all(|(index, oid)| !values[..index].contains(oid))
            && !values.contains(&self.receipt.epoch_relation_oid)
            && !values.contains(&self.receipt.journal_relation_oid)
    }
}

/// Verify the complete receipt profile in the caller's bounded transaction.
/// The caller must already hold the shared cooperative catalog fence and the
/// independently pinned physical, ledger, epoch and manifest barriers. The
/// caller must compare shared role/schema/epoch pins with its pool authority.
/// No raw connection or checked proof may escape the caller's transaction.
pub(crate) async fn verify_guarded_receipt_profile(
    connection: &mut PgConnection,
    schema: &str,
    runtime: &str,
    api: &str,
    pins: &GuardedReceiptOperationPins,
) -> Result<()> {
    if !pins.valid() {
        return Err(RestrictedIngressProfileError::Mismatch(
            "guarded receipt pins",
        ));
    }
    let receipt = &pins.receipt;
    let observed = super::inspect_profile_with_receipt_journal(
        connection,
        schema,
        receipt.schema_oid,
        runtime,
        api,
        receipt.epoch_relation_oid,
        super::ProfileMode::GuardedReceipt,
    )
    .await?;
    if observed.runtime_role_oid != receipt.runtime_role_oid
        || observed.api_owner_oid != receipt.api_owner_role_oid
    {
        return Err(RestrictedIngressProfileError::Mismatch(
            "guarded receipt roles",
        ));
    }
    receipt_catalog::verify(connection, schema, runtime, api, receipt, true).await?;

    // Bounded storage was checked by the ingress profile before either of
    // these queries. Check built-in-only dependencies before deparsing checks.
    let exact: bool = sqlx::query_scalar(SLOT_CATALOG)
        .bind(schema)
        .bind(Oid(receipt.schema_oid))
        .bind(Oid(receipt.schema_owner_role_oid))
        .bind(Oid(receipt.runtime_role_oid))
        .bind(Oid(receipt.api_owner_role_oid))
        .bind(Oid(pins.singleton_slot_relation_oid))
        .bind(Oid(pins.singleton_slot_primary_index_oid))
        .bind(Oid(pins.singleton_lease_relation_oid))
        .bind(Oid(pins.singleton_lease_primary_index_oid))
        .bind(Oid(pins.singleton_lease_generation_index_oid))
        .bind(Oid(pins.singleton_lease_identity_index_oid))
        .fetch_one(&mut *connection)
        .await?;
    if !exact {
        return Err(RestrictedIngressProfileError::Mismatch(
            "singleton catalog/ACL",
        ));
    }
    catalog_preflight::verify_expression_dependencies(
        connection,
        schema,
        &["singleton_candidate_slot", "singleton_candidate_leases"],
    )
    .await?;
    let exact_constraints: bool = sqlx::query_scalar(SLOT_CONSTRAINTS)
        .bind(Oid(pins.singleton_slot_relation_oid))
        .bind(Oid(pins.singleton_lease_relation_oid))
        .bind(Oid(pins.singleton_slot_primary_index_oid))
        .bind(Oid(pins.singleton_lease_primary_index_oid))
        .bind(Oid(pins.singleton_lease_generation_index_oid))
        .bind(Oid(pins.singleton_lease_identity_index_oid))
        .fetch_one(&mut *connection)
        .await?;
    if !exact_constraints {
        return Err(RestrictedIngressProfileError::Mismatch(
            "singleton constraints",
        ));
    }

    let body = CANDIDATE
        .split_once("AS $p21_guarded$")
        .and_then(|(_, tail)| tail.split_once("$p21_guarded$").map(|(body, _)| body))
        .ok_or(RestrictedIngressProfileError::Mismatch(
            "guarded receipt source",
        ))?
        .replace("__SCHEMA__", &quote_identifier(schema));
    let function: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT p.oid=$4 AND n.oid=$5 AND n.nspowner=$6 AND p.proowner=$7
          AND p.prosrc=$8 AND l.lanname='plpgsql' AND p.prokind='f'
          AND p.pronargs=7 AND p.proargtypes='25 2950 20 2950 2950 23 17'::pg_catalog.oidvector
          AND p.proargnames=ARRAY['p_node_id','p_incarnation','p_generation','p_operation_id',
              'p_message_key','p_receipt_kind','p_semantic_identity_hash']::text[]
          AND p.prorettype='pg_catalog.bool'::pg_catalog.regtype AND NOT p.proretset
          AND p.pronargdefaults=0 AND p.proargdefaults IS NULL AND p.proargmodes IS NULL
          AND p.proallargtypes IS NULL AND p.provariadic=0 AND p.prosqlbody IS NULL
          AND p.probin IS NULL AND p.protrftypes IS NULL AND p.prosupport=0
          AND p.prosecdef AND p.provolatile='v' AND p.proparallel='u'
          AND NOT p.proleakproof AND NOT p.proisstrict AND p.procost=100 AND p.prorows=0
          AND p.proconfig=ARRAY['search_path=pg_catalog, pg_temp']::text[]
          AND pg_catalog.has_function_privilege($9,p.oid,'EXECUTE')
          AND NOT pg_catalog.has_function_privilege($9,p.oid,'EXECUTE WITH GRANT OPTION')
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) acl
              WHERE acl.privilege_type<>'EXECUTE' OR acl.grantee NOT IN (p.proowner,$9)
                OR (acl.grantee=$9 AND (acl.is_grantable OR acl.grantor<>p.proowner)))
          AND (SELECT count(*) FROM pg_catalog.pg_proc other
               WHERE other.pronamespace=n.oid AND other.proname=p.proname)=1
        FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
        JOIN pg_catalog.pg_language l ON l.oid=p.prolang
        JOIN pg_catalog.pg_roles r ON r.rolname=$2 AND r.oid=$9
        JOIN pg_catalog.pg_roles a ON a.rolname=$3 AND a.oid=$7
        WHERE n.nspname=$1 AND p.proname='waddle_p21_guarded_record_effect_receipt'
    "#,
    )
    .bind(schema)
    .bind(runtime)
    .bind(api)
    .bind(Oid(pins.guarded_receipt_function_oid))
    .bind(Oid(receipt.schema_oid))
    .bind(Oid(receipt.schema_owner_role_oid))
    .bind(Oid(receipt.api_owner_role_oid))
    .bind(body)
    .bind(Oid(receipt.runtime_role_oid))
    .fetch_optional(&mut *connection)
    .await?;
    if function != Some(true) {
        return Err(RestrictedIngressProfileError::Mismatch(
            "guarded receipt function/ACL",
        ));
    }
    Ok(())
}
