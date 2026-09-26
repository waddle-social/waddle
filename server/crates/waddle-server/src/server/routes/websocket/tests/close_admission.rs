//! RFC 7395 stream close must fence delivery before its acknowledgement.

use super::*;
use crate::server::routes::websocket::{
    connection::{handle_inbound_text, ConnectionIo, FrameAuthority, RegistrationChannels},
    transport_xml::{websocket_stream_close_element, websocket_stream_open_element},
};
use axum::extract::ws::Message;

#[tokio::test]
async fn graceful_close_rejects_old_sender_before_close_acknowledgement() {
    assert_outbound_admission_at_response(true).await;
}

#[tokio::test]
async fn authenticated_stream_open_preserves_outbound_admission() {
    assert_outbound_admission_at_response(false).await;
}

async fn assert_outbound_admission_at_response(close: bool) {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/close-admission".parse().unwrap();
    let mut conn = WsConnState::new();
    conn.authenticated_session = Some(create_test_session(&state, "alice").await);
    conn.phase = if close {
        ConnectionPhase::ready(jid.clone(), false)
    } else {
        ConnectionPhase::authenticated(&jid)
    };
    conn.presence_available = close;
    let (sender, mut outbound_rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(sender.clone());
    if close {
        assert!(matches!(
            register_bound_connection_after_frame(
                &state,
                "example.com",
                &mut conn,
                &mut pending_tx
            )
            .await,
            RegistrationAfterFrame::Registered(_)
        ));
    }
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut socket = Box::pin(futures::sink::unfold(
        (captured.clone(), sender),
        |(captured, sender), frame: Message| async move {
            if matches!(&frame, Message::Text(_)) {
                // This is the old actor route's actual sender. Probe at the
                // exact point the peer can observe the terminal response,
                // before handle_inbound_text returns to shutdown cleanup.
                let offered = OutboundStanza::peer_stanza(Stanza::Message(
                    xmpp_parsers::message::Message::new(None::<jid::Jid>),
                ));
                let closed = matches!(
                    sender.try_send(offered),
                    Err(mpsc::error::TrySendError::Closed(_))
                );
                captured.lock().unwrap().push((frame, closed));
            }
            Ok::<_, std::io::Error>((captured, sender))
        },
    ));
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut reader = futures::stream::pending::<Result<Message, std::io::Error>>();
    let mut force_detach_rx = None;
    let frame = element_to_xml(if close {
        websocket_stream_close_element()
    } else {
        websocket_stream_open_element("example.com")
    });
    let remains_open = handle_inbound_text(
        &frame,
        "example.com",
        &state,
        &mut conn,
        RegistrationChannels {
            outbound_rx: &mut outbound_rx,
            pending_tx: &mut pending_tx,
            force_detach_rx: &mut force_detach_rx,
        },
        ConnectionIo {
            sender: &mut socket,
            receiver: &mut reader,
        },
        FrameAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert_eq!(remains_open, !close);
    let frames = captured.lock().unwrap();
    assert!(!frames.is_empty(), "the acknowledgement reached the sink");
    if close {
        assert!(frames.iter().all(|(_, closed)| *closed),
            "the old actor sender must reject admission before the close acknowledgement is written");
        assert!(outbound_rx.is_closed());
        assert!(
            conn.presence_available,
            "shutdown cleanup still needs the previous presence state"
        );
    } else {
        assert!(frames.iter().all(|(_, closed)| !closed));
        assert!(!outbound_rx.is_closed());
        assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    }
}
