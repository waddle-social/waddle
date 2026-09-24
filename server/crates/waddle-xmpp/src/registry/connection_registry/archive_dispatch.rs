//! Serialize acceptance with a per-archive frontier, rather than retaining one
//! deduplication key for every message ever sent on a long-lived connection.

use std::collections::BTreeMap;

use tokio::sync::mpsc::error::TrySendError;

use super::{ConnectionEntry, ConnectionRegistry, OutboundStanza};
use crate::{mam::ArchiveOrdinal, stream_management::SmIngressAppendKey};

#[derive(Debug, Default)]
pub(super) struct ArchiveDispatchFrontiers {
    archives: BTreeMap<jid::BareJid, (ArchiveOrdinal, Vec<SmIngressAppendKey>)>,
}

impl ConnectionEntry {
    /// The caller has authorized the canonical positions. The non-blocking
    /// enqueue and frontier publication share one lock across registry/actor
    /// clones. Concurrent retries cannot enqueue A again after B is accepted.
    pub(crate) fn try_send_archive_ordered(
        &self,
        outbound: OutboundStanza,
    ) -> Result<(), TrySendError<()>> {
        if outbound.ingress_append.as_ref().is_some_and(|obligation| {
            [
                crate::ingress::IngressEffectKind::Carbons,
                crate::ingress::IngressEffectKind::RelayCarbons,
            ]
            .iter()
            .any(|kind| kind.storage_tag() == obligation.key.kind.to_storage())
        }) && !self.is_carbons_enabled()
        {
            // A frozen audience is only an upper bound. Check current opt-in
            // at queue acceptance, after any relay/database await and against
            // this exact connection entry. Like duplicate suppression below,
            // success settles the obligation without a new wire copy.
            return Ok(());
        }
        let Some(obligation) = outbound
            .ingress_append
            .as_ref()
            .filter(|obligation| !obligation.archive_positions.is_empty())
        else {
            return self.sender.try_send(outbound).map_err(queue_error);
        };
        let key = obligation.key.clone();
        if obligation
            .dispatch_stream
            .as_ref()
            .is_some_and(|stream| self.sm_stream_id().as_ref() != Some(stream))
        {
            // Backpressure, not "disconnected": the caller must recheck its
            // predecessor gate instead of falling through to a different stream.
            return Err(TrySendError::Full(()));
        }
        let positions = obligation.archive_positions.clone();
        let Ok(mut state) = self.archive_dispatch.lock() else {
            return Err(TrySendError::Full(()));
        };
        let duplicate = positions.iter().any(|position| {
            state
                .archives
                .get(&position.archive)
                .is_some_and(|(ordinal, keys)| {
                    *ordinal > position.ordinal
                        || (*ordinal == position.ordinal && keys.contains(&key))
                })
        });
        if duplicate {
            // Queue acceptance is sufficient for ordinary ingress delivery.
            // It cannot fabricate the separate write-acceptance callback.
            return if outbound.write_acceptance.is_some() {
                Err(TrySendError::Full(()))
            } else {
                Ok(())
            };
        }
        self.sender.try_send(outbound).map_err(queue_error)?;
        for position in positions {
            let entry = state
                .archives
                .entry(position.archive)
                .or_insert_with(|| (position.ordinal, Vec::new()));
            if entry.0 < position.ordinal {
                *entry = (position.ordinal, Vec::new());
            }
            if !entry.1.contains(&key) {
                entry.1.push(key.clone());
            }
        }
        Ok(())
    }
}

fn queue_error(error: TrySendError<OutboundStanza>) -> TrySendError<()> {
    match error {
        TrySendError::Full(_) => TrySendError::Full(()),
        TrySendError::Closed(_) => TrySendError::Closed(()),
    }
}

