use super::super::cleanup::{
    cleanup_force_detach_connection_shutdown, finish_socket_cleanup, ConnectionShutdownOutcome,
};
use super::super::frame::handle_xmpp_frame;
use super::super::registration::{register_bound_connection_after_frame, RegistrationAfterFrame};
use super::super::state::WsConnState;
use super::super::tests::{create_test_session, create_test_websocket_state_with_clustering};
use super::super::transport_xml::element_to_xml;
use crate::clustering::ClusteringHandles;
use jid::FullJid;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::registry::ForceDetachOutcome;
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SM_NS};
use xmpp_parsers::minidom::Element;

async fn local_resume_after_slow_cleanup(clustering: ClusteringHandles) {
    let state = create_test_websocket_state_with_clustering(
        clustering,
        Arc::new(InMemorySmSessionRegistry::new().with_persistence(Arc::new(
            waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new(),
        ))),
    )
    .await;
    let session = create_test_session(&state, "alice").await;
    let jid: FullJid = "alice@example.com/slow-cleanup".parse().expect("JID");
    let mut old = WsConnState::new();
    old.phase = ConnectionPhase::ready(jid.clone(), false);
    old.authenticated_session = Some(session.clone());
    let (sender, mut outbound) = mpsc::channel(4);
    let mut pending = Some(sender);
    assert!(matches!(
        register_bound_connection_after_frame(&state, "example.com", &mut old, &mut pending).await,
        RegistrationAfterFrame::Registered(_)
    ));
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        &state,
        &mut old,
    )
    .await;
    let enabled = Element::from_str(&enabled[0]).expect("enabled response");
    let stream_id = enabled.attr("id").expect("SM stream ID");
    old.publish_pending_sm_enable(&state);
    let mut requests = old
        .socket_lifecycle
        .as_ref()
        .expect("old socket lifecycle")
        .entry
        .take_force_detach_rx()
        .expect("old control receiver");
    let resume = element_to_xml(
        Element::builder("resume", SM_NS)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), stream_id)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
            .build(),
    );

    // Keep database/actor setup on real time; only accepted cleanup needs a
    // virtual delay. Service preflight on the old connection's owner task.
    tokio::time::pause();
    let resuming = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            let mut conn = WsConnState::new();
            conn.phase = ConnectionPhase::authenticated(&jid);
            conn.authenticated_session = Some(session);
            handle_xmpp_frame(&resume, "example.com", &state, &mut conn).await
        }
    });
    std::future::poll_fn(|cx| old.live_resume_control.poll(cx)).await;
    let request = requests.recv().await.expect("local detach request");
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;
    assert!(
        !request.ack.is_closed(),
        "accepted local resume must wait through captured cleanup, even past its handshake budget"
    );
    tokio::time::resume();

    let outcome = cleanup_force_detach_connection_shutdown(
        &state,
        &mut outbound,
        &mut old,
        false,
        request.origin,
    )
    .await;
    assert_eq!(outcome, ConnectionShutdownOutcome::Detached);
    finish_socket_cleanup(&state, &old, outcome).await;
    request
        .ack
        .send(ForceDetachOutcome::Detached)
        .expect("accepted local resume still waits for cleanup");
    let responses = resuming.await.expect("resume task");
    assert!(
        responses.iter().any(|frame| {
            let element = Element::from_str(frame).expect("response XML");
            element.name() == "resumed" && element.ns() == SM_NS
        }),
        "expected resumed after delayed cleanup: {responses:?}"
    );
}

#[tokio::test]
async fn local_resume_default_budget_allows_cleanup_past_five_seconds() {
    local_resume_after_slow_cleanup(ClusteringHandles::default()).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn local_resume_with_cluster_configuration_waits_for_slow_cleanup() {
    local_resume_after_slow_cleanup(ClusteringHandles {
        resume_handshake_timeout: Some(Duration::from_secs(10)),
        ..Default::default()
    })
    .await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn local_resume_waits_for_captured_cleanup_past_handshake_budget() {
    local_resume_after_slow_cleanup(ClusteringHandles {
        resume_handshake_timeout: Some(Duration::from_secs(2)),
        ..Default::default()
    })
    .await;
}
