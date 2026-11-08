//! Test-only bounded origin observation. The caller must prove its catalog
//! and collation on the same transaction before invoking this query.

use jid::{BareJid, Jid};
use sqlx::{PgConnection, Row};
use thiserror::Error;
use waddle_xmpp_core::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};
use xmpp_parsers::message::MessageType;

use super::decode::decode_postgres_message_row;
use crate::mam::storage::tombstone::{
    groupchat_subject_is_state_update, origin_id_tombstone_match_parts,
};

const KEY_BYTES: usize = 1024 * 1024;
const CANDIDATES: i64 = 64;
const ROW_BYTES: i64 = 16 * 1024 * 1024;
const TOTAL_BYTES: i64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum BoundedMamOriginError {
    #[error("MAM origin request exceeds its diagnostic limit")]
    RequestTooLarge,
    #[error("MAM origin candidate limit was exceeded")]
    CandidateOverflow,
    #[error("MAM origin candidate bytes exceed the diagnostic limit")]
    RowTooLarge,
    #[error("MAM origin candidate is malformed")]
    Decode,
    #[error("MAM origin query failed")]
    Query(#[source] sqlx::Error),
}

/// Only the bounded typed facts used by the existing tombstone predicate.
/// Constructing this does not clone the archived payload.
pub struct BoundedMamOriginProbe {
    archive: BareJid,
    origin: Option<OriginId>,
    from: Jid,
    sender_scope: Option<BareJid>,
    eligible: bool,
}

impl BoundedMamOriginProbe {
    pub fn new(
        archive: &BareJid,
        incoming: &ArchivedMessage,
    ) -> Result<Self, BoundedMamOriginError> {
        let sender = incoming
            .rich
            .as_ref()
            .and_then(|rich| rich.muc_sender.as_ref());
        if archive.as_str().len() > KEY_BYTES
            || incoming.from.as_str().len() > KEY_BYTES
            || incoming
                .origin_id
                .as_ref()
                .is_some_and(|origin| origin.as_str().len() > KEY_BYTES)
            || incoming
                .body
                .as_ref()
                .is_some_and(|body| body.len() > KEY_BYTES)
            || sender.is_some_and(|sender| sender.jid.as_str().len() > KEY_BYTES)
        {
            return Err(BoundedMamOriginError::RequestTooLarge);
        }
        Ok(Self {
            archive: archive.clone(),
            origin: incoming.origin_id.clone(),
            from: incoming.from.clone(),
            sender_scope: sender.map(|sender| sender.jid.to_bare()),
            eligible: incoming.message_type == MessageType::Groupchat
                && !groupchat_subject_is_state_update(incoming),
        })
    }

    /// Run only after the same transaction has proved its pinned heap, index,
    /// collation and external mutation assumptions. This helper itself does not
    /// attest any of them and is not an append capability.
    pub async fn observe_on_postgres_connection_in_schema(
        &self,
        conn: &mut PgConnection,
        schema: &str,
    ) -> Result<Option<StanzaId>, BoundedMamOriginError> {
        let Some(origin) = &self.origin else {
            return Ok(None);
        };
        let relation = super::postgres_relation(Some(schema), "mam_messages");
        let sql = ORIGIN_QUERY.replace("__MAM_RELATION__", &relation);
        let rows = sqlx::query(&sql)
            .bind(self.archive.as_str())
            .bind(origin.as_str())
            .bind(CANDIDATES + 1)
            .bind(CANDIDATES)
            .bind(ROW_BYTES)
            .bind(TOTAL_BYTES)
            .fetch_all(conn)
            .await
            .map_err(BoundedMamOriginError::Query)?;
        let summary = rows.first().ok_or(BoundedMamOriginError::Decode)?;
        let count: i64 = summary
            .try_get(17)
            .map_err(|_| BoundedMamOriginError::Decode)?;
        let oversized: bool = summary
            .try_get(18)
            .map_err(|_| BoundedMamOriginError::Decode)?;
        if count > CANDIDATES {
            return Err(BoundedMamOriginError::CandidateOverflow);
        }
        if oversized {
            return Err(BoundedMamOriginError::RowTooLarge);
        }
        if count == 0 {
            return Ok(None);
        }
        if usize::try_from(count).ok() != Some(rows.len()) {
            return Err(BoundedMamOriginError::Decode);
        }
        for row in rows {
            let candidate =
                decode_postgres_message_row(&row).map_err(|_| BoundedMamOriginError::Decode)?;
            if origin_id_tombstone_match_parts(
                &candidate,
                self.origin.as_ref(),
                &self.from,
                self.sender_scope.as_ref(),
                self.eligible,
            ) {
                return Ok(Some(StanzaId::new(
                    candidate.id,
                    self.archive.clone().into(),
                )));
            }
        }
        Ok(None)
    }
}

// The candidate CTE materializes only tuple IDs and ordinals. Lengths are
// measured in the same statement snapshot before CASE permits any text to be
// returned. N+1 candidates and over-budget bytes mask every text field. This
// bounds client transfer, not the server's index scan, comparisons, or sort.
const ORIGIN_QUERY: &str = r#"
WITH candidate AS MATERIALIZED (
  SELECT ctid, archive_seq FROM ONLY __MAM_RELATION__
  WHERE room_jid=$1 AND origin_id=$2
  ORDER BY archive_seq ASC LIMIT $3
), measured AS MATERIALIZED (
  SELECT c.ctid, c.archive_seq,
    COALESCE(pg_catalog.octet_length(m.id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.room_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.from_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.to_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.body),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.stanza_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.thread_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.reply_to_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.reply_to_jid),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.origin_id),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.message_type),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.stanza_xml),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.rich_payload),0)::pg_catalog.int8 +
    COALESCE(pg_catalog.octet_length(m.parent_thread_id),0)::pg_catalog.int8 AS bytes
  FROM ONLY __MAM_RELATION__ m JOIN candidate c ON m.ctid=c.ctid
), summary AS MATERIALIZED (
  SELECT pg_catalog.count(*)::pg_catalog.int8 AS n,
         COALESCE(pg_catalog.bool_or(bytes>$5),false)
           OR COALESCE(pg_catalog.sum(bytes),0)::pg_catalog.int8>$6 AS oversized
  FROM measured
)
SELECT CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.id END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.room_jid END,
       m.timestamp,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.from_jid END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.to_jid END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.body END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.stanza_id END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.thread_id END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.reply_to_id END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.reply_to_jid END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.origin_id END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.message_type END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.stanza_xml END,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.rich_payload END,
       m.nickname_generation,
       CASE WHEN s.n<=$4 AND NOT s.oversized THEN m.parent_thread_id END,
       m.archive_seq, s.n, s.oversized
FROM summary s LEFT JOIN measured b ON true
LEFT JOIN ONLY __MAM_RELATION__ m ON m.ctid=b.ctid
ORDER BY b.archive_seq ASC
"#;
