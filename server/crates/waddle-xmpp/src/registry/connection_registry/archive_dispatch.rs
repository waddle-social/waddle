//! Serialize acceptance with a per-archive frontier, rather than retaining one
//! deduplication key for every message ever sent on a long-lived connection.

use crate::ingress::IngressEffectKind;
use crate::pending_delivery::SmSessionId;
use std::collections::BTreeMap;

use super::endpoint::{EndpointEnqueueOutcome, EndpointEnqueueRejection};
use super::{ConnectionEntry, ConnectionRegistry, OutboundStanza};
use crate::mam::ArchiveOrdinal;
use crate::stream_management::SmIngressAppendKey;

/// Why an archive-aware local enqueue was proven not to accept its envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveDispatchRefusalReason {
    Provisional,
    Sealed,
    Full,
    ChannelClosed,
    BindingMismatch,
    ArchiveOrderMismatch,
    ArchiveFrontierUnavailable,
    DuplicateRequiresWriteAcceptance,
}

/// Exact envelope returned from a local archive dispatch that did not enqueue.
#[derive(Debug)]
pub struct ArchiveDispatchRefusal {
    pub reason: ArchiveDispatchRefusalReason,
    pub outbound: Box<OutboundStanza>,
}

impl ArchiveDispatchRefusal {
    fn new(reason: ArchiveDispatchRefusalReason, outbound: OutboundStanza) -> Self {
        Self {
            reason,
            outbound: Box::new(outbound),
        }
    }
}

impl From<EndpointEnqueueRejection> for ArchiveDispatchRefusalReason {
    fn from(reason: EndpointEnqueueRejection) -> Self {
        match reason {
            EndpointEnqueueRejection::Provisional => Self::Provisional,
            EndpointEnqueueRejection::Sealed => Self::Sealed,
            EndpointEnqueueRejection::Full => Self::Full,
            EndpointEnqueueRejection::ChannelClosed => Self::ChannelClosed,
            EndpointEnqueueRejection::BindingMismatch => Self::BindingMismatch,
        }
    }
}

