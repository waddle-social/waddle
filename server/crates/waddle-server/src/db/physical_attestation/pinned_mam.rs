//! Bounded, dark MAM lookup on one pinned, read-only transaction.
//!
//! The operator must independently supply the MAM heap and index identities.
//! An independently enforced quiescence or trusted mutation protocol must
//! cover catalog, index, and role changes through lookup and rollback. The heap
//! lock does not stop `CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY`,
//! or `REINDEX CONCURRENTLY`. The complete attestor dependency graph also
//! requires an independent pre-attestor check before the pool's connection
//! hooks run. The 16 MiB row limit is a refusal threshold, not proof about
//! historical archives. This is not a startup selector or a general MAM store
//! authority.

use crate::db::DarkEpochAclStage;
use std::sync::Arc;

use sqlx::postgres::types::Oid;
use thiserror::Error;
use tokio::sync::{oneshot, Semaphore};
use waddle_xmpp::mam::storage::BoundedMamLookupError;
use waddle_xmpp::mam::{ArchivedMessage, SqlxMamStorage};
use waddle_xmpp_core::xep0359::StanzaId;

use super::pinned_entry::{prepare_entry, DarkPinnedEntryError, ENTRY_DEADLINE};
use super::{quote_identifier, AttestationMode, AttestedPgPool, PinnedRuntimeAuthority};
use crate::db::{Database, Transaction};

/// Independently provisioned MAM object identities. The nine index OIDs are,
/// in order: `mam_messages_pkey`, `idx_mam_room_timestamp`,
/// `idx_mam_room_sender`, `idx_mam_room_id`, `idx_mam_room_thread`,
/// `idx_mam_room_reply_to`, `idx_mam_room_origin`, `idx_mam_room_stanza`,
/// and `idx_mam_room_archive_seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
pub struct PinnedMamLookupPins {
    pub messages_relation_oid: u32,
    pub index_oids: [u32; 9],
}

