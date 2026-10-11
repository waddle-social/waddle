//! Host-only ordering of XEP-0357 backing items. This projection metadata is
//! deliberately independent of canonical delivery receipts and of item lifetime.
use jid::BareJid;
use waddle_xmpp::pubsub::{
    PubSubItem, PublicationError, PublicationFingerprint, PublicationNode, PublicationVersion,
    PublishResult, VersionedPublishResult,
};
use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;

fn storage(error: crate::db::DatabaseError) -> PublicationError {
    PublicationError::Storage(XmppError::internal(error.to_string()))
}

impl DatabasePubSubStorage {
    /// Additive projection metadata, independent of the core PubSub schema
    /// version and its reset policy. No FK: item/node deletion must preserve it.
    pub(super) async fn initialize_publication_watermarks(&self) -> Result<(), XmppError> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS pubsub_push_publications (
                service_jid TEXT NOT NULL, node_name TEXT NOT NULL,
                revision BIGINT NOT NULL CHECK (revision > 0),
                job_token TEXT NOT NULL, payload_hash TEXT NOT NULL,
                PRIMARY KEY (service_jid, node_name)
            )",
            (),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn publish_push_item_versioned_impl(
        &self,
        service: &BareJid,
        publisher: &BareJid,
        node: &PublicationNode,
        item: &PubSubItem,
        version: PublicationVersion,
    ) -> Result<VersionedPublishResult, PublicationError> {
        let fingerprint = PublicationFingerprint::of(item, publisher)?;
        let payload_hash = hex::encode(fingerprint.to_storage_bytes());
        let item_id = item
            .id
            .as_ref()
            .filter(|id| !id.trim().is_empty())
            .ok_or(PublicationError::InvalidPublication)?;
        let payload = item
            .payload
            .as_ref()
            .filter(|payload| {
                payload.name() == "notification"
                    && payload.ns() == waddle_xmpp::xep::xep0357::NS_PUSH
            })
            .ok_or(PublicationError::InvalidPublication)?;
        let config = self
            .get_node_impl(service, node.as_str())
            .await?
            .ok_or_else(|| {
                XmppError::item_not_found(Some("Push backing node does not exist".to_owned()))
            })?
            .config;
        let payload_xml = String::from(payload);
        let revision =
            i64::try_from(version.revision()).map_err(|_| PublicationError::InvalidPublication)?;
        let token = version.job_token().to_string();
        let service_key = service.to_string();
        let publisher = publisher.to_string();
        let mut tx = self.db.begin_immediate().await.map_err(storage)?;
        // This conditional upsert is the first write and takes PostgreSQL's
        // conflicting row lock (SQLite already owns its writer lock). A newer
        // publication cannot commit its payload between this stamp and ours.
        let applied = tx
            .execute(
                "INSERT INTO pubsub_push_publications \
             (service_jid, node_name, revision, job_token, payload_hash) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(service_jid, node_name) DO UPDATE SET \
               revision = excluded.revision, job_token = excluded.job_token, \
               payload_hash = excluded.payload_hash \
             WHERE pubsub_push_publications.revision < excluded.revision",
                crate::db_params![
                    service_key.clone(),
                    node.as_str(),
                    revision,
                    token.clone(),
                    payload_hash.clone()
                ],
            )
            .await
            .map_err(storage)?;
        if applied == 0 {
            let mut rows = tx
                .query(
                    "SELECT revision, job_token, payload_hash \
                 FROM pubsub_push_publications WHERE service_jid = ? AND node_name = ?",
                    crate::db_params![service_key.clone(), node.as_str()],
                )
                .await
                .map_err(storage)?;
            let row = rows
                .next()
                .await
                .map_err(storage)?
                .ok_or(PublicationError::IntegrityConflict)?;
            let previous: i64 = row.get(0).map_err(storage)?;
            if previous > revision {
                tx.commit().await.map_err(storage)?;
                return Ok(VersionedPublishResult::Superseded);
            }
            let previous_token: String = row.get(1).map_err(storage)?;
            let previous_hash: String = row.get(2).map_err(storage)?;
            if previous != revision || previous_token != token || previous_hash != payload_hash {
                return Err(PublicationError::IntegrityConflict);
            }
            tx.commit().await.map_err(storage)?;
            return Ok(VersionedPublishResult::AlreadyApplied);
        }
        tx.execute(
            "INSERT INTO pubsub_items \
             (owner_jid, node_name, item_id, payload_xml, publisher_jid, published_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(owner_jid, node_name, item_id) DO UPDATE SET \
               seq = excluded.seq, payload_xml = excluded.payload_xml, \
               publisher_jid = excluded.publisher_jid, published_at_ms = excluded.published_at_ms",
            crate::db_params![
                service_key.clone(),
                node.as_str(),
                item_id.clone(),
                payload_xml,
                publisher,
                crate::time::now_ms()
            ],
        )
        .await
        .map_err(storage)?;
        let evicted_item_ids = self
            .enforce_max_items_tx(&mut tx, service, node.as_str(), config.max_items)
            .await?;
        tx.commit().await.map_err(storage)?;
        Ok(VersionedPublishResult::Applied(PublishResult {
            item_id: item_id.clone(),
            node_created: false,
            evicted_item_ids,
        }))
    }
}
