//! Unwired, same-process ordered-claim observation over one admitted pool.
//!
//! This lab does not release, offer, send, or acknowledge rows. Its opaque
//! attempt is not durable across process loss; a visible row does not recover
//! the original batch or establish what an absent row means.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use waddle_xmpp::pending_delivery::storage::PendingClaimToken;
use waddle_xmpp::pending_delivery::{PendingRowId, SmSessionId, SM_SESSION_ID_MAX_LEN};

use crate::db::physical_attestation::{
    quote_identifier, validate_identifier, AttestedPostgresWriterCapability,
};
use crate::db::{Database, DatabaseDriver, DatabaseError, DatabasePool, Row, Transaction};

const MAX_BATCH: usize = 64;
const MAX_ROW_ID_BYTES: usize = 128;
const POSTGRES_DEADLINE: Duration = Duration::from_secs(15);

tokio::task_local! {
    static LOSE_COMMIT_ACK: bool;
    static TEST_SEARCH_PATH: String;
}

/// Refusal or uncertainty from the dark ordered-claim lab.
#[derive(Debug, thiserror::Error)]
pub enum OrderedClaimError {
    #[error("the pool does not supply the required bound lanes")]
    InvalidPool,
    #[error("SQLite requires exactly one pool connection")]
    InvalidSqliteCapacity,
    #[error("claim batch must contain between 1 and 64 rows")]
    InvalidLimit,
    #[error("the attempt has an invalid session or recipient identity")]
    InvalidIdentity,
    #[error("the attempt belongs to another authority")]
    WrongAuthority,
    #[error("the attempt has already started")]
    AttemptAlreadyStarted,
    #[error("the attempt has not started")]
    AttemptNotStarted,
    #[error("an earlier claim for this recipient is still unsequenced")]
    Contended,
    #[error("the database is a PostgreSQL standby")]
    Standby,
    #[error("SQLite TEMP triggers are not permitted by this lab")]
    TemporaryTrigger,
    #[error("invalid or oversized pending row identity or claim state")]
    CorruptRow,
    #[error("pending relation lacks the required ordinary-table unique row identity")]
    UnsafePendingRelation,
    #[error("more matching rows exist than the attempt was permitted to claim")]
    ObservationOverflow,
    #[error("the transaction outcome is indeterminate")]
    Indeterminate,
    #[error("the observation could not complete")]
    ObservationUnavailable,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// A visible phase is a point-in-time observation, not proof of delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderedClaimPhase {
    Unoffered,
    Offered,
    Sequenced { offered: bool },
}

struct AttemptInner {
    authority: Arc<()>,
    recipient: BareJid,
    session: SmSessionId,
    token: PendingClaimToken,
    limit: usize,
    started: AtomicBool,
}

/// Opaque same-process identity, minted before a claim is dispatched.
#[derive(Clone)]
pub(crate) struct OrderedClaimAttempt(Arc<AttemptInner>);

/// A row positively observed for one exact attempt.
#[derive(Debug)]
pub(crate) struct OrderedClaimObservation {
    row_id: PendingRowId,
    phase: OrderedClaimPhase,
}

impl OrderedClaimObservation {
    pub(crate) fn row_id(&self) -> &PendingRowId {
        &self.row_id
    }

    pub(crate) fn phase(&self) -> OrderedClaimPhase {
        self.phase
    }
}

/// Closed dark capability. It intentionally has no database accessor.
pub(crate) struct OrderedClaimAuthority {
    main: Database,
    postgres: Option<AttestedPostgresWriterCapability>,
    pending: String,
    mam: String,
    binding: Arc<()>,
}