#[derive(Debug, Error)]
#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
pub enum PinnedMamLookupError {
    #[error("MAM lookup requires a pinned pool and distinct nonzero object pins")]
    InvalidPins,
    #[error("MAM lookup is at its bounded concurrency limit")]
    Overloaded,
    #[error("MAM lookup requires a Tokio runtime")]
    RuntimeUnavailable,
    #[error("the MAM lookup worker did not return a result")]
    WorkerLost,
    #[error("the MAM lookup transaction timed out")]
    Timeout,
    #[error("MAM lookup rollback was not acknowledged")]
    RollbackUnconfirmed,
    #[error("the pinned MAM catalog differs from the supplied identities or shape")]
    CatalogDrift,
    #[error(transparent)]
    Entry(#[from] DarkPinnedEntryError),
    #[error(transparent)]
    Mam(#[from] BoundedMamLookupError),
    #[error("MAM catalog PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// Non-cloneable authority for a single fixed lookup. The pool and every
/// transaction stay private. Dropping a job does not cancel its worker. Once
/// BEGIN returns a transaction, the worker retains its admission permit and
/// exact backend through rollback completion. A failed checkout or BEGIN uses
/// the database adapter's cleanup path and cannot return a message or a miss.
/// No returned message or definitive miss is exposed before rollback is
/// acknowledged. Runtime shutdown or a backend failure is not rollback proof.
#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
pub struct PinnedMamLookupAuthority {
    pool: AttestedPgPool,
    pins: PinnedMamLookupPins,
    admission: Arc<Semaphore>,
}

/// An owned result receiver. Dropping it detaches the already-started worker.
#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
pub struct PinnedMamLookupJob {
    result: oneshot::Receiver<Result<Option<ArchivedMessage>, PinnedMamLookupError>>,
}

#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
struct LookupPause {
    ready: oneshot::Sender<(i32, bool, bool)>,
    release: oneshot::Receiver<()>,
}

impl PinnedMamLookupJob {
    #[cfg_attr(
        not(feature = "test-support"),
        expect(dead_code, reason = "pinned MAM awaits serving attachment")
    )]
    pub async fn finish(self) -> Result<Option<ArchivedMessage>, PinnedMamLookupError> {
        self.result
            .await
            .map_err(|_| PinnedMamLookupError::WorkerLost)?
    }
}

impl PinnedMamLookupAuthority {
    #[cfg_attr(
        not(feature = "test-support"),
        expect(dead_code, reason = "pinned MAM awaits serving attachment")
    )]
    pub fn new(
        pool: AttestedPgPool,
        pins: PinnedMamLookupPins,
    ) -> Result<Self, PinnedMamLookupError> {
        let AttestationMode::Pinned(authority) = &pool.writer_capability.mode else {
            return Err(PinnedMamLookupError::InvalidPins);
        };
        let mut all = vec![
            authority.attestor.lineage_relation_oid,
            authority.attestor.lineage_primary_index_oid,
            authority.ledger.ledger_relation_oid,
            authority.ledger.ledger_primary_index_oid,
            authority.epoch_relation_oid,
            authority.epoch_primary_index_oid,
            authority.guard_manifest_relation_oid,
            pins.messages_relation_oid,
        ];
        all.extend_from_slice(&pins.index_oids);
        if all.contains(&0)
            || all
                .iter()
                .enumerate()
                .any(|(position, oid)| all[..position].contains(oid))
            || pool.max_connections() == 0
        {
            return Err(PinnedMamLookupError::InvalidPins);
        }
        let admission = Arc::new(Semaphore::new(pool.max_connections() as usize));
        Ok(Self {
            pool,
            pins,
            admission,
        })
    }

    /// Start immediately, transferring the immutable typed request to bounded
    /// supervised ownership before the caller's first cancellable wait.
    #[cfg_attr(
        not(feature = "test-support"),
        expect(dead_code, reason = "pinned MAM awaits serving attachment")
    )]
    pub fn start_lookup(
        &self,
        stanza: StanzaId,
    ) -> Result<PinnedMamLookupJob, PinnedMamLookupError> {
        self.start(stanza, None)
    }

    /// Pause after catalog checks and relation locks for external observation.
    #[cfg(feature = "test-support")]
    pub fn start_lookup_paused_for_test(
        &self,
        stanza: StanzaId,
        ready: oneshot::Sender<(i32, bool, bool)>,
        release: oneshot::Receiver<()>,
    ) -> Result<PinnedMamLookupJob, PinnedMamLookupError> {
        self.start(stanza, Some(LookupPause { ready, release }))
    }

    #[cfg_attr(
        not(feature = "test-support"),
        expect(dead_code, reason = "pinned MAM awaits serving attachment")
    )]
    fn start(
        &self,
        stanza: StanzaId,
        pause: Option<LookupPause>,
    ) -> Result<PinnedMamLookupJob, PinnedMamLookupError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| PinnedMamLookupError::RuntimeUnavailable)?;
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| PinnedMamLookupError::Overloaded)?;
        let AttestationMode::Pinned(authority) = &self.pool.writer_capability.mode else {
            return Err(PinnedMamLookupError::InvalidPins);
        };
        let authority = (**authority).clone();
        let pins = self.pins;
        let database = Database::from_postgres_pool("pinned MAM lookup", self.pool.pool.clone());
        let (sender, result) = oneshot::channel();
        drop(runtime.spawn(async move {
            let _permit = permit;
            let outcome = lookup(&database, &authority, pins, stanza, pause).await;
            let _ = sender.send(outcome);
        }));
        Ok(PinnedMamLookupJob { result })
    }
}

