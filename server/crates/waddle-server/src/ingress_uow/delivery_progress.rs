//! Transaction-scoped progress beneath an exact recorded delivery obligation.
use jid::FullJid;
use waddle_xmpp::ingress::MessageKey;

use super::{IngressUowError, IngressUowTransaction};
use crate::{
    db::DatabaseDriver,
    ingress::{decision::EffectReceiptKey, room_manifest::RoomManifestChildDisposition},
    ingress_substrate::EffectReceiptKind,
};

pub(crate) struct DeliveryProgressRepository;

/// Resolves an original child without asserting socket delivery or SM custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryPolicyDiscardReason {
    OccupancyDeparted,
    OccupancySuperseded,
    OccupancyIncarnationLost,
    RoomRetired,
}

impl DeliveryPolicyDiscardReason {
    pub(crate) fn as_storage(self) -> &'static str {
        match self {
            Self::OccupancyDeparted => "occupancy_departed",
            Self::OccupancySuperseded => "occupancy_superseded",
            Self::OccupancyIncarnationLost => "occupancy_incarnation_lost",
            Self::RoomRetired => "room_retired",
        }
    }
}

impl DeliveryProgressRepository {
    pub(crate) async fn load(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<FullJid>, IngressUowError> {
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "SELECT resource FROM ingress_delivery_receipts WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ORDER BY resource"
        } else {
            "SELECT resource FROM ingress_delivery_receipts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? ORDER BY resource"
        };
        let mut rows = tx
            .transaction_mut()
            .query(
                sql,
                crate::db_params![
                    message.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec()
                ],
            )
            .await?;
        let mut recipients = Vec::new();
        while let Some(row) = rows.next().await? {
            let recipient: String = row.get(0)?;
            recipients.push(
                recipient
                    .parse()
                    .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?,
            );
        }
        drop(rows);
        for (policy_receipt, target) in Self::no_transport_children(tx, message).await? {
            if &policy_receipt == receipt {
                recipients.push(target);
            }
        }
        recipients.sort();
        recipients.dedup();
        Ok(recipients)
    }

