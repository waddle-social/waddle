use super::frame::{ResponseFrame, StreamErrorFrame};
use super::transport_xml::websocket_stream_close_element;
use super::{
    debug, info, warn, BareJid, ConnectionPhase, Element, FromStr, Session, SmEnable, SmResume,
    SmStanza, StreamManagementState, WebSocketState,
};
use crate::auth::session::PrincipalResolution;
use crate::db::actor::DbExecute;
use crate::ingress_uow::IngressUowError;
use crate::server::routes::interpret::SmInboundCompletionTracker;
#[cfg(test)]
use std::sync::Arc;
mod observe;
#[cfg(test)]
mod timeout_tests;
use observe::{apply_sm_ack_observation, observe_sm_ack, SmAckObservation};

#[cfg(test)]
mod enable_claim_guard_tests {
    use super::SmEnableClaimGuard;
    use std::sync::Arc;
    use waddle_xmpp::ownership::{ClaimStore as _, Entity, EntityType};
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

    #[tokio::test]
    async fn dropped_prepublication_guard_defers_and_releases_the_exact_claim() {
        let store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        let identity = waddle_xmpp::ownership::NodeIdentity::new("sm-node", "incarnation");
        let shared_identity = waddle_xmpp::ownership::SharedNodeIdentity::new(identity);
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_claim_store(store.clone(), shared_identity.clone()),
        );
        let stream_id = "cancelled-before-enabled-publication";
        let claim_publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("claim admission");

        let guard = SmEnableClaimGuard::new(
            registry.clone(),
            waddle_xmpp::pending_delivery::SmSessionId::new(stream_id),
            claim_publication,
        );
        drop(guard);