#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
async fn lookup(
    database: &Database,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamLookupPins,
    stanza: StanzaId,
    pause: Option<LookupPause>,
) -> Result<Option<ArchivedMessage>, PinnedMamLookupError> {
    let mut transaction = database.begin().await.map_err(DarkPinnedEntryError::from)?;
    let outcome = tokio::time::timeout(ENTRY_DEADLINE, async {
        prepare_entry(
            &mut transaction,
            authority,
            DarkEpochAclStage::RelationGuardEpochZero,
            None,
        )
        .await?;
        preflight_mam(&mut transaction, authority, pins).await?;
        if let Some(pause) = pause {
            let connection = transaction
                .postgres_connection()
                .ok_or(PinnedMamLookupError::InvalidPins)?;
            let observation: (i32, bool, bool) = sqlx::query_as(
                "SELECT pg_catalog.pg_backend_pid(), \
                 pg_catalog.current_setting('transaction_read_only') = 'on', \
                 pg_catalog.current_setting('transaction_isolation') = 'read committed'",
            )
            .fetch_one(connection)
            .await?;
            let _ = pause.ready.send(observation);
            pause
                .release
                .await
                .map_err(|_| PinnedMamLookupError::Timeout)?;
        }
        let connection = transaction
            .postgres_connection()
            .ok_or(PinnedMamLookupError::InvalidPins)?;
        let schema = &authority.config.postgres().schema.name.0;
        Ok(
            SqlxMamStorage::lookup_bounded_archive_or_stanza_id_on_postgres_connection_in_schema(
                connection, schema, &stanza,
            )
            .await?,
        )
    })
    .await
    .unwrap_or(Err(PinnedMamLookupError::Timeout));

    // Cancellation of the receiving future cannot cancel this worker. Even an
    // entry/query timeout therefore reaches this explicit acknowledgement. A
    // stalled cleanup retains its exact checkout and admission permit.
    transaction
        .rollback()
        .await
        .map_err(|_| PinnedMamLookupError::RollbackUnconfirmed)?;
    outcome
}

#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
async fn preflight_mam(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamLookupPins,
) -> Result<(), PinnedMamLookupError> {
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamLookupError::InvalidPins)?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(connection)
        .await?;
    verify_mam_catalog(tx, authority, pins).await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamLookupError::InvalidPins)?;
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.mam_messages IN ACCESS SHARE MODE",
        quote_identifier(&authority.config.postgres().schema.name.0)
    ))
    .execute(connection)
    .await?;
    // READ COMMITTED gives this post-wait query a fresh catalog snapshot.
    verify_mam_catalog(tx, authority, pins).await
}

#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
async fn verify_mam_catalog(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamLookupPins,
) -> Result<(), PinnedMamLookupError> {
    verify_mam_catalog_with_acl(tx, authority, pins, false).await
}

#[cfg(feature = "test-support")]
pub(super) async fn verify_mam_append_messages_catalog(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamLookupPins,
) -> Result<(), PinnedMamLookupError> {
    verify_mam_catalog_with_acl(tx, authority, pins, true).await
}

async fn verify_mam_catalog_with_acl(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamLookupPins,
    append_acl: bool,
) -> Result<(), PinnedMamLookupError> {
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamLookupError::InvalidPins)?;
    let relation = Oid(pins.messages_relation_oid);
    let schema = Oid(authority.config.postgres().schema.oid);
    let owner = Oid(authority.attestor.schema_owner_role_oid);
    let runtime = Oid(authority.attestor.runtime_role_oid);
    let indexes: Vec<Oid> = pins.index_oids.into_iter().map(Oid).collect();
    let inventory: bool = sqlx::query_scalar(INVENTORY)
        .bind(relation)
        .bind(schema)
        .bind(owner)
        .bind(&authority.config.postgres().schema.name.0)
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !inventory {
        return Err(PinnedMamLookupError::CatalogDrift);
    }
    // Bound raw variable-length catalog vectors before the shape check expands
    // them. Counts were capped by a separate statement above.
    let bounded: bool = sqlx::query_scalar(BOUNDED)
        .bind(relation)
        .fetch_one(&mut *connection)
        .await?;
    if !bounded {
        return Err(PinnedMamLookupError::CatalogDrift);
    }
    let exact: bool = sqlx::query_scalar(SHAPE)
        .bind(relation)
        .bind(schema)
        .bind(owner)
        .bind(runtime)
        .bind(&indexes)
        .bind(append_acl)
        .fetch_one(&mut *connection)
        .await?;
    if !exact {
        return Err(PinnedMamLookupError::CatalogDrift);
    }
    Ok(())
}