impl OrderedClaimAuthority {
    /// Attach without DDL to the two relations of exactly this pool.
    pub(crate) async fn from_pool(pool: &DatabasePool) -> Result<Self, OrderedClaimError> {
        let main = pool.global().clone();
        let (postgres, pending, mam) = match main.driver() {
            DatabaseDriver::Sqlite => {
                let sqlite = main.sqlite_pool().ok_or(OrderedClaimError::InvalidPool)?;
                if sqlite.options().get_max_connections() != 1 {
                    return Err(OrderedClaimError::InvalidSqliteCapacity);
                }
                (
                    None,
                    "main.pending_delivery".to_owned(),
                    "main.mam_messages".to_owned(),
                )
            }
            DatabaseDriver::Postgres => {
                let ingress = pool.ingress();
                let capability = main
                    .attested_pg_writer_capability()
                    .ok_or(OrderedClaimError::InvalidPool)?;
                if !ingress.is_ingress_lane() || ingress.ingress_pool_size().is_none() {
                    return Err(OrderedClaimError::InvalidPool);
                }
                ingress
                    .verify_attested_pg_writer_capability(&capability)
                    .map_err(DatabaseError::from)?;
                // The physical-attestation configuration validates this name.
                let schema = quote_identifier(&capability.identity().schema.name.0);
                (
                    Some(capability),
                    format!("{schema}.pending_delivery"),
                    format!("{schema}.mam_messages"),
                )
            }
        };
        let authority = Self {
            main,
            postgres,
            pending,
            mam,
            binding: Arc::new(()),
        };
        authority
            .with_read_deadline(authority.verify_relations())
            .await?;
        Ok(authority)
    }

    pub(crate) fn attempt(
        &self,
        recipient: BareJid,
        session: SmSessionId,
        limit: usize,
    ) -> Result<OrderedClaimAttempt, OrderedClaimError> {
        if !(1..=MAX_BATCH).contains(&limit) {
            return Err(OrderedClaimError::InvalidLimit);
        }
        if session.as_str().is_empty()
            || session.as_str().len() > SM_SESSION_ID_MAX_LEN
            || recipient.to_string().len() > 3071
        {
            return Err(OrderedClaimError::InvalidIdentity);
        }
        Ok(OrderedClaimAttempt(Arc::new(AttemptInner {
            authority: Arc::clone(&self.binding),
            recipient,
            session,
            token: PendingClaimToken::fresh(),
            limit,
            started: AtomicBool::new(false),
        })))
    }

    fn check_attempt(&self, attempt: &OrderedClaimAttempt) -> Result<(), OrderedClaimError> {
        if Arc::ptr_eq(&self.binding, &attempt.0.authority) {
            Ok(())
        } else {
            Err(OrderedClaimError::WrongAuthority)
        }
    }

    /// Dispatch once. Dropping the future leaves the result indeterminate.
    pub(crate) async fn claim(
        &self,
        attempt: &OrderedClaimAttempt,
    ) -> Result<Vec<OrderedClaimObservation>, OrderedClaimError> {
        self.check_attempt(attempt)?;
        if attempt.0.started.swap(true, Ordering::AcqRel) {
            return Err(OrderedClaimError::AttemptAlreadyStarted);
        }
        let operation = self.claim_inner(attempt);
        if self.postgres.is_some() {
            tokio::time::timeout(POSTGRES_DEADLINE, operation)
                .await
                .map_err(|_| OrderedClaimError::Indeterminate)?
        } else {
            operation.await
        }
    }

    /// Observe currently visible matching rows, not the original batch/order.
    pub(crate) async fn observe(
        &self,
        attempt: &OrderedClaimAttempt,
    ) -> Result<Vec<OrderedClaimObservation>, OrderedClaimError> {
        self.check_started(attempt)?;
        self.with_read_deadline(self.observe_inner(attempt, None))
            .await
    }

    /// An absent or mismatched row is not a rollback or completion proof.
    pub(crate) async fn observe_row(
        &self,
        attempt: &OrderedClaimAttempt,
        row_id: &PendingRowId,
    ) -> Result<Option<OrderedClaimObservation>, OrderedClaimError> {
        self.check_started(attempt)?;
        if row_id.as_str().is_empty() || row_id.as_str().len() > MAX_ROW_ID_BYTES {
            return Err(OrderedClaimError::CorruptRow);
        }
        let mut rows = self
            .with_read_deadline(self.observe_inner(attempt, Some(row_id)))
            .await?;
        Ok(rows.pop())
    }