        assert_eq!(registry.pending_claim_release_count(), 1);
        assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
        assert_eq!(registry.pending_claim_release_count(), 0);
        assert!(store
            .current_claim(&Entity::new(EntityType::SmSession, stream_id))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test]
    async fn prepublication_guard_blocks_self_fence_demotion_until_commit() {
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let stream_id = "enable-publication-vs-self-fence";
        let claim_publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("claim admission");
        let guard = SmEnableClaimGuard::new(
            registry.clone(),
            waddle_xmpp::pending_delivery::SmSessionId::new(stream_id),
            claim_publication,
        );
        let demotion = tokio::spawn({
            let registry = registry.clone();
            async move { registry.forget_claim_locally(stream_id).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !demotion.is_finished(),
            "pending transport publication must block self-fence demotion"
        );

        guard.commit();
        demotion.await.expect("self-fence demotion");
        assert!(registry
            .locally_owned_claim_ids()
            .expect("owned inventory")
            .is_empty());
    }
}

mod registration;

pub(super) use observe::observe_sm_resume_finalized;
pub(super) use registration::{
    finalize_sm_after_registry_registration, reject_unregistered_resume, SmRegistrationFinalization,
};

#[cfg(test)]
mod ingress_checkpoint_tests {
    use super::*;
    use crate::server::routes::websocket::state::WsConnState;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::ingress::WireHandledCount;
    use waddle_xmpp::pending_delivery::SmSessionId;

    #[tokio::test]
    async fn pending_iq_completion_flushes_committed_message_frontier_before_ack() {
        let state = create_test_websocket_state().await;
        let stream = SmSessionId::new("checkpoint-before-ack");
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&stream)
            .await
            .expect("enroll");
        let mut conn = WsConnState::new();
        conn.sm_state.enable(stream.to_string(), true, Some(300));
        conn.phase = ConnectionPhase::ready("alice@example.com/web".parse().expect("jid"), false);
        let iq = conn.sm_inbound_completion.reserve(&conn.sm_state);
        for _ in 0..2 {
            let message = conn.sm_inbound_completion.reserve(&conn.sm_state);
            let checkpoint = conn.sm_inbound_completion.checkpoint_for(message);
            assert_eq!(checkpoint, 0);
            conn.sm_inbound_completion
                .mark_committed(message, checkpoint);
            conn.sm_inbound_completion
                .complete(message, &mut conn.sm_state);
        }
        assert_eq!(conn.sm_state.inbound_count(), 0);
        conn.sm_inbound_completion.complete(iq, &mut conn.sm_state);
        assert_eq!(conn.sm_state.inbound_count(), 3);
        assert!(conn.sm_inbound_completion.checkpoint_dirty());
        let request = waddle_xmpp::stream_management::SmRequest::to_xml();
        let frames =
            super::super::frame::handle_xmpp_frame(&request, "example.com", &state, &mut conn)
                .await;
        let ack = frames
            .first()
            .expect("ack frame")
            .parse::<Element>()
            .expect("XML");
        assert_eq!(ack.attr("h"), Some("3"));
        assert_eq!(
            state
                .deps
                .protocol
                .ingress
                .load_resume_checkpoint(&stream)
                .await
                .expect("load"),
            Some(WireHandledCount::from_storage(3))
        );
        assert!(!conn.sm_inbound_completion.checkpoint_dirty());
    }

    #[tokio::test]
    async fn resume_checkpoint_restores_wrap_aware_maximum() {
        let state = create_test_websocket_state().await;
        let stream = SmSessionId::new("resume-checkpoint-wrap");
        let id = state
            .deps
            .protocol
            .ingress
            .enroll_stream(&stream)
            .await
            .expect("enroll");
        let mut sm = StreamManagementState::new();
        sm.enable(stream.to_string(), true, Some(300));
        sm.inbound_count = u32::MAX;
        let mut detached = sm
            .to_detached_session(waddle_xmpp::stream_management::DetachedSessionSnapshot {
                user_id: "alice@example.com".into(),
                jid: "alice@example.com/web".parse().expect("jid"),
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .expect("detached");
        state
            .deps
            .protocol
            .ingress
            .flush_checkpoint(id, WireHandledCount::from_storage(1))
            .await
            .expect("flush");
        restore_ingress_checkpoint(&state, &mut detached)
            .await
            .expect("restore checkpoint");
        assert_eq!(detached.inbound_count, 1);
        detached.inbound_count = 2;
        restore_ingress_checkpoint(&state, &mut detached)
            .await
            .expect("restore checkpoint");
        assert_eq!(
            detached.inbound_count, 2,
            "snapshot ahead of checkpoint wins"
        );
    }

    #[tokio::test]
    async fn cancelled_enabled_write_retires_unpublished_ingress_stream() {
        let state = create_test_websocket_state().await;
        let mut sm = StreamManagementState::new();
        let phase = ConnectionPhase::ready("alice@example.com/web".parse().expect("jid"), false);
        let mut pending = None;
        let frames = handle_sm_enable(
            SmEnable::with_resume(Some(300)),
            &state,
            &mut sm,
            &phase,
            &mut pending,
        )
        .await;
        let enabled = frames
            .into_iter()
            .next()
            .expect("enabled")
            .into_serialized_xml()
            .parse::<Element>()
            .expect("enabled XML");
        assert_eq!(enabled.name(), "enabled");
        let stream = SmSessionId::new(enabled.attr("id").expect("stream id"));
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("lookup")
            .is_some());
        assert!(!sm.enabled, "unwritten enabled cannot publish SM");
        drop(pending.take());
        state
            .deps
            .protocol
            .sm_session_registry
            .retry_pending_claim_releases(64)
            .await;
        crate::server::session_janitors::run_ingress_retirement_sweep(&state).await;
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("lookup")
            .is_none());
    }

    #[tokio::test]
    async fn checkpoint_failure_never_exposes_ack() {
        let state = create_test_websocket_state().await;
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("missing-ingress-stream".into(), true, Some(300));
        conn.phase = ConnectionPhase::ready("alice@example.com/web".parse().expect("jid"), false);
        let iq = conn.sm_inbound_completion.reserve(&conn.sm_state);
        let message = conn.sm_inbound_completion.reserve(&conn.sm_state);
        conn.sm_inbound_completion.mark_committed(message, 0);
        conn.sm_inbound_completion
            .complete(message, &mut conn.sm_state);
        conn.sm_inbound_completion.complete(iq, &mut conn.sm_state);
        let frames = super::super::frame::handle_xmpp_frame(
            &waddle_xmpp::stream_management::SmRequest::to_xml(),
            "example.com",
            &state,
            &mut conn,
        )
        .await;
        assert!(frames.is_empty());
        assert!(conn.sm_inbound_completion.checkpoint_dirty());
        assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));
    }
}

mod implementation;
pub(super) use implementation::apply_sm_ack;
pub(super) use implementation::attempt_cross_node_resume_raced;
pub(super) use implementation::complete_acknowledged_ingress_receipts;
pub(super) use implementation::defer_superseded_sm_claim;
pub(super) use implementation::flush_ingress_checkpoint;
#[cfg(test)]
use implementation::handle_sm_enable;
pub(super) use implementation::handle_sm_stanza;
pub(super) use implementation::is_countable_stanza;
#[cfg(test)]
use implementation::restore_ingress_checkpoint;
pub(super) use implementation::sm_show_from_name;
pub(super) use implementation::sm_show_name;
pub(super) use implementation::terminate_if_demoted_during_publication;
pub(super) use implementation::CrossNodeAttemptOutcome;
pub(super) use implementation::SmCtx;
#[cfg(test)]
use implementation::SmEnableClaimGuard;
pub(super) use implementation::SmEnableCommit;
pub(super) use implementation::SmResumeClaimGuard;
