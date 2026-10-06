//! RFC 0018 §2 and XEP-0198 §4: unavailable ownership cannot accept responsibility.
use super::super::{
    effects::{
        delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        Effect, ExternalEffect, PlanFailure,
    },
    Deps, OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind,
};
use super::plan_message_dispatch;
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
    IngressStreamIdentity,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use waddle_xmpp::{
    ownership::*,
    protocol::{StanzaDispatcher, XmppStateMachine},
};

pub(crate) struct PlanningClaims {
    owner: std::sync::Mutex<NodeIdentity>,
    failed: AtomicBool,
    stale: AtomicBool,
}
impl PlanningClaims {
    pub(crate) fn new(owner: NodeIdentity) -> Self {
        Self {
            owner: std::sync::Mutex::new(owner),
            failed: AtomicBool::new(false),
            stale: AtomicBool::new(false),
        }
    }
    /// Moves every claim, as a demotion and reacquisition would.
    pub(crate) fn set_owner(&self, owner: NodeIdentity) {
        *self.owner.lock().expect("planning owner") = owner;
    }
    pub(crate) fn fail_reads(&self, value: bool) {
        self.failed.store(value, Ordering::SeqCst);
    }
    pub(crate) fn set_stale(&self, value: bool) {
        self.stale.store(value, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl ClaimStore for PlanningClaims {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        Ok(())
    }
    async fn acquire(&self, _: &Entity, _: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("planning is read-only")
    }
    async fn ensure_claimed(&self, _: &Entity, _: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("planning is read-only")
    }
    async fn steal_stale(
        &self,
        _: &Entity,
        _: ClaimEpoch,
        _: StalePredicate,
        _: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("planning is read-only")
    }
    async fn steal_for_resume(
        &self,
        _: &Entity,
        _: ClaimEpoch,
        _: ResumeIdentityProof,
        _: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("planning is read-only")
    }
    async fn current_claim(&self, _: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(ClaimError::Backend("ownership fixture unavailable".into()));
        }
        Ok(Some(ClaimSnapshot {
            owner: self.owner.lock().expect("planning owner").clone(),
            claim_epoch: ClaimEpoch(1),
            owner_lease_fresh: !self.stale.load(Ordering::SeqCst),
        }))
    }
    async fn fence(&self, _: &Entity, _: &NodeIdentity, _: ClaimEpoch) -> Result<bool, ClaimError> {
        unreachable!("planning is read-only")
    }
    async fn release(&self, _: &Entity, _: &NodeIdentity, _: ClaimEpoch) -> Result<(), ClaimError> {
        unreachable!("planning is read-only")
    }
    async fn release_many(&self, _: &[Entity], _: &NodeIdentity) -> Result<(), ClaimError> {
        unreachable!("planning is read-only")
    }
}

async fn ownership_failure(fixture: IngressFixture) {
    use crate::clustering::{route_bridge::OrderedRelayDeliveryBridge, ClusteringHandles};
    use waddle_xmpp::{ingress::WireHandledCount, pending_delivery::SmSessionId};
    let claims = Arc::new(PlanningClaims::new(NodeIdentity::new("local", "epoch")));
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            ClusteringHandles {
                claim_store: Some(claims.clone()),
                node_identity: Some(SharedNodeIdentity::new(NodeIdentity::new("local", "epoch"))),
                ordered_relay_delivery_bridge: Some(OrderedRelayDeliveryBridge::new(
                    tokio_util::sync::CancellationToken::new(),
                    &crate::config::ClusteringMessagingConfig::default(),
                )),
                ..Default::default()
            },
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        )
        .await;
    // The planning node owns this recipient. Its claim lookup must succeed
    // before the bridge permits actor-local recipient inventory.
    let bridge = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
        .expect("bridge");
    crate::clustering::route_bridge::wire_for_test(
        bridge,
        &state,
        claims.clone(),
        SharedNodeIdentity::new(NodeIdentity::new("local", "epoch")),
    )
    .await;
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let (recipient_tx, mut recipient_rx) = tokio::sync::mpsc::channel(2);
    crate::server::routes::websocket::tests::register_test_connection(
        &state,
        &recipient,
        recipient_tx,
    )
    .await;
    let mut submission = fixture.submission(Some("ownership-retry"), "recipient body");
    let stream_id = SmSessionId::new("ownership-read-stream");
    let mut tx = fixture.uow.begin().await.expect("stream transaction");
    let sm_ingress_id = crate::ingress_uow::SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("mint stream");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        owner: NodeIdentity::new("unused", "unused"),
        claim_epoch: ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::from_storage(1),
        checkpoint_h: WireHandledCount::from_storage(1),
    };
    let entity = Entity::new(
        EntityType::UserActor,
        submission.sender.to_bare().to_string(),
    );
    let mut deps = Deps::registry_only(&state.deps.protocol.connection_registry);
    deps.web_socket_state = Some(state.as_ref());
    deps.message_dispatcher = Some(&state.deps.protocol.dispatcher);
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.mam_storage = Some(&state.deps.protocol.mam_storage);
    deps.inbox_storage = Some(&state.deps.protocol.inbox_storage);
    deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(entity.clone()),
        sender_entity: entity,
        inbound_sequence: 1,
        handoff: None,
    });
    // This test isolates route ownership; carbon inventory has its own failure tests.
    submission
        .plan
        .sanitized_message
        .payloads
        .push(minidom::Element::builder("private", waddle_xmpp_core::carbons::CARBONS_NS).build());
    let message = submission.plan.sanitized_message.clone();
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(submission.sender.clone(), false);
    claims.fail_reads(true);
    submission.plan = plan_message_dispatch(&mut machine, message.clone(), &deps).await;
    assert_eq!(submission.plan.failure, Some(PlanFailure::OwnershipLookup));
    let failure = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect_err("unavailable ownership must not commit");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_refs",
        "ingress_deliveries",
        "mam_messages",
        "inbox_entries",
    ] {
        assert_eq!(fixture.count(table).await, 0, "{table}");
    }
    let conn = fixture.db.guard().await.expect("database");
    let mut rows = conn
        .query(
            "SELECT CAST(handled_ordinal AS BIGINT), checkpoint_h FROM ingress_sm_streams",
            (),
        )
        .await
        .expect("frontier");
    let row = rows.next().await.expect("query").expect("stream");
    assert_eq!(row.get::<i64>(0).expect("ordinal"), 0);
    assert_eq!(row.get::<i64>(1).expect("checkpoint"), 0);
    drop(rows);
    drop(conn);
    claims.fail_reads(false);
    submission.plan = plan_message_dispatch(&mut machine, message, &deps).await;
    assert_eq!(submission.plan.failure, None);
    assert!(submission.plan.plan.iter().any(|effect| matches!(
        &effect.effect,
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { jid, kind: PeerDeliveryKind::DirectFrame, .. }
        )) if jid == &recipient
    )));
    assert!(
        recipient_rx.try_recv().is_err(),
        "planning cannot execute recipient delivery"
    );
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("healthy retry commits the prepared recipient obligation");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(
        decision.ordinal,
        Some(waddle_xmpp::ingress::IngressOrdinal::FIRST)
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    let conn = fixture.db.guard().await.expect("database");
    let mut rows = conn
        .query(
            "SELECT CAST(handled_ordinal AS BIGINT), checkpoint_h FROM ingress_sm_streams",
            (),
        )
        .await
        .expect("retry frontier");
    let row = rows.next().await.expect("query").expect("stream");
    assert_eq!(row.get::<i64>(0).expect("retry ordinal"), 1);
    assert_eq!(row.get::<i64>(1).expect("retry checkpoint"), 1);
    drop(rows);
    drop(conn);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_ownership_lookup_failure_retry_sqlite() {
    ownership_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_ownership_lookup_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ownership_retry").await {
        ownership_failure(fixture).await;
    }
}