#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
const INVENTORY: &str = r#"
SELECT c.relnamespace=$2 AND c.relowner=$3 AND c.relname='mam_messages'
  AND n.oid=$2 AND n.nspname=$4 AND n.nspowner=$3
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute a
       WHERE a.attrelid=$1 LIMIT 33) bounded) <= 32
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_index i
       WHERE i.indrelid=$1 LIMIT 10) bounded) = 9
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_statistic_ext s
       WHERE s.stxrelid=$1 LIMIT 1) bounded) = 0
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amop o
       WHERE o.amopfamily IN (1976::pg_catalog.oid,1994::pg_catalog.oid,434::pg_catalog.oid)
       LIMIT 257) bounded) <= 256
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amproc p
       WHERE p.amprocfamily IN (1976::pg_catalog.oid,1994::pg_catalog.oid,434::pg_catalog.oid)
       LIMIT 257) bounded) <= 256
FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
WHERE c.oid=$1
"#;

#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
const BOUNDED: &str = r#"
SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid=$1
  AND c.relacl IS NOT NULL AND (pg_catalog.pg_column_compression(c.relacl) IS NOT NULL
    OR pg_catalog.pg_column_size(c.relacl)>4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a WHERE a.attrelid=$1
    AND a.attacl IS NOT NULL AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL
      OR pg_catalog.pg_column_size(a.attacl)>4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index i WHERE i.indrelid=$1 AND (
    i.indnatts>3 OR i.indnkeyatts>3
    OR pg_catalog.pg_column_compression(i.indkey) IS NOT NULL OR pg_catalog.pg_column_size(i.indkey)>4096
    OR pg_catalog.pg_column_compression(i.indclass) IS NOT NULL OR pg_catalog.pg_column_size(i.indclass)>4096
    OR pg_catalog.pg_column_compression(i.indcollation) IS NOT NULL OR pg_catalog.pg_column_size(i.indcollation)>4096
    OR pg_catalog.pg_column_compression(i.indoption) IS NOT NULL OR pg_catalog.pg_column_size(i.indoption)>4096))
"#;

