use super::super::{
    cleanup::{
        cleanup_force_detach_connection_shutdown, finish_socket_cleanup, ConnectionShutdownOutcome,
    },
    frame::handle_xmpp_frame,
    registration::{register_bound_connection_after_frame, RegistrationAfterFrame},
    state::WsConnState,
    tests::{create_test_session, create_test_websocket_state_with_clustering},
    transport_xml::element_to_xml,
};
use crate::clustering::ClusteringHandles;
use jid::FullJid;
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use waddle_xmpp::{
    protocol::ConnectionPhase,
    registry::ForceDetachOutcome,
    stream_management::{InMemorySmSessionRegistry, SM_NS},
};
use xmpp_parsers::minidom::Element;

async fn local_resume_after_slow_cleanup(clustering: ClusteringHandles, expect_waiting: bool) {
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

    // Keep database/actor setup on real time; only the held handshake needs
    // a virtual delay. Its request proves the timeout future has been polled.
    tokio::time::pause();
    let mut resuming = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            let mut conn = WsConnState::new();
            conn.phase = ConnectionPhase::authenticated(&jid);
            conn.authenticated_session = Some(session);
            handle_xmpp_frame(&resume, "example.com", &state, &mut conn).await
        }
    });
    let request = requests.recv().await.expect("local detach request");
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        !request.ack.is_closed(),
        expect_waiting,
        "local resume must honor its handshake budget while cleanup is delayed"
    );
    tokio::time::resume();
    // An expired attempt must observe the missing snapshot before cleanup
    // publishes it; a later snapshot must not turn this into a successful resume.
    let expired_responses = if expect_waiting {
        None
    } else {
        Some((&mut resuming).await.expect("expired resume task"))
    };

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
    assert_eq!(
        request.ack.send(ForceDetachOutcome::Detached).is_ok(),
        expect_waiting,
        "only an unexpired resume can receive cleanup acknowledgement"
    );
    let responses = match expired_responses {
        Some(responses) => responses,
        None => resuming.await.expect("resume task"),
    };
    let expected_response = if expect_waiting { "resumed" } else { "failed" };
    assert!(
        responses.iter().any(|frame| {
            let element = Element::from_str(frame).expect("response XML");
            element.name() == expected_response && element.ns() == SM_NS
        }),
        "expected {expected_response} after delayed cleanup: {responses:?}"
    );
}

#[tokio::test]
async fn local_resume_default_budget_allows_cleanup_past_five_seconds() {
    local_resume_after_slow_cleanup(ClusteringHandles::default(), true).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn local_resume_uses_configured_handshake_budget_for_slow_cleanup() {
    local_resume_after_slow_cleanup(
        ClusteringHandles {
            resume_handshake_timeout: Some(Duration::from_secs(10)),
            ..Default::default()
        },
        true,
    )
    .await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn local_resume_expires_at_configured_short_handshake_budget() {
    local_resume_after_slow_cleanup(
        ClusteringHandles {
            resume_handshake_timeout: Some(Duration::from_secs(2)),
            ..Default::default()
        },
        false,
    )
    .await;
}
