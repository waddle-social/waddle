//! Additional plan-mode projections composed into the ingress transaction.
use super::{InboxRepository, IngressUowError, IngressUowTransaction, MamArchiveRepository};
use jid::BareJid;
use waddle_xmpp::{
    inbox::{ConversationKind, InboxEntry},
    ingress::{InboxProjectionMutation, MessageKey},
    mam::{ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone},
};
use waddle_xmpp_core::{mam::ThreadId, xep0359::StanzaId};

impl InboxRepository {
    pub async fn mark_read(
        tx: &mut IngressUowTransaction<'_>,
        owner: &BareJid,
        channel: &BareJid,
        thread: Option<&ThreadId>,
    ) -> Result<Option<InboxEntry>, IngressUowError> {
        Ok(
            crate::inbox::mark_read_in_transaction(tx.transaction_mut(), owner, channel, thread)
                .await?,
        )
    }
    pub async fn apply_call_thread(
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
        owner: &BareJid,
        mutation: &InboxProjectionMutation,
    ) -> Result<(), IngressUowError> {
        match mutation {
            InboxProjectionMutation::DirectCallThreadAnchor {
                peer,
                thread_id,
                archive_stanza_id,
                media,
                last_updated,
            } => {
                let entry = InboxEntry::new(
                    peer.clone(),
                    ConversationKind::Direct,
                    archive_stanza_id.id.clone(),
                    *last_updated,
                )
                .with_thread(thread_id.as_str())
                .with_call_thread(waddle_xmpp::xep::CallThreadKind::Dm, *media);
                Self::apply_once(tx, key, owner, entry, false).await?;
            }
            InboxProjectionMutation::DirectCallThreadEnded {
                peer,
                thread_id,
                ended,
                duration,
            } => {
                tx.transaction_mut().execute("UPDATE inbox_entries SET call_ended_at = ?, call_duration = ? WHERE user_jid = ? AND partner_jid = ? AND thread_id = ? AND kind = 'direct' AND call_thread_kind = 'dm' AND call_thread_media IS NOT NULL", crate::db_params![ended.timestamp(), duration.as_str().to_owned(), owner.to_string(), peer.to_string(), thread_id.as_str().to_owned()]).await?;
            }
            _ => return Err(IngressUowError::UnsupportedInboxProjection),
        }
        Ok(())
    }
}
impl MamArchiveRepository {
    pub async fn replace_with_tombstone(
        tx: &mut IngressUowTransaction<'_>,
        archive: &BareJid,
        target: &StanzaId,
        tombstone: &ArchivedTombstone,
    ) -> Result<(), IngressUowError> {
        // A sender retraction must not replace an already terminal moderation
        // tombstone. Lock before decoding so a concurrent moderator cannot
        // change the payload between this check and the update.
        let sql = match tx.transaction_mut().driver() {
            crate::db::DatabaseDriver::Postgres => {
                "SELECT rich_payload FROM mam_messages WHERE room_jid = ? AND id = ? FOR UPDATE"
            }
            crate::db::DatabaseDriver::Sqlite => {
                "SELECT rich_payload FROM mam_messages WHERE room_jid = ? AND id = ?"
            }
        };
        let mut rows = tx
            .transaction_mut()
            .query(
                sql,
                crate::db_params![archive.to_string(), target.id.clone()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(());
        };
        let stored: Option<String> = row.get(0)?;
        drop(rows);
        let current = stored
            .as_deref()
            .map(serde_json::from_str::<ArchivedRichMessage>)
            .transpose()
            .map_err(|_| IngressUowError::InvalidArchiveRichPayload)?;
        if current
            .as_ref()
            .is_some_and(ArchivedRichMessage::is_tombstoned)
        {
            return Ok(());
        }
        let rich = ArchivedRichMessage {
            payload: Some(ArchivedRichPayload::Tombstone(tombstone.clone())),
            reply: None,
            references: Vec::new(),
            mentions: Vec::new(),
            subjects: Default::default(),
            occupant_id: None,
            muc_sender: None,
        };
        let encoded =
            serde_json::to_string(&rich).map_err(|_| IngressUowError::TombstonePayloadEncoding)?;
        tx.transaction_mut().execute("UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ? WHERE room_jid = ? AND id = ?", crate::db_params![encoded, archive.to_string(), target.id.clone()]).await?;
        Ok(())
    }
}