    /// Settled resources for the row, grouped by delivery obligation, including
    /// sealed room-host children that require no transport. Bulk reads avoid
    /// a query per route: a lost groupchat row carries one route per inbox owner,
    /// and a per-route read would spend the recovery row deadline before any
    /// recoverable work on it could run.
    pub(crate) async fn load_all(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
    ) -> Result<Vec<(EffectReceiptKey, Vec<FullJid>)>, IngressUowError> {
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "SELECT kind, semantic_identity_hash, resource FROM ingress_delivery_receipts WHERE message_key = ?::uuid ORDER BY kind, semantic_identity_hash, resource"
        } else {
            "SELECT kind, semantic_identity_hash, resource FROM ingress_delivery_receipts WHERE message_key = ? ORDER BY kind, semantic_identity_hash, resource"
        };
        let mut rows = tx
            .transaction_mut()
            .query(sql, crate::db_params![message.to_storage().to_string()])
            .await?;
        let mut progress: Vec<(EffectReceiptKey, Vec<FullJid>)> = Vec::new();
        while let Some(row) = rows.next().await? {
            let kind: i64 = row.get(0)?;
            let hash: Vec<u8> = row.get(1)?;
            let resource: String = row.get(2)?;
            let receipt = EffectReceiptKey {
                kind: EffectReceiptKind::from_storage(
                    i32::try_from(kind).map_err(|_| IngressUowError::InvalidStoredReceiptHash)?,
                ),
                semantic_identity_hash: hash
                    .try_into()
                    .map_err(|_| IngressUowError::InvalidStoredReceiptHash)?,
            };
            let recipient = resource
                .parse()
                .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?;
            match progress.last_mut() {
                Some((last, recipients)) if *last == receipt => recipients.push(recipient),
                _ => progress.push((receipt, vec![recipient])),
            }
        }
        drop(rows);
        for (receipt, target) in Self::no_transport_children(tx, message).await? {
            match progress.iter_mut().find(|(stored, _)| *stored == receipt) {
                Some((_, recipients)) => recipients.push(target),
                None => progress.push((receipt, vec![target])),
            }
        }
        progress.sort_by_key(|(receipt, _)| {
            (receipt.kind.to_storage(), receipt.semantic_identity_hash)
        });
        for (_, recipients) in &mut progress {
            recipients.sort();
            recipients.dedup();
        }
        Ok(progress)
    }

    /// Frozen room-host policy settles only these logical children. It never
    /// fabricates transport delivery rows, send completion, or aggregate receipts.
    async fn no_transport_children(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
    ) -> Result<Vec<(EffectReceiptKey, FullJid)>, IngressUowError> {
        let Some(envelope) = super::CanonicalMessageRepository::load_envelope(tx, message).await?
        else {
            return Ok(Vec::new());
        };
        let Some(manifest) = envelope.room_manifest() else {
            return Ok(Vec::new());
        };
        Ok(manifest
            .children
            .iter()
            .filter(|child| {
                child.disposition == RoomManifestChildDisposition::HostRoomCopyRequiresNoTransport
            })
            .filter_map(|child| {
                child
                    .target
                    .as_ref()
                    .map(|target| (child.receipt.clone(), target.clone()))
            })
            .collect())
    }

    pub(crate) async fn record(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
        receipt: &EffectReceiptKey,
        recipients: &[FullJid],
    ) -> Result<(), IngressUowError> {
        if recipients.is_empty() {
            return Ok(());
        }
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "INSERT INTO ingress_delivery_receipts (message_key, kind, semantic_identity_hash, resource) VALUES (?::uuid, ?, ?, ?) ON CONFLICT DO NOTHING"
        } else {
            "INSERT INTO ingress_delivery_receipts (message_key, kind, semantic_identity_hash, resource) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING"
        };
        for recipient in recipients {
            tx.transaction_mut()
                .execute(
                    sql,
                    crate::db_params![
                        message.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        recipient.to_string()
                    ],
                )
                .await?;
        }
        Ok(())
    }

    /// Existing custody or policy resolution always wins. Never relabel a
    /// transport receipt when later evidence retires the original occupancy.
    pub(crate) async fn record_policy_discard(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
        receipt: &EffectReceiptKey,
        recipients: &[FullJid],
        reason: DeliveryPolicyDiscardReason,
    ) -> Result<(), IngressUowError> {
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "INSERT INTO ingress_delivery_receipts (message_key, kind, semantic_identity_hash, resource, policy_discard_reason) VALUES (?::uuid, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
        } else {
            "INSERT INTO ingress_delivery_receipts (message_key, kind, semantic_identity_hash, resource, policy_discard_reason) VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
        };
        for recipient in recipients {
            tx.transaction_mut()
                .execute(
                    sql,
                    crate::db_params![
                        message.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        recipient.to_string(),
                        reason.as_storage(),
                    ],
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::LineageConfig,
        db::{Database, MigrationRunner},
        ingress::{FrozenRoomManifest, RoomManifestSnapshot},
        ingress_substrate::MessageEnvelope,
        ingress_uow::{
            CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository,
            IngressUnitOfWork, SendAttemptRepository, SendObligation,
        },
    };
    use waddle_xmpp::{
        ingress::{EffectMessageIdentity, EntityGeneration, IngressEffectIntent, SemanticDigest},
        muc::{durable::AdminMutationId, room_actor::OccupancyWatermark},
    };
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::Message;

    #[tokio::test]
    async fn xep0045_frozen_host_policy_settles_only_its_child_without_transport_or_aggregate_receipts(
    ) {
        let db = Database::in_memory("host-room-progress")
            .await
            .expect("database");
        MigrationRunner::single().run(&db).await.expect("migrate");
        let uow = IngressUnitOfWork::open(
            db,
            LineageConfig {
                deployment_uuid: None,
                action: None,
            },
        )
        .expect("uow");
        let room: jid::BareJid = "room@muc.example.test".parse().expect("room");
        let sender: FullJid = "sender@example.test/phone".parse().expect("sender");
        let human: FullJid = "human@example.test/phone".parse().expect("human");
        let bot: FullJid = "bot@example.test/host".parse().expect("bot");
        let key = MessageKey::new();
        let stamp = StanzaId::new("sealed", room.clone().into());
        let route = IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![sender.clone(), human.clone(), bot.clone()],
            reflection: sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(stamp.clone()),
        };
        let receipt = crate::ingress::receipt_key(&route).expect("receipt");
        let mut snapshot = RoomManifestSnapshot {
            coordinates: None,
            occupancy_incarnation: AdminMutationId::generate(),
            occupants: [sender.clone(), human.clone(), bot.clone()]
                .into_iter()
                .enumerate()
                .map(|(index, target)| (target, OccupancyWatermark::from_revision(index as u64)))
                .collect(),
            host_no_transport: vec![bot.clone(), sender.clone()],
        };
        let manifest = FrozenRoomManifest::build(
            key,
            room.clone(),
            stamp.clone(),
            &snapshot,
            None,
            std::slice::from_ref(&route),
        )
        .expect("manifest");
        assert_eq!(
            manifest
                .child(&receipt, Some(&sender))
                .expect("reflection child")
                .disposition,
            RoomManifestChildDisposition::Delivery
        );
        let mut message = Message::new(Some(room.clone().into()));
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        message.from = Some(room.with_resource_str("sender").expect("nick").into());
        waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &stamp);
        let envelope = MessageEnvelope::new(message);
        let mut tx = uow.begin().await.expect("begin acceptance");
        CanonicalMessageRepository::record_message(
            &mut tx,
            key,
            &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
            Some(&envelope),
        )
        .await
        .expect("canonical");
        EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&route), false)
            .await
            .expect("route authority");
        CanonicalMessageRepository::record_room_canonical_envelope(
            &mut tx,
            key,
            &envelope.clone().with_room_manifest(manifest).expect("seal"),
        )
        .await
        .expect("store sealed policy");
        tx.commit().await.expect("acceptance commit");
        let mut tx = uow.begin().await.expect("read progress");
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("logical host progress"),
            vec![bot.clone()]
        );
        assert_eq!(
            DeliveryProgressRepository::load_all(&mut tx, key)
                .await
                .expect("all logical progress"),
            vec![(receipt.clone(), vec![bot.clone()])]
        );
        assert!(
            SendAttemptRepository::status(
                &mut tx,
                &SendObligation {
                    message: key,
                    receipt: receipt.clone(),
                    recipient: bot.clone()
                }
            )
            .await
            .expect("host send status")
            .is_none(),
            "no fabricated host transport acceptance"
        );
        assert!(
            !EffectReceiptRepository::contains(
                &mut tx,
                key,
                receipt.kind,
                &receipt.semantic_identity_hash
            )
            .await
            .expect("aggregate receipt"),
            "missing human and reflection remain owed"
        );
        let mut rows = tx
            .transaction_mut()
            .query(
                "SELECT COUNT(*) FROM ingress_delivery_receipts WHERE message_key = ?",
                crate::db_params![key.to_storage().to_string()],
            )
            .await
            .expect("physical delivery rows");
        let count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("count row")
            .get(0)
            .expect("count");
        assert_eq!(
            count, 0,
            "policy is envelope metadata, never a fake delivery row"
        );
        drop(rows);
        DeliveryProgressRepository::record(&mut tx, key, &receipt, std::slice::from_ref(&human))
            .await
            .expect("existing delivered child");
        DeliveryProgressRepository::record_policy_discard(
            &mut tx,
            key,
            &receipt,
            std::slice::from_ref(&human),
            DeliveryPolicyDiscardReason::OccupancyDeparted,
        )
        .await
        .expect("later departure cannot replace delivery");
        DeliveryProgressRepository::record_policy_discard(
            &mut tx,
            key,
            &receipt,
            std::slice::from_ref(&sender),
            DeliveryPolicyDiscardReason::RoomRetired,
        )
        .await
        .expect("original reflection retirement");
        DeliveryProgressRepository::record(&mut tx, key, &receipt, std::slice::from_ref(&sender))
            .await
            .expect("late progress cannot relabel retirement");
        let mut rows = tx.transaction_mut().query(
            "SELECT resource, policy_discard_reason FROM ingress_delivery_receipts WHERE message_key = ? ORDER BY resource",
            crate::db_params![key.to_storage().to_string()],
        ).await.expect("dispositions");
        let row = rows.next().await.expect("delivered row").expect("human");
        assert_eq!(row.get::<String>(0).expect("resource"), human.to_string());
        assert_eq!(
            row.get::<Option<String>>(1).expect("reason"),
            None,
            "proven custody stays transport completion"
        );
        let row = rows.next().await.expect("retired row").expect("sender");
        assert_eq!(row.get::<String>(0).expect("resource"), sender.to_string());
        assert_eq!(
            row.get::<Option<String>>(1).expect("reason").as_deref(),
            Some("room_retired"),
            "policy resolution stays distinct from transport completion"
        );
        drop(rows);
        let mut expected = vec![bot.clone(), human, sender];
        expected.sort();
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("all resolved"),
            expected
        );
        snapshot.host_no_transport.clear();
        let changed = FrozenRoomManifest::build(key, room, stamp, &snapshot, None, &[route])
            .expect("new host configuration");
        assert!(
            CanonicalMessageRepository::record_room_canonical_envelope(
                &mut tx,
                key,
                &envelope.with_room_manifest(changed).expect("new snapshot")
            )
            .await
            .is_err(),
            "later host configuration cannot rewrite original policy"
        );
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("policy unchanged"),
            vec![bot]
        );
        tx.commit().await.expect("read commit");
    }
}