    fn check_started(&self, attempt: &OrderedClaimAttempt) -> Result<(), OrderedClaimError> {
        self.check_attempt(attempt)?;
        if attempt.0.started.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(OrderedClaimError::AttemptNotStarted)
        }
    }

    async fn with_read_deadline<T>(
        &self,
        operation: impl std::future::Future<Output = Result<T, OrderedClaimError>>,
    ) -> Result<T, OrderedClaimError> {
        if self.postgres.is_some() {
            tokio::time::timeout(POSTGRES_DEADLINE, operation)
                .await
                .map_err(|_| OrderedClaimError::ObservationUnavailable)?
        } else {
            // An in-flight timeout can destroy the only private-memory DB.
            operation.await
        }
    }

    async fn begin(&self, write: bool) -> Result<Transaction<'_>, OrderedClaimError> {
        let mut tx = if write {
            self.main.begin_immediate().await?
        } else {
            self.main.begin().await?
        };
        if let Some(capability) = &self.postgres {
            let mode = if write { "READ WRITE" } else { "READ ONLY" };
            tx.execute(
                &format!("SET TRANSACTION ISOLATION LEVEL READ COMMITTED {mode}"),
                (),
            )
            .await?;
            crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
                .await
                .map_err(|_| OrderedClaimError::ObservationUnavailable)?;
            capability.attest_transaction(&mut tx).await?;
            let mut rows = tx
                .query("SELECT NOT pg_catalog.pg_is_in_recovery()", ())
                .await?;
            let primary: bool = rows
                .next()
                .await?
                .ok_or(OrderedClaimError::Standby)?
                .get(0)?;
            drop(rows);
            if !primary {
                return Err(OrderedClaimError::Standby);
            }
            // A qualified relation alone is insufficient when operators and
            // aggregates can be shadowed by an earlier schema.
            let schema = quote_identifier(&capability.identity().schema.name.0);
            let safe_path = if let Ok(shadow) = TEST_SEARCH_PATH.try_with(Clone::clone) {
                validate_identifier(&shadow, "test shadow schema").map_err(DatabaseError::from)?;
                format!(
                    "pg_catalog, {}, {schema}, pg_temp",
                    quote_identifier(&shadow)
                )
            } else {
                format!("pg_catalog, {schema}, pg_temp")
            };
            tx.query(
                "SELECT pg_catalog.set_config('search_path', ?, true)",
                crate::db_params![safe_path],
            )
            .await?;
        } else {
            // This check and the following SQL use the same pinned connection.
            let mut rows = tx
                .query(
                    "SELECT 1 FROM temp.sqlite_schema WHERE type = 'trigger' LIMIT 1",
                    (),
                )
                .await?;
            let found = rows.next().await?.is_some();
            drop(rows);
            if found {
                return Err(OrderedClaimError::TemporaryTrigger);
            }
        }
        self.verify_pending_row_key(&mut tx).await?;
        if self.postgres.is_some() {
            self.verify_postgres_column_types(&mut tx).await?;
        } else {
            self.verify_sqlite_identity_types(&mut tx).await?;
        }
        Ok(tx)
    }

    // LIMIT bounds changed rows only if a candidate row ID can name at most
    // one row. Owner and privileged schema changes remain an external
    // quiescence prerequisite; this check is not an owner attestation.
    async fn verify_pending_row_key(
        &self,
        tx: &mut Transaction<'_>,
    ) -> Result<(), OrderedClaimError> {
        let (sql, params) = if let Some(capability) = &self.postgres {
            (
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace JOIN pg_catalog.pg_index i ON i.indrelid=t.oid JOIN pg_catalog.pg_class ix ON ix.oid=i.indexrelid JOIN pg_catalog.pg_am am ON am.oid=ix.relam JOIN pg_catalog.pg_opclass oc ON oc.oid=i.indclass[0] JOIN pg_catalog.pg_namespace opn ON opn.oid=oc.opcnamespace JOIN pg_catalog.pg_attribute a ON a.attrelid=t.oid AND a.attnum=i.indkey[0] WHERE am.amname='btree' AND opn.nspname='pg_catalog' AND oc.opcname IN ('text_ops','text_pattern_ops') AND oc.opcintype='pg_catalog.text'::pg_catalog.regtype AND oc.opcmethod=am.oid AND n.nspname=? AND t.relname='pending_delivery' AND t.relkind='r' AND t.relpersistence='p' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhparent=t.oid) AND a.attname='row_id' AND a.attnotnull AND i.indisunique AND i.indisvalid AND i.indisready AND i.indislive AND i.indimmediate AND i.indnkeyatts=1 AND i.indnatts=1 AND i.indpred IS NULL AND i.indexprs IS NULL)",
                crate::db_params![capability.identity().schema.name.0.clone()],
            )
        } else {
            (
                "SELECT EXISTS (SELECT 1 FROM pragma_table_list('pending_delivery') t WHERE t.schema='main' AND t.name='pending_delivery' AND t.type='table' AND t.wr=0) AND EXISTS (SELECT 1 FROM pragma_index_list('pending_delivery','main') i WHERE i.\"unique\"=1 AND i.partial=0 AND (SELECT count(*) FROM pragma_index_info(i.name,'main'))=1 AND (SELECT name FROM pragma_index_info(i.name,'main') WHERE seqno=0)='row_id' COLLATE BINARY AND EXISTS (SELECT 1 FROM pragma_index_xinfo(i.name,'main') k WHERE k.seqno=0 AND k.\"key\"=1 AND k.name='row_id' COLLATE BINARY AND upper(k.coll) IN ('BINARY','NOCASE','RTRIM')))",
                crate::db_params![],
            )
        };
        let mut rows = tx.query(sql, params).await?;
        let valid: bool = rows
            .next()
            .await?
            .ok_or(OrderedClaimError::UnsafePendingRelation)?
            .get(0)?;
        drop(rows);
        if valid {
            Ok(())
        } else {
            Err(OrderedClaimError::UnsafePendingRelation)
        }
    }

    // SQLx eagerly fetches RETURNING and observation rows. Check the actual
    // scalar types before querying, so malformed TEXT columns cannot return
    // unbounded values in place of the expected small numeric fields.
    async fn verify_postgres_column_types(
        &self,
        tx: &mut Transaction<'_>,
    ) -> Result<(), OrderedClaimError> {
        let capability = self
            .postgres
            .as_ref()
            .ok_or(OrderedClaimError::InvalidPool)?;
        let mut rows = tx.query(
            "SELECT count(*)=14 FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace JOIN pg_catalog.pg_attribute a ON a.attrelid=t.oid WHERE n.nspname=? AND t.relkind='r' AND t.relpersistence='p' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhparent=t.oid) AND a.attnum>0 AND NOT a.attisdropped AND ((t.relname='pending_delivery' AND ((a.attname IN ('row_id','recipient_jid','flushed_in_session','claim_token','payload_kind','archive_stanza_by','archive_stanza_id') AND a.atttypid='pg_catalog.text'::pg_catalog.regtype) OR (a.attname IN ('outbound_sequence','claim_offered') AND a.atttypid='pg_catalog.int4'::pg_catalog.regtype) OR (a.attname='claimed_at_ms' AND a.atttypid='pg_catalog.int8'::pg_catalog.regtype))) OR (t.relname='mam_messages' AND ((a.attname IN ('id','stanza_id','room_jid') AND a.atttypid='pg_catalog.text'::pg_catalog.regtype) OR (a.attname='archive_seq' AND a.atttypid='pg_catalog.int8'::pg_catalog.regtype))))",
            crate::db_params![capability.identity().schema.name.0.clone()],
        ).await?;
        let valid: bool = rows
            .next()
            .await?
            .ok_or(OrderedClaimError::UnsafePendingRelation)?
            .get(0)?;
        drop(rows);
        if valid {
            Ok(())
        } else {
            Err(OrderedClaimError::UnsafePendingRelation)
        }
    }

    // SQLite's collation does not disable numeric affinity. Require the
    // owner schema's TEXT declarations before comparing protocol identities.
    async fn verify_sqlite_identity_types(
        &self,
        tx: &mut Transaction<'_>,
    ) -> Result<(), OrderedClaimError> {
        let mut rows = tx.query(
            "SELECT (SELECT count(*) FROM pragma_table_info('pending_delivery','main') WHERE name IN ('row_id','recipient_jid','flushed_in_session','claim_token','payload_kind','archive_stanza_by','archive_stanza_id') AND upper(type)='TEXT') + (SELECT count(*) FROM pragma_table_info('mam_messages','main') WHERE name IN ('id','stanza_id','room_jid') AND upper(type)='TEXT') = 10",
            (),
        ).await?;
        let valid: bool = rows
            .next()
            .await?
            .ok_or(OrderedClaimError::UnsafePendingRelation)?
            .get(0)?;
        drop(rows);
        if valid {
            Ok(())
        } else {
            Err(OrderedClaimError::UnsafePendingRelation)
        }
    }

    async fn verify_relations(&self) -> Result<(), OrderedClaimError> {
        let mut tx = self.begin(false).await?;
        tx.query(&format!("SELECT row_id, recipient_jid, flushed_in_session, outbound_sequence, claimed_at_ms, claim_token, claim_offered, payload_kind, archive_stanza_by, archive_stanza_id FROM {} WHERE 1=0", self.pending), ()).await?;
        tx.query(
            &format!(
                "SELECT id, stanza_id, room_jid, archive_seq FROM {} WHERE 1=0",
                self.mam
            ),
            (),
        )
        .await?;
        tx.rollback().await?;
        Ok(())
    }

    fn bounded_text_expression(&self, value: &str, max_bytes: usize) -> String {
        let length = if self.postgres.is_some() {
            format!("pg_catalog.octet_length({value})")
        } else {
            format!("length(CAST({value} AS BLOB))")
        };
        let text_check = if self.postgres.is_some() {
            String::new()
        } else {
            format!("typeof({value}) = 'text' AND ")
        };
        format!(
            "CASE WHEN {text_check}{length} BETWEEN 1 AND {max_bytes} THEN {value} ELSE NULL END"
        )
    }

    fn identity_collation(&self) -> &'static str {
        if self.postgres.is_some() {
            "pg_catalog.\"C\""
        } else {
            "BINARY"
        }
    }

    fn zero_offer_expression(&self, field: &str) -> String {
        if self.postgres.is_some() {
            format!("{field} = 0")
        } else {
            format!("typeof({field}) = 'integer' AND {field} = 0")
        }
    }

    async fn claim_inner(
        &self,
        attempt: &OrderedClaimAttempt,
    ) -> Result<Vec<OrderedClaimObservation>, OrderedClaimError> {
        let mut tx = self.begin(true).await?;
        if self.postgres.is_some() {
            let mut rows = tx
                .query(
                    "SELECT pg_catalog.pg_try_advisory_xact_lock(pg_catalog.hashtext(?))",
                    crate::db_params![attempt.0.recipient.to_string()],
                )
                .await?;
            let acquired: bool = rows
                .next()
                .await?
                .ok_or(OrderedClaimError::Contended)?
                .get(0)?;
            drop(rows);
            if !acquired {
                return Err(OrderedClaimError::Contended);
            }
        }
        let coll = self.identity_collation();
        let mut prefix = tx.query(&format!("SELECT 1 FROM {} WHERE recipient_jid COLLATE {coll} = ? AND flushed_in_session IS NOT NULL AND outbound_sequence IS NULL LIMIT 1", self.pending), crate::db_params![attempt.0.recipient.to_string()]).await?;
        let contended = prefix.next().await?.is_some();
        drop(prefix);
        if contended {
            return Err(OrderedClaimError::Contended);
        }
        let pending_offer = self.zero_offer_expression("p.claim_offered");
        let mut invalid = tx.query(
            &format!("SELECT 1 FROM {} p WHERE p.recipient_jid COLLATE {coll} = ? AND p.flushed_in_session IS NULL AND (p.outbound_sequence IS NOT NULL OR p.claim_token IS NOT NULL OR p.claimed_at_ms IS NOT NULL OR NOT COALESCE(({pending_offer}), FALSE)) LIMIT 1", self.pending),
            crate::db_params![attempt.0.recipient.to_string()],
        ).await?;
        let inconsistent = invalid.next().await?.is_some();
        drop(invalid);
        if inconsistent {
            return Err(OrderedClaimError::CorruptRow);
        }
        let bounded_id = self.bounded_text_expression("row_id", MAX_ROW_ID_BYTES);
        let bounded_recipient = self.bounded_text_expression("recipient_jid", 3071);
        let target_offer = self.zero_offer_expression("pending_delivery.claim_offered");
        let ordinal_result = if self.postgres.is_some() {
            "ordinal"
        } else {
            "CASE WHEN ordinal IS NULL THEN NULL WHEN typeof(ordinal) = 'integer' THEN ordinal ELSE -1 END"
        };
        let sql = format!(
            "WITH eligible AS ( \
                 SELECT p.row_id COLLATE {coll} AS row_id, p.payload_kind, p.archive_stanza_by, p.archive_stanza_id, (SELECT MAX(m.archive_seq) FROM {mam} m \
                     WHERE m.room_jid COLLATE {coll}=p.archive_stanza_by AND (m.id COLLATE {coll}=p.archive_stanza_id OR m.stanza_id COLLATE {coll}=p.archive_stanza_id)) AS ordinal \
                 FROM {pending} p WHERE p.recipient_jid COLLATE {coll} = ? AND p.flushed_in_session IS NULL AND p.outbound_sequence IS NULL AND p.claim_token IS NULL AND p.claimed_at_ms IS NULL AND ({pending_offer}) \
             ), merged AS ( \
                 SELECT row_id, payload_kind, archive_stanza_by, archive_stanza_id, ordinal, MAX(row_id) OVER (PARTITION BY payload_kind COLLATE {coll} \
                     ORDER BY ordinal ASC NULLS FIRST, row_id COLLATE {coll} ASC \
                     ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS fifo_key FROM eligible \
             ), candidates AS MATERIALIZED ( \
                 SELECT row_id, payload_kind, archive_stanza_by, archive_stanza_id, ordinal, ROW_NUMBER() OVER (ORDER BY fifo_key COLLATE {coll} ASC, ordinal ASC NULLS FIRST, row_id COLLATE {coll} ASC) AS claim_order \
                 FROM merged ORDER BY fifo_key COLLATE {coll} ASC, ordinal ASC NULLS FIRST, row_id COLLATE {coll} ASC LIMIT ? \
             ) \
             UPDATE {pending} SET flushed_in_session = ?, outbound_sequence = NULL, claimed_at_ms = ?, claim_token = ?, claim_offered = 0 \
             WHERE recipient_jid COLLATE {coll} = ? AND flushed_in_session IS NULL AND outbound_sequence IS NULL AND claim_token IS NULL AND claimed_at_ms IS NULL AND ({target_offer}) AND EXISTS ( \
                 SELECT 1 FROM candidates c WHERE c.row_id COLLATE {coll} = pending_delivery.row_id \
                 AND c.payload_kind COLLATE {coll} IS NOT DISTINCT FROM pending_delivery.payload_kind \
                 AND c.archive_stanza_by COLLATE {coll} IS NOT DISTINCT FROM pending_delivery.archive_stanza_by \
                 AND c.archive_stanza_id COLLATE {coll} IS NOT DISTINCT FROM pending_delivery.archive_stanza_id) \
             RETURNING {bounded_id}, {bounded_recipient}, (SELECT {ordinal_result} FROM candidates WHERE candidates.row_id COLLATE {coll} = pending_delivery.row_id), \
                       (SELECT claim_order FROM candidates WHERE candidates.row_id COLLATE {coll} = pending_delivery.row_id)",
            pending = self.pending, mam = self.mam,
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    attempt.0.recipient.to_string(),
                    attempt.0.limit as i64,
                    attempt.0.session.as_str().to_owned(),
                    chrono::Utc::now().timestamp_millis(),
                    attempt.0.token.to_string(),
                    attempt.0.recipient.to_string()
                ],
            )
            .await?;
        let mut claimed = Vec::new();
        while let Some(row) = rows.next().await? {
            let id = bounded_id_from_row(&row, 0)?;
            let recipient: Option<String> = row.get(1)?;
            if recipient.as_deref() != Some(attempt.0.recipient.to_string().as_str()) {
                return Err(OrderedClaimError::CorruptRow);
            }
            let ordinal: Option<i64> = row.get(2)?;
            if let Some(ordinal) = ordinal {
                waddle_xmpp::mam::ArchiveOrdinal::from_storage(ordinal)
                    .map_err(|_| OrderedClaimError::CorruptRow)?;
            }
            let order: i64 = row.get(3)?;
            if order <= 0 {
                return Err(OrderedClaimError::CorruptRow);
            }
            claimed.push((id, order));
        }
        drop(rows);
        tx.commit()
            .await
            .map_err(|_| OrderedClaimError::Indeterminate)?;
        if LOSE_COMMIT_ACK.try_with(|lost| *lost).unwrap_or(false) {
            return Err(OrderedClaimError::Indeterminate);
        }
        claimed.sort_by_key(|(_, order)| *order);
        Ok(claimed
            .into_iter()
            .map(|(row_id, _)| OrderedClaimObservation {
                row_id,
                phase: OrderedClaimPhase::Unoffered,
            })
            .collect())
    }

    async fn observe_inner(
        &self,
        attempt: &OrderedClaimAttempt,
        row_id: Option<&PendingRowId>,
    ) -> Result<Vec<OrderedClaimObservation>, OrderedClaimError> {
        let mut tx = self.begin(false).await?;
        let bounded_id = self.bounded_text_expression("row_id", MAX_ROW_ID_BYTES);
        let coll = self.identity_collation();
        let row_filter = if row_id.is_some() {
            format!(" AND row_id COLLATE {coll} = ?")
        } else {
            String::new()
        };
        let (offered, sequence) = if self.postgres.is_some() {
            ("claim_offered", "outbound_sequence")
        } else {
            ("CASE WHEN typeof(claim_offered) = 'integer' AND claim_offered IN (0,1) THEN claim_offered ELSE -1 END", "CASE WHEN outbound_sequence IS NULL THEN NULL WHEN typeof(outbound_sequence) = 'integer' AND outbound_sequence BETWEEN 0 AND 4294967295 THEN outbound_sequence ELSE -1 END")
        };
        let identity_types = if self.postgres.is_some() {
            "1"
        } else {
            "CASE WHEN typeof(recipient_jid)='text' AND typeof(flushed_in_session)='text' AND typeof(claim_token)='text' THEN 1 ELSE 0 END"
        };
        let sql = format!("SELECT {bounded_id}, {offered}, {sequence}, {identity_types} FROM {} WHERE recipient_jid COLLATE {coll} = ? AND flushed_in_session COLLATE {coll} = ? AND claim_token COLLATE {coll} = ?{row_filter} ORDER BY row_id COLLATE {coll} LIMIT ?", self.pending);
        let mut params = crate::db_params![
            attempt.0.recipient.to_string(),
            attempt.0.session.as_str().to_owned(),
            attempt.0.token.to_string()
        ];
        if let Some(row_id) = row_id {
            params.push(row_id.as_str().to_owned().into());
        }
        params.push(((attempt.0.limit + 1) as i64).into());
        let mut rows = tx.query(&sql, params).await?;
        let mut found = Vec::new();
        while let Some(row) = rows.next().await? {
            let row_id = bounded_id_from_row(&row, 0)?;
            if row.get::<i64>(3)? != 1 {
                return Err(OrderedClaimError::CorruptRow);
            }
            let offered: i64 = row.get(1)?;
            let offered = match offered {
                0 => false,
                1 => true,
                _ => return Err(OrderedClaimError::CorruptRow),
            };
            let sequence: Option<i64> = row.get(2)?;
            let phase = if let Some(sequence) = sequence {
                u32::try_from(sequence).map_err(|_| OrderedClaimError::CorruptRow)?;
                OrderedClaimPhase::Sequenced { offered }
            } else if offered {
                OrderedClaimPhase::Offered
            } else {
                OrderedClaimPhase::Unoffered
            };
            found.push(OrderedClaimObservation { row_id, phase });
        }
        drop(rows);
        if found.len() > attempt.0.limit {
            return Err(OrderedClaimError::ObservationOverflow);
        }
        tx.rollback().await?;
        Ok(found)
    }
}

fn bounded_id_from_row(row: &Row, column: usize) -> Result<PendingRowId, OrderedClaimError> {
    let value: Option<String> = row.get(column)?;
    let value = value.ok_or(OrderedClaimError::CorruptRow)?;
    if value.is_empty() || value.len() > MAX_ROW_ID_BYTES {
        return Err(OrderedClaimError::CorruptRow);
    }
    Ok(PendingRowId::new(value))
}

pub(crate) async fn with_lost_commit_ack<T>(future: impl std::future::Future<Output = T>) -> T {
    LOSE_COMMIT_ACK.scope(true, future).await
}

pub(crate) async fn with_search_path<T>(
    path: String,
    future: impl std::future::Future<Output = T>,
) -> T {
    TEST_SEARCH_PATH.scope(path, future).await
}