/// RFC 0018 §3: clustered planning requires authoritative recipient inventory.
async fn full_jid_inventory_plan(remote_unavailable: bool) {
    use crate::clustering::{route_bridge::OrderedRelayDeliveryBridge, ClusteringHandles};
    use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};
    let owner = if remote_unavailable {
        "unavailable-recipient-owner"
    } else {
        "local"
    };
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            ClusteringHandles {
                claim_store: Some(Arc::new(PlanningClaims::new(NodeIdentity::new(
                    owner, "epoch",
                )))),
                node_identity: Some(SharedNodeIdentity::new(NodeIdentity::new("local", "epoch"))),
                ordered_relay_delivery_bridge: Some(OrderedRelayDeliveryBridge::new(
                    tokio_util::sync::CancellationToken::new(),
                    &crate::config::ClusteringMessagingConfig::default(),
                )),
                ..Default::default()
            },
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        )
        .await;
    // Bridge and planning identities agree. Only the success case owns the
    // recipient locally; the remote case must request an unavailable owner.
    // A local resource is deliberately present in both cases to catch fallback.
    let clustering = &state.deps.app_state.clustering_claims;
    crate::clustering::route_bridge::wire_for_test(
        clustering
            .ordered_relay_delivery_bridge
            .as_ref()
            .expect("bridge"),
        &state,
        clustering.claim_store.as_ref().expect("claims").clone(),
        SharedNodeIdentity::new(NodeIdentity::new("local", "epoch")),
    )
    .await;
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let (recipient_tx, mut recipient_rx) = tokio::sync::mpsc::channel(2);
    crate::server::routes::websocket::tests::register_test_connection(
        &state,
        &recipient,
        recipient_tx,
    )
    .await;
    let entity = Entity::new(EntityType::UserActor, sender.to_bare().to_string());
    let mut deps = Deps::registry_only(&state.deps.protocol.connection_registry);
    deps.web_socket_state = Some(state.as_ref());
    deps.message_dispatcher = Some(&state.deps.protocol.dispatcher);
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.mam_storage = Some(&state.deps.protocol.mam_storage);
    deps.inbox_storage = Some(&state.deps.protocol.inbox_storage);
    deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(entity.clone()),
        sender_entity: entity,
        inbound_sequence: 1,
        handoff: None,
    });
    let mut message = xmpp_parsers::message::Message::new(Some(recipient.clone().into()));
    message.from = Some(sender.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .payloads
        .push(minidom::Element::builder("private", waddle_xmpp_core::carbons::CARBONS_NS).build());
    message
        .bodies
        .insert(Default::default(), "recipient delivery".into());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "full-receipt");
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(sender, false);
    let plan = plan_message_dispatch(&mut machine, message, &deps).await;
    if remote_unavailable {
        assert_eq!(plan.failure, Some(PlanFailure::OwnershipLookup));
        assert!(
            !plan.plan.iter().any(|effect| matches!(
                effect.effect,
                Effect::External(ExternalEffect::Delivery(_))
            )),
            "failed remote inventory cannot fall back to the local resource"
        );
        assert!(
            !plan.intents.iter().any(|intent| match intent {
                IngressEffectIntent::RouteDirect {
                    recipient: bare, ..
                } => bare == &recipient.to_bare(),
                IngressEffectIntent::ArchiveAuthoritative { archive, .. } =>
                    archive == &recipient.to_bare(),
                IngressEffectIntent::InboxProject { owner, .. } => owner == &recipient.to_bare(),
                _ => false,
            }),
            "unavailable remote inventory cannot run a local recipient pass"
        );
        assert!(
            recipient_rx.try_recv().is_err(),
            "no unreceipted peer fallback"
        );
        return;
    }
    assert_eq!(plan.failure, None);
    let identity = plan
        .intents
        .iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::RouteDirect {
                prepared: _,
                recipient: bare,
                fanout,
                route_identity,
            } if bare == &recipient.to_bare() && fanout.contains(&recipient) => {
                Some(route_identity)
            }
            _ => None,
        })
        .expect("captured direct route to full recipient");
    assert!(matches!(identity, EffectMessageIdentity::CaptureOrdinal(_)));
    let deliveries: Vec<_> = plan
        .plan
        .iter()
        .filter_map(|effect| match &effect.effect {
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                jid,
                route_identity,
                kind: PeerDeliveryKind::DirectFrame,
                stanza,
                ..
            })) if jid == &recipient => {
                let waddle_xmpp::Stanza::Message(message) = stanza.as_ref() else {
                    panic!("prepared recipient message");
                };
                assert_eq!(message.to, Some(recipient.clone().into()));
                use waddle_xmpp_core::xep0359::StanzaIdCarrier;
                let recipient_stamp = plan
                    .intents
                    .iter()
                    .find_map(|intent| match intent {
                        IngressEffectIntent::ArchiveAuthoritative {
                            archive, stanza_id, ..
                        } if archive == &recipient.to_bare() => Some(stanza_id),
                        _ => None,
                    })
                    .expect("recipient archive obligation");
                assert!(message.stanza_ids().contains(recipient_stamp));
                Some(route_identity)
            }
            _ => None,
        })
        .collect();
    assert_eq!(deliveries, vec![&Some(identity.clone())]);
    assert!(plan.intents.iter().any(|intent| matches!(intent,
        IngressEffectIntent::InboxProject { owner, .. } if owner == &recipient.to_bare()
    )));
    assert!(
        recipient_rx.try_recv().is_err(),
        "planning sends no original"
    );
}

#[tokio::test]
async fn ingress_owner_local_full_jid_plan_preserves_direct_receipt_identity() {
    full_jid_inventory_plan(false).await;
}

#[tokio::test]
async fn ingress_remote_full_jid_inventory_unavailable_rejects_local_fallback() {
    full_jid_inventory_plan(true).await;
}