impl ConnectionRegistry {
    pub fn local_sm_stream(
        &self,
        resource: &jid::FullJid,
    ) -> Option<crate::pending_delivery::SmSessionId> {
        self.connections
            .get(resource)
            .filter(|entry| entry.is_locally_hosted())
            .and_then(|entry| entry.sm_stream_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ingress::MessageKey,
        registry::{BroadcastOutcome, ConnectionRegistry},
        stream_management::{
            ArchiveDispatchPosition, SmIngressReceiptKind, SmRelayedAppendObligation,
        },
        Stanza,
    };

    fn copy(resource: &jid::FullJid, key: MessageKey, ordinal: i64) -> OutboundStanza {
        let mut message = xmpp_parsers::message::Message::new(Some(resource.clone().into()));
        message.id = Some(xmpp_parsers::message::Id(ordinal.to_string()));
        OutboundStanza::new(Stanza::Message(message)).with_ingress_append(
            SmRelayedAppendObligation {
                key: SmIngressAppendKey {
                    message_key: key,
                    kind: SmIngressReceiptKind::from_storage(1),
                    semantic_identity_hash: [1; 32],
                    resource: resource.clone(),
                },
                sender_bare: "alice@example.test".parse().expect("valid sender bare JID"),
                received_at: None,
                archive_positions: vec![ArchiveDispatchPosition {
                    archive: resource.to_bare(),
                    ordinal: ArchiveOrdinal::from_storage(ordinal)
                        .expect("valid archive ordinal fixture"),
                }],
                dispatch_stream: None,
            },
        )
    }

    #[test]
    fn carbon_dispatch_checks_current_opt_in_with_or_without_archive_positions() {
        for kind in [
            crate::ingress::IngressEffectKind::Carbons,
            crate::ingress::IngressEffectKind::RelayCarbons,
        ] {
            for archived in [true, false] {
                let registry = ConnectionRegistry::new();
                let resource: jid::FullJid = "bob@example.test/phone"
                    .parse()
                    .expect("valid recipient resource JID");
                let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
                registry.register_with_carbons(resource.clone(), sender, true);
                let entry = registry
                    .get_entry(&resource)
                    .expect("registered recipient resource");
                let mut outbound = copy(&resource, MessageKey::new(), 1);
                let obligation = outbound
                    .ingress_append
                    .as_mut()
                    .expect("outbound fixture has an ingress append obligation");
                obligation.key.kind = SmIngressReceiptKind::from_storage(kind.storage_tag());
                if !archived {
                    obligation.archive_positions.clear();
                }
                registry.set_carbons_enabled(&resource, false);
                assert!(
                    entry.try_send_archive_ordered(outbound).is_ok(),
                    "confirmed opt-out settles the carbon"
                );
                assert!(
                    receiver.try_recv().is_err(),
                    "opted-out resource gets no carbon copy"
                );
                assert!(
                    entry
                        .archive_dispatch
                        .lock()
                        .expect("archive dispatch mutex is not poisoned")
                        .archives
                        .is_empty(),
                    "suppression must not fabricate queue acceptance"
                );
            }
        }
    }

    #[tokio::test]
    async fn late_retry_cannot_enqueue_behind_a_newer_archive_position() {
        let registry = ConnectionRegistry::new();
        let resource: jid::FullJid = "bob@example.test/phone"
            .parse()
            .expect("valid recipient resource JID");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), sender);
        let owner = registry
            .get_entry(&resource)
            .expect("registered recipient resource")
            .carbons_enabled;
        let a = copy(&resource, MessageKey::new(), 1);
        let b = copy(&resource, MessageKey::new(), 2);
        for outbound in [a.clone(), b, a] {
            assert_eq!(
                registry.try_send_outbound_if_owner(&resource, &owner, outbound),
                BroadcastOutcome::Delivered
            );
        }
        for expected in ["1", "2"] {
            let Stanza::Message(message) = receiver
                .recv()
                .await
                .expect("receive accepted archive delivery")
                .stanza
            else {
                panic!("message")
            };
            assert_eq!(message.id.as_ref().map(|id| id.0.as_str()), Some(expected));
        }
        assert!(
            receiver.try_recv().is_err(),
            "late duplicate must not reach the wire queue"
        );
    }

    #[tokio::test]
    async fn full_channel_does_not_publish_a_false_acceptance_frontier() {
        let registry = ConnectionRegistry::new();
        let resource: jid::FullJid = "bob@example.test/phone"
            .parse()
            .expect("valid recipient resource JID");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        registry.register(resource.clone(), sender);
        let owner = registry
            .get_entry(&resource)
            .expect("registered recipient resource")
            .carbons_enabled;
        let a = copy(&resource, MessageKey::new(), 1);
        let b = copy(&resource, MessageKey::new(), 2);
        assert_eq!(
            registry.try_send_outbound_if_owner(&resource, &owner, a),
            BroadcastOutcome::Delivered
        );
        assert_eq!(
            registry.try_send_outbound_if_owner(&resource, &owner, b.clone()),
            BroadcastOutcome::DroppedFull
        );
        receiver
            .recv()
            .await
            .expect("receive first queued archive delivery");
        assert_eq!(
            registry.try_send_outbound_if_owner(&resource, &owner, b),
            BroadcastOutcome::Delivered
        );
        assert!(receiver.recv().await.is_some());
    }

    #[tokio::test]
    async fn distinct_receipts_at_one_position_are_each_deliverable_once() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let entry = ConnectionEntry::new(sender);
        let resource = "bob@example.test/phone"
            .parse()
            .expect("valid recipient resource JID");
        let a = copy(&resource, MessageKey::new(), 1);
        let mut other_receipt = a.clone();
        other_receipt
            .ingress_append
            .as_mut()
            .expect("outbound fixture has an ingress append obligation")
            .key
            .semantic_identity_hash = [2; 32];
        for outbound in [a.clone(), other_receipt.clone(), a, other_receipt] {
            entry
                .try_send_archive_ordered(outbound)
                .expect("enqueue archive-ordered receipt");
        }
        assert!(receiver.recv().await.is_some());
        assert!(receiver.recv().await.is_some());
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn pending_order_exemption_cannot_move_to_a_replacement_stream() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let entry = ConnectionEntry::new(sender);
        let old_stream = crate::pending_delivery::SmSessionId::new("old");
        entry.set_sm_stream_id(Some(crate::pending_delivery::SmSessionId::new(
            "replacement",
        )));
        let resource = "bob@example.test/phone"
            .parse()
            .expect("valid recipient resource JID");
        let mut outbound = copy(&resource, MessageKey::new(), 2);
        outbound
            .ingress_append
            .as_mut()
            .expect("outbound fixture has an ingress append obligation")
            .dispatch_stream = Some(old_stream);
        assert!(matches!(
            entry.try_send_archive_ordered(outbound),
            Err(TrySendError::Full(_))
        ));
        assert!(receiver.try_recv().is_err());
    }
}
