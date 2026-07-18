use std::sync::Arc;

use tokio::sync::broadcast;
use waddle_xmpp_client::{
    request::StanzaId, ClientEvent, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent,
    SessionBinding, StreamManagementEvent,
};

use super::{
    NativeEventPump, WaddleClient, WaddleClientEvent, WaddleConfig, WaddleConnectionGeneration,
    WaddleDeliveryAttemptId, WaddleDeliveryAttemptRef, WaddleDeliveryStanzaId,
    WaddleSessionReadyKind,
};

fn attempt() -> WaddleDeliveryAttemptRef {
    WaddleDeliveryAttemptRef {
        attempt_id: WaddleDeliveryAttemptId {
            value: "00000000-0000-4000-8000-000000000001".to_string(),
        },
        connection_generation: WaddleConnectionGeneration { value: 4 },
    }
}

fn binding() -> SessionBinding {
    SessionBinding {
        jid: "alice@waddle.test/android"
            .parse()
            .expect("test full JID parses"),
        stream_id: None,
        resumable: false,
    }
}

fn config() -> WaddleConfig {
    WaddleConfig {
        server_url: "wss://xmpp.waddle.test".to_string(),
        jid: "alice@waddle.test".to_string(),
        access_token: "token".to_string(),
        resource: "android".to_string(),
        delivery_attempt: attempt(),
        resume_state: None,
    }
}

fn pump(awaiting_resume: bool) -> (broadcast::Sender<ClientEvent>, NativeEventPump) {
    let (sender, receiver) = broadcast::channel(32);
    (
        sender,
        NativeEventPump::new(
            receiver,
            "alice@waddle.test".to_string(),
            attempt(),
            awaiting_resume,
            1,
        ),
    )
}

#[tokio::test]
async fn failed_resume_returns_one_transition_before_later_fresh_event() {
    let (sender, mut pump) = pump(true);
    let failed = ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed {
        stanza_id: StanzaId::new("m-1").expect("stanza id"),
    });
    sender.send(failed.clone()).expect("first failure queued");
    sender.send(failed).expect("duplicate failure queued");
    sender
        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Failed,
        )))
        .expect("SM failure queued");
    sender
        .send(ClientEvent::Lifecycle(LifecycleEvent::SessionReady(
            binding(),
        )))
        .expect("fresh readiness queued");

    let first = pump.next_event().await;
    let transition = match first {
        WaddleClientEvent::ResumeFailed {
            transition,
            affected,
        } => {
            assert_eq!(
                affected,
                vec![WaddleDeliveryStanzaId {
                    value: "m-1".to_string()
                }]
            );
            transition
        }
        _ => panic!("expected failed-resume transition"),
    };
    assert_eq!(transition.old, attempt());
    assert_ne!(transition.old.attempt_id, transition.fresh.attempt_id);
    assert_eq!(transition.fresh.connection_generation.value, 5);
    assert_eq!(pump.duplicate_resume_failure_count, 1);

    let second = pump.next_event().await;
    assert!(matches!(
        second,
        WaddleClientEvent::SessionReady {
            kind: WaddleSessionReadyKind::Fresh,
            attempt: ready_attempt,
        } if ready_attempt == transition.fresh
    ));
}

#[tokio::test]
async fn contradictory_resume_ack_and_failure_self_fences() {
    let (sender, mut pump) = pump(true);
    sender
        .send(ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked {
            stanza_id: StanzaId::new("m-1").expect("stanza id"),
        }))
        .expect("ack queued");
    sender
        .send(ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed {
            stanza_id: StanzaId::new("m-1").expect("stanza id"),
        }))
        .expect("failure queued");

    assert!(matches!(
        pump.next_event().await,
        WaddleClientEvent::DeliveryAcked { .. }
    ));
    assert!(matches!(
        pump.next_event().await,
        WaddleClientEvent::Error { .. }
    ));
    assert!(matches!(
        pump.next_event().await,
        WaddleClientEvent::Disconnected
    ));
}

#[tokio::test]
async fn resumed_readiness_preserves_the_original_attempt() {
    let (sender, mut pump) = pump(true);
    sender
        .send(ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Resumed { h: 9 },
        )))
        .expect("resumed queued");
    assert!(matches!(
        pump.next_event().await,
        WaddleClientEvent::SessionReady {
            kind: WaddleSessionReadyKind::Resumed,
            attempt: ready_attempt,
        } if ready_attempt == attempt()
    ));
}

#[tokio::test]
async fn disconnect_wakes_a_poll_even_before_a_pump_exists() {
    let client = WaddleClient::new_for_test(config(), Arc::new(std::sync::Mutex::new(Vec::new())));
    let polling_client = Arc::clone(&client);
    let poll = tokio::spawn(async move { polling_client.next_event().await });
    tokio::task::yield_now().await;

    client.disconnect().await;

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), poll)
        .await
        .expect("disconnect must wake the poll")
        .expect("poll task joins");
    assert!(matches!(event, WaddleClientEvent::Disconnected));
}