impl ArchiveDispatchRefusalReason {
    fn as_endpoint_rejection(self) -> EndpointEnqueueRejection {
        match self {
            Self::Provisional => EndpointEnqueueRejection::Provisional,
            Self::Sealed => EndpointEnqueueRejection::Sealed,
            Self::Full
            | Self::ArchiveOrderMismatch
            | Self::ArchiveFrontierUnavailable
            | Self::DuplicateRequiresWriteAcceptance => EndpointEnqueueRejection::Full,
            Self::ChannelClosed => EndpointEnqueueRejection::ChannelClosed,
            Self::BindingMismatch => EndpointEnqueueRejection::BindingMismatch,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct ArchiveDispatchFrontiers {
    archives: BTreeMap<jid::BareJid, (ArchiveOrdinal, Vec<SmIngressAppendKey>)>,
}

impl ConnectionEntry {
    /// The caller has authorized the canonical positions. The non-blocking
    /// enqueue and frontier publication share one lock across registry/actor
    /// clones. Concurrent retries cannot enqueue A again after B is accepted.
    pub(in crate::registry) fn try_send_archive_ordered(
        &self,
        outbound: OutboundStanza,
    ) -> Result<(), EndpointEnqueueRejection> {
        self.try_send_archive_ordered_with_refusal(outbound)
            .map_err(|refusal| refusal.reason.as_endpoint_rejection())
    }

    /// Attempt a non-blocking archive-aware enqueue while retaining the exact
    /// outbound envelope whenever the local endpoint proves it was not
    /// accepted. The caller remains responsible for binding this refusal to
    /// the local owner and `BindingRef` before exposing retry custody.
    pub(in crate::registry) fn try_send_archive_ordered_with_refusal(
        &self,
        outbound: OutboundStanza,
    ) -> Result<(), ArchiveDispatchRefusal> {
        if outbound.ingress_append.as_ref().is_some_and(|obligation| {
            [IngressEffectKind::Carbons, IngressEffectKind::RelayCarbons]
                .iter()
                .any(|kind| kind.storage_tag() == obligation.key.kind.to_storage())
        }) {
            // Suppression settles the obligation without a queue write, so it
            // must still be ordered against activation and sealing. Check
            // current opt-in while holding the same admission gate as enqueue.
            let suppressed = match self.endpoint.with_active(|_| !self.is_carbons_enabled()) {
                Ok(suppressed) => suppressed,
                Err(reason) => {
                    return Err(ArchiveDispatchRefusal::new(reason.into(), outbound));
                }
            };
            if suppressed {
                return Ok(());
            }
        }
        let Some(obligation) = outbound
            .ingress_append
            .as_ref()
            .filter(|obligation| !obligation.archive_positions.is_empty())
        else {
            return match self.endpoint.try_send(outbound) {
                EndpointEnqueueOutcome::AcceptedOrUnknown => Ok(()),
                EndpointEnqueueOutcome::ProvenNotEnqueued { reason, outbound } => {
                    Err(ArchiveDispatchRefusal::new(reason.into(), *outbound))
                }
            };
        };
        let key = obligation.key.clone();
        if obligation
            .dispatch_stream
            .as_ref()
            .is_some_and(|stream| self.sm_stream_id().as_ref() != Some(stream))
        {
            // Backpressure, not "disconnected": the caller must recheck its
            // predecessor gate instead of falling through to a different stream.
            return Err(ArchiveDispatchRefusal::new(
                ArchiveDispatchRefusalReason::ArchiveOrderMismatch,
                outbound,
            ));
        }
        let positions = obligation.archive_positions.clone();
        self.endpoint.with_active_outbound(
            outbound,
            |reason, outbound| ArchiveDispatchRefusal::new(reason.into(), outbound),
            |sink, outbound| {
                let Ok(mut state) = self.archive_dispatch.lock() else {
                    return Err(ArchiveDispatchRefusal::new(
                        ArchiveDispatchRefusalReason::ArchiveFrontierUnavailable,
                        outbound,
                    ));
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
                        Err(ArchiveDispatchRefusal::new(
                            ArchiveDispatchRefusalReason::DuplicateRequiresWriteAcceptance,
                            outbound,
                        ))
                    } else {
                        Ok(())
                    };
                }
                match sink.try_send(outbound) {
                    EndpointEnqueueOutcome::AcceptedOrUnknown => {}
                    EndpointEnqueueOutcome::ProvenNotEnqueued { reason, outbound } => {
                        return Err(ArchiveDispatchRefusal::new(reason.into(), *outbound));
                    }
                }
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
            },
        )
    }
}

impl ConnectionRegistry {
    pub fn local_sm_stream(&self, resource: &jid::FullJid) -> Option<SmSessionId> {
        self.connections
            .get(resource)
            .filter(|entry| entry.is_locally_hosted())
            .and_then(|entry| entry.sm_stream_id())
    }
}

#[cfg(test)]
mod tests {
    use super::EndpointEnqueueRejection;
    use super::*;
    use crate::ingress::MessageKey;
    use crate::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
    use crate::registry::{BroadcastOutcome, ConnectionRegistry};
    use crate::stream_management::{
        ArchiveDispatchPosition, SmIngressReceiptKind, SmRelayedAppendObligation,
    };
    use crate::Stanza;

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
    fn sealed_binding_refusal_does_not_advance_archive_frontier() {
        let resource: jid::FullJid = "bob@example.test/phone"
            .parse()
            .expect("valid recipient resource JID");
        let (endpoint, mut receiver) = super::super::SocketOutboundEndpoint::channel(1);
        let binding_ref =
            BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
        let entry = ConnectionEntry::new_for_binding(endpoint, binding_ref.clone());
        let owner = entry.binding_id();

        assert!(entry.seal_binding(owner, &binding_ref));
        assert_eq!(
            entry.try_send_archive_ordered(copy(&resource, MessageKey::new(), 1)),
            Err(EndpointEnqueueRejection::Sealed)
        );
        assert!(
            entry
                .archive_dispatch
                .lock()
                .expect("archive dispatch mutex is not poisoned")
                .archives
                .is_empty(),
            "refused enqueue must not publish an archive frontier"
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn sealed_binding_refusal_returns_exact_envelope_without_queue_or_frontier_change() {
        let resource: jid::FullJid = "bob@example.test/phone"
            .parse()
            .expect("valid recipient resource JID");
        let (endpoint, mut receiver) = super::super::SocketOutboundEndpoint::channel(1);
        let binding_ref =
            BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
        let entry = ConnectionEntry::new_for_binding(endpoint, binding_ref.clone());
        let owner = entry.binding_id();
        let (acceptance, mut acceptance_rx) = crate::registry::OutboundWriteAcceptance::new();
        let mut outbound = copy(&resource, MessageKey::new(), 1);
        outbound.pending_row_id = Some(crate::pending_delivery::PendingRowId::new("retry-row"));
        outbound.original_receipt_at = Some(chrono::Utc::now());
        outbound.write_acceptance = Some(acceptance);
        let expected_ingress = outbound
            .ingress_append
            .clone()
            .expect("archive dispatch envelope has ingress identity");
        let expected_row = outbound.pending_row_id.clone();
        let expected_receipt_at = outbound.original_receipt_at;
        assert!(entry.seal_binding(owner, &binding_ref));

        let refusal = entry
            .try_send_archive_ordered_with_refusal(outbound)
            .expect_err("sealed admission must return the untouched envelope");
        assert_eq!(refusal.reason, ArchiveDispatchRefusalReason::Sealed);
        assert_eq!(refusal.outbound.pending_row_id, expected_row);
        assert_eq!(refusal.outbound.original_receipt_at, expected_receipt_at);
        assert_eq!(refusal.outbound.ingress_append, Some(expected_ingress));
        assert!(
            receiver.try_recv().is_err(),
            "refusal must leave the queue empty"
        );
        assert!(
            entry
                .archive_dispatch
                .lock()
                .expect("archive dispatch mutex is not poisoned")
                .archives
                .is_empty(),
            "refusal must leave the archive frontier unchanged"
        );
        assert!(matches!(
            acceptance_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        refusal
            .outbound
            .write_acceptance
            .as_ref()
            .expect("callback stays attached to the original envelope")
            .acknowledge();
        acceptance_rx
            .await
            .expect("returned callback remains the original callback");
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
                    .entry(&resource)
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
        let (endpoint, mut receiver) = super::super::SocketOutboundEndpoint::channel(8);
        registry.register(resource.clone(), endpoint);
        let owner = registry
            .entry(&resource)
            .expect("registered recipient resource")
            .binding_id();
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
        let (endpoint, mut receiver) = super::super::SocketOutboundEndpoint::channel(1);
        registry.register(resource.clone(), endpoint);
        let owner = registry
            .entry(&resource)
            .expect("registered recipient resource")
            .binding_id();
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
            Err(EndpointEnqueueRejection::Full)
        ));
        assert!(receiver.try_recv().is_err());
    }
}