#[cfg_attr(
    not(feature = "test-support"),
    expect(dead_code, reason = "pinned MAM awaits serving attachment")
)]
const SHAPE: &str = r#"
WITH expected_columns(name, typ, required) AS (VALUES
 ('id',25::pg_catalog.oid,true), ('room_jid',25::pg_catalog.oid,true),
 ('timestamp',1184::pg_catalog.oid,true), ('from_jid',25::pg_catalog.oid,true),
 ('to_jid',25::pg_catalog.oid,true), ('body',25::pg_catalog.oid,false),
 ('stanza_id',25::pg_catalog.oid,false), ('thread_id',25::pg_catalog.oid,false),
 ('reply_to_id',25::pg_catalog.oid,false), ('reply_to_jid',25::pg_catalog.oid,false),
 ('origin_id',25::pg_catalog.oid,false), ('message_type',25::pg_catalog.oid,true),
 ('stanza_xml',25::pg_catalog.oid,false), ('rich_payload',25::pg_catalog.oid,false),
 ('nickname_generation',20::pg_catalog.oid,false), ('parent_thread_id',25::pg_catalog.oid,false),
 ('archive_seq',20::pg_catalog.oid,true), ('created_at',1184::pg_catalog.oid,true)
), expected_indexes(pos, name, unique_index, primary_index, keys, options) AS (VALUES
 (1,'mam_messages_pkey',true,true, ARRAY['id'], ARRAY[0]),
 (2,'idx_mam_room_timestamp',false,false, ARRAY['room_jid','timestamp'], ARRAY[0,3]),
 (3,'idx_mam_room_sender',false,false, ARRAY['room_jid','from_jid','timestamp'], ARRAY[0,0,3]),
 (4,'idx_mam_room_id',false,false, ARRAY['room_jid','id'], ARRAY[0,0]),
 (5,'idx_mam_room_thread',false,false, ARRAY['room_jid','thread_id','timestamp'], ARRAY[0,0,3]),
 (6,'idx_mam_room_reply_to',false,false, ARRAY['room_jid','reply_to_id','timestamp'], ARRAY[0,0,3]),
 (7,'idx_mam_room_origin',false,false, ARRAY['room_jid','origin_id'], ARRAY[0,0]),
 (8,'idx_mam_room_stanza',false,false, ARRAY['room_jid','stanza_id'], ARRAY[0,0]),
 (9,'idx_mam_room_archive_seq',true,false, ARRAY['room_jid','archive_seq'], ARRAY[0,0])
)
SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid=$1
  AND c.relnamespace=$2 AND c.relowner=$3 AND c.relkind='r' AND c.relpersistence='p'
  AND c.relam=2::pg_catalog.oid AND NOT c.relispartition AND c.reloftype=0::pg_catalog.oid
  AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relhasrules
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid=c.oid OR h.inhparent=c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r WHERE r.ev_class=c.oid)
  AND pg_catalog.has_table_privilege($4,c.oid,'SELECT')
  AND CASE WHEN $6 THEN pg_catalog.has_table_privilege($4,c.oid,'INSERT')
           ELSE NOT pg_catalog.has_table_privilege($4,c.oid,'INSERT') END
  AND NOT pg_catalog.has_table_privilege($4,c.oid,'UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
  AND NOT pg_catalog.has_table_privilege($4,c.oid,'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION,MAINTAIN WITH GRANT OPTION')
  AND NOT pg_catalog.has_any_column_privilege($4,c.oid,'UPDATE,REFERENCES')
  AND ($6 OR NOT pg_catalog.has_any_column_privilege($4,c.oid,'INSERT'))
  AND NOT pg_catalog.has_any_column_privilege($4,c.oid,'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION')
  AND EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
    WHERE acl.grantee=$4 AND acl.grantor=c.relowner
      AND acl.privilege_type='SELECT' AND NOT acl.is_grantable)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
    COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl
    WHERE acl.grantee=0::pg_catalog.oid OR (acl.grantee=$4 AND
      (acl.grantor<>c.relowner OR acl.is_grantable OR NOT (
        acl.privilege_type='SELECT' OR ($6 AND acl.privilege_type='INSERT')))))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
    WHERE a.attrelid=c.oid AND (acl.grantee=0::pg_catalog.oid OR
      (acl.grantee=$4 AND (acl.grantor<>c.relowner OR acl.is_grantable OR acl.privilege_type<>'SELECT')))))
 AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute a
      WHERE a.attrelid=$1 AND a.attnum>0 AND NOT a.attisdropped)=18
 AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
      WHERE a.attrelid=$1 AND a.attnum>0 AND a.attisdropped)
 AND NOT EXISTS (SELECT 1 FROM expected_columns e WHERE NOT EXISTS (
   SELECT 1 FROM pg_catalog.pg_attribute a WHERE a.attrelid=$1 AND a.attnum>0
     AND NOT a.attisdropped AND a.attname=e.name AND a.atttypid=e.typ AND a.attnotnull=e.required
     AND a.attcollation=CASE WHEN e.typ=25::pg_catalog.oid THEN 100::pg_catalog.oid ELSE 0::pg_catalog.oid END
     AND a.atttypmod=-1 AND a.attndims=0 AND a.attislocal AND a.attinhcount=0
     AND NOT a.atthasmissing AND a.attgenerated='' AND a.attidentity=''
     AND a.attoptions IS NULL AND a.attfdwoptions IS NULL))
 AND NOT EXISTS (SELECT 1 FROM expected_indexes e WHERE NOT EXISTS (
   SELECT 1 FROM pg_catalog.pg_index i
   JOIN pg_catalog.pg_class idx ON idx.oid=i.indexrelid
   JOIN pg_catalog.pg_am am ON am.oid=idx.relam
   WHERE i.indrelid=$1 AND idx.oid=($5::pg_catalog.oid[])[e.pos]
     AND idx.relname=e.name AND idx.relnamespace=$2 AND idx.relowner=$3
     AND idx.relkind='i' AND idx.relpersistence='p' AND idx.relam=403::pg_catalog.oid
     AND am.amname='btree' AND am.amtype='i' AND am.amhandler=330::pg_catalog.oid
     AND i.indisunique=e.unique_index AND i.indisprimary=e.primary_index
     AND i.indisvalid AND i.indisready AND i.indislive AND i.indimmediate
     AND NOT i.indisexclusion AND NOT i.indnullsnotdistinct
     AND i.indnatts=pg_catalog.cardinality(e.keys) AND i.indnkeyatts=i.indnatts
     AND i.indexprs IS NULL AND i.indpred IS NULL
     AND NOT EXISTS (SELECT 1 FROM pg_catalog.generate_series(0,i.indnkeyatts-1) n(pos)
       LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid=$1 AND a.attnum=i.indkey[n.pos]
       LEFT JOIN pg_catalog.pg_opclass op ON op.oid=i.indclass[n.pos]
       LEFT JOIN pg_catalog.pg_opfamily f ON f.oid=op.opcfamily
       WHERE a.attname IS DISTINCT FROM e.keys[n.pos+1]
         OR i.indoption[n.pos] IS DISTINCT FROM e.options[n.pos+1]::pg_catalog.int2
         OR i.indcollation[n.pos] IS DISTINCT FROM a.attcollation
         OR op.opcnamespace IS DISTINCT FROM 11::pg_catalog.oid
         OR op.opcmethod IS DISTINCT FROM 403::pg_catalog.oid
         OR op.opcintype IS DISTINCT FROM a.atttypid OR NOT op.opcdefault
         OR op.opckeytype IS DISTINCT FROM 0::pg_catalog.oid
         OR op.oid IS DISTINCT FROM CASE a.atttypid
           WHEN 25::pg_catalog.oid THEN 3126::pg_catalog.oid
           WHEN 20::pg_catalog.oid THEN 3124::pg_catalog.oid
           WHEN 1184::pg_catalog.oid THEN 3127::pg_catalog.oid END
         OR op.opcname IS DISTINCT FROM CASE a.atttypid
           WHEN 25::pg_catalog.oid THEN 'text_ops'
           WHEN 20::pg_catalog.oid THEN 'int8_ops'
           WHEN 1184::pg_catalog.oid THEN 'timestamptz_ops' END
         OR f.opfnamespace IS DISTINCT FROM 11::pg_catalog.oid
         OR f.opfmethod IS DISTINCT FROM 403::pg_catalog.oid
         OR f.oid IS DISTINCT FROM CASE a.atttypid
           WHEN 25::pg_catalog.oid THEN 1994::pg_catalog.oid
           WHEN 20::pg_catalog.oid THEN 1976::pg_catalog.oid
           WHEN 1184::pg_catalog.oid THEN 434::pg_catalog.oid END
         OR EXISTS (SELECT 1 FROM pg_catalog.pg_amproc ap
           JOIN pg_catalog.pg_proc p ON p.oid=ap.amproc
           WHERE ap.amprocfamily=CASE a.atttypid
             WHEN 25::pg_catalog.oid THEN 1994::pg_catalog.oid
             WHEN 20::pg_catalog.oid THEN 1976::pg_catalog.oid
             WHEN 1184::pg_catalog.oid THEN 434::pg_catalog.oid END
             AND p.pronamespace<>11::pg_catalog.oid)
         OR EXISTS (SELECT 1 FROM pg_catalog.pg_amop ao
           JOIN pg_catalog.pg_operator o ON o.oid=ao.amopopr
           JOIN pg_catalog.pg_proc p ON p.oid=o.oprcode
           WHERE ao.amopfamily=CASE a.atttypid
             WHEN 25::pg_catalog.oid THEN 1994::pg_catalog.oid
             WHEN 20::pg_catalog.oid THEN 1976::pg_catalog.oid
             WHEN 1184::pg_catalog.oid THEN 434::pg_catalog.oid END
             AND (o.oprnamespace<>11::pg_catalog.oid OR p.pronamespace<>11::pg_catalog.oid)))))
"#;
