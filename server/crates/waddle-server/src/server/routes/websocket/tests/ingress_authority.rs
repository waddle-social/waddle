use super::super::frame::{handle_xmpp_frame, settle_inbound_dispatch};
use super::super::frame_backstop::InboundDisposition;
use super::super::state::WsConnState;
use super::*;
use waddle_xmpp::pending_delivery::SmSessionId;

async fn connection(state: &WebSocketState, resumable: bool) -> WsConnState {
    let mut conn = WsConnState::new();
    let jid: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(state, "alice").await);
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid,
        false,
        Default::default(),
    );
    if resumable {
        let id = SmSessionId::new("authority-connection");
        drop(
            state
                .deps
                .protocol
                .sm_session_registry
                .ensure_session_claim(id.as_str())
                .await
                .expect("SM claim"),
        );
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&id)
            .await
            .expect("enroll");
        conn.sm_ingress_fence = state
            .deps
            .protocol
            .sm_session_registry
            .current_sm_claim_fence(id.as_str());
        conn.sm_state
            .enable(id.as_str().to_owned(), true, Some(300));
    }
    conn
}

fn offered_message() -> String {
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("target")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    // A committed semantic rejection exercises the transaction without recipient writes.
    message
        .payloads
        .push(minidom::Element::builder("result", waddle_xmpp::xep::NS_INBOX).build());
    super::super::transport_xml::stanza_to_xml(&Stanza::Message(message))
}

#[tokio::test]
async fn committed_message_advances_h_and_records_checkpoint() {
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    let frames = handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn).await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    assert!(frames.iter().any(|frame| frame.contains("bad-request")));
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&SmSessionId::new("authority-connection"))
            .await
            .expect("checkpoint")
            .expect("stream checkpoint")
            .to_storage(),
        1
    );
}

#[tokio::test]
async fn non_advancing_resumable_message_leaves_an_ordinary_hole() {
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    assert!(
        state
            .deps
            .protocol
            .ingress
            .drain_and_join(std::time::Duration::from_secs(1))
            .await
    );
    let frames = handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn).await;
    assert!(frames.is_empty());
    assert_eq!(conn.sm_state.get_inbound_count(), 0);
    assert!(conn.sm_state.is_resumable());
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert!(!conn.sm_recovery_required);
}

#[tokio::test]
async fn non_advancing_ephemeral_message_emits_stream_error_and_close() {
    let database = crate::db::Database::in_memory("unavailable-ingress")
        .await
        .expect("database");
    let ingress = Arc::new(crate::ingress::IngressAuthority::for_test(database).await);
    assert!(
        ingress
            .drain_and_join(std::time::Duration::from_secs(1))
            .await
    );
    let state = create_test_websocket_state_with_sm_registry_and_ingress(
        Arc::new(InMemorySmSessionRegistry::new()),
        ingress,
    )
    .await;
    let mut conn = connection(&state, false).await;
    let frames = handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn).await;
    assert_eq!(frames.len(), 2);
    let error: minidom::Element = frames[0].parse().expect("typed stream error");
    assert!(error.is("error", waddle_xmpp::ns::STREAM));
    assert!(error
        .get_child("internal-server-error", xmpp_parsers::ns::XMPP_STREAMS)
        .is_some());
    assert!(frames[1].contains("close"));
}

#[test]
fn deferred_iq_keeps_h_pending_until_its_handoff() {
    let mut conn = WsConnState::new();
    conn.sm_state
        .enable("deferred-iq".to_owned(), true, Some(300));
    let seq = conn.sm_inbound_completion.reserve(&conn.sm_state);
    settle_inbound_dispatch(
        InboundDisposition::Handled,
        true,
        Some(seq),
        &mut conn.sm_inbound_completion,
        &mut conn.sm_state,
    );
    assert_eq!(conn.sm_state.get_inbound_count(), 0);
    conn.sm_inbound_completion.complete(seq, &mut conn.sm_state);
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn phase_c_timeout_keeps_committed_handled_disposition() {
    let mut conn = WsConnState::new();
    conn.sm_state
        .enable("phase-c-timeout".to_owned(), true, Some(300));
    let seq = conn.sm_inbound_completion.reserve(&conn.sm_state);
    conn.sm_inbound_completion.mark_committed(seq, 1);
    settle_inbound_dispatch(
        InboundDisposition::Handled,
        false,
        Some(seq),
        &mut conn.sm_inbound_completion,
        &mut conn.sm_state,
    );
    let batch = super::super::frame::execute_committed_message(
        std::future::pending(),
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(batch.frames.is_empty());
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
}

#[tokio::test]
async fn message_waits_for_ingress_commit_before_responding_or_checkpointing() {
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    let id = SmSessionId::new("authority-connection");
    let blocked = state.deps.protocol.ingress.block_test_stream(&id).await;
    let state_for_task = Arc::clone(&state);
    let task = tokio::spawn(async move {
        let frames = handle_xmpp_frame(
            &offered_message(),
            "example.com",
            &state_for_task,
            &mut conn,
        )
        .await;
        (conn, frames)
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&id)
            .await
            .expect("checkpoint")
            .expect("enrolled stream")
            .to_storage(),
        0
    );
    drop(blocked);
    let (conn, frames) = task.await.expect("dispatch task");
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!frames.is_empty());
}

#[tokio::test]
async fn accepted_message_commits_archive_and_inbox_on_the_authority_database() {
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    create_test_session(&state, "bob").await;
    let alice: jid::BareJid = "alice@example.com".parse().expect("sender");
    let bob: jid::BareJid = "bob@example.com".parse().expect("recipient");
    let mut message = xmpp_parsers::message::Message::new(Some(bob.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "durably accepted".to_owned());
    let wire = super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    let archived = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &alice,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("sender archive");
    assert_eq!(archived.messages.len(), 1);
    assert_eq!(
        archived.messages[0].body.as_deref(),
        Some("durably accepted")
    );
    let inbox = state
        .deps
        .protocol
        .inbox_storage
        .list(&alice)
        .await
        .expect("sender inbox");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].partner, bob);
}

#[tokio::test]
async fn ack_flushes_two_committed_messages_after_deferred_iq_completes() {
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    let iq = conn.sm_inbound_completion.reserve(&conn.sm_state);
    for _ in 0..2 {
        let frames = handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn).await;
        assert!(frames.iter().any(|frame| frame.contains("bad-request")));
        assert_eq!(conn.sm_state.get_inbound_count(), 0);
    }
    let stream = SmSessionId::new("authority-connection");
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&stream)
            .await
            .expect("checkpoint")
            .expect("enrolled")
            .to_storage(),
        0
    );
    conn.sm_inbound_completion.complete(iq, &mut conn.sm_state);
    assert_eq!(conn.sm_state.get_inbound_count(), 3);
    let frames = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmRequest::to_xml(),
        "example.com",
        &state,
        &mut conn,
    )
    .await;
    let ack = frames
        .first()
        .expect("ack")
        .parse::<minidom::Element>()
        .expect("XML");
    assert!(ack.is("a", waddle_xmpp::stream_management::SM_NS));
    assert_eq!(ack.attr("h"), Some("3"));
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&stream)
            .await
            .expect("checkpoint")
            .expect("enrolled")
            .to_storage(),
        3
    );
    assert!(!conn.sm_inbound_completion.checkpoint_dirty());
}

#[tokio::test]
async fn direct_message_spoofed_sender_stanza_id_is_replaced_and_foreign_stamp_is_preserved() {
    use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId, NS_SID};
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    create_test_session(&state, "bob").await;
    let sender: jid::BareJid = "alice@example.com".parse().expect("sender");
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("recipient")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "trusted digest input".into());
    add_stanza_id(
        &mut message,
        &StanzaId::new("spoofed-sender", sender.clone().into()),
    );
    add_stanza_id(
        &mut message,
        &StanzaId::new(
            "foreign-stamp",
            "remote.example.net".parse().expect("foreign authority"),
        ),
    );
    let wire = super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    let archived = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &sender,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("archive");
    assert_eq!(archived.messages.len(), 1);
    let stored: minidom::Element = archived.messages[0]
        .stanza_xml
        .as_ref()
        .expect("stored stanza")
        .parse()
        .expect("stored XML");
    assert!(!stored
        .children()
        .any(|element| element.is("stanza-id", NS_SID)
            && element.attr("id") == Some("spoofed-sender")));
    assert!(stored
        .children()
        .any(|element| element.is("stanza-id", NS_SID)
            && element.attr("id") == Some("foreign-stamp")));
}

#[tokio::test]
async fn groupchat_spoofed_room_stanza_id_reaches_committed_semantic_response() {
    use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    let room: jid::BareJid = "absent@muc.example.com".parse().expect("room");
    let mut message = xmpp_parsers::message::Message::new(Some(room.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message
        .bodies
        .insert(Default::default(), "room digest input".into());
    add_stanza_id(&mut message, &StanzaId::new("spoofed-room", room.into()));
    let wire = super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    assert!(
        frames.iter().any(|frame| {
            let element: minidom::Element = frame.parse().expect("response XML");
            element.is("message", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
                && element.attr("type") == Some("error")
        }),
        "unknown room must produce its standard message error: {frames:?}"
    );
}

async fn frame_receipt_state(state: &WebSocketState) -> (i64, i64) {
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    let mut rows = db.query(
        "SELECT (SELECT COUNT(*) FROM ingress_effect_receipts), (SELECT COUNT(*) FROM ingress_messages WHERE terminal_at IS NOT NULL), (SELECT COUNT(*) FROM ingress_messages)",
        (),
    ).await.expect("receipt state");
    let row = rows.next().await.expect("row").expect("counts");
    assert_eq!(row.get::<i64>(2).expect("canonical rows"), 1);
    (
        row.get(0).expect("receipts"),
        row.get(1).expect("terminal rows"),
    )
}

async fn connection_reply_receipt_after_transport_write(
    state: Arc<WebSocketState>,
    transport_lost: bool,
    resumable: bool,
    nested_owner: bool,
) {
    connection_reply_receipt_after_transport_write_with_remote(
        state,
        transport_lost,
        resumable,
        nested_owner,
        false,
    )
    .await;
}

async fn connection_reply_receipt_after_transport_write_with_remote(
    state: Arc<WebSocketState>,
    transport_lost: bool,
    resumable: bool,
    nested_owner: bool,
    remote_owner: bool,
) {
    use super::super::batch_write::{
        write_ingress_response_batch_with_admission, BatchAuthority, BatchSmPolicy,
        BatchWriteOutcome,
    };
    use axum::extract::ws::Message;
    let mut conn = connection(&state, resumable).await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut responses = super::super::frame::handle_xmpp_frame_with_admission(
        &offered_message(),
        "example.com",
        &state,
        &mut conn,
        &permit,
        &shutdown,
    )
    .await;
    assert_eq!(responses.frames.len(), 1);
    assert_eq!(responses.ingress_reports.len(), 1);
    #[cfg(not(feature = "clustering"))]
    assert!(!nested_owner && !remote_owner);
    #[cfg(feature = "clustering")]
    let mut pending_remote_receipts =
        crate::clustering::relay::frame_receipts::PendingReplyReceipts::default();
    #[cfg(feature = "clustering")]
    if nested_owner {
        let report = responses.ingress_reports.pop().expect("owner report");
        let completion = crate::ingress::execute::RelayFrameReceiptCompletion::new(
            crate::clustering::route_bridge::RelayFrameCompletion {
                authority: Arc::clone(&state.deps.protocol.ingress),
                report,
            },
        );
        let completion = if remote_owner {
            use crate::clustering::ordered_relay::{
                OrderedRelayAck, OrderedRelayChannel, OrderedRelayOrigin, OrderedRelayRecipient,
                OrderedRelaySequence,
            };
            let token = pending_remote_receipts
                .register(completion)
                .expect("owner token");
            let ack = OrderedRelayAck {
                reply_receipt: Some(token),
                channel: OrderedRelayChannel {
                    origin: OrderedRelayOrigin::SmSession(
                        waddle_xmpp::pending_delivery::SmSessionId::new("remote-origin"),
                    ),
                    recipient: OrderedRelayRecipient::BareJid(
                        "room@example.com".parse().expect("room"),
                    ),
                    target_epoch: waddle_xmpp::ownership::ClaimEpoch(0),
                },
                sequence: OrderedRelaySequence::FIRST,
                duplicate: false,
                next_expected: OrderedRelaySequence::FIRST,
                client_replies: responses
                    .frames
                    .iter()
                    .map(|frame| {
                        let super::super::frame::ResponseFrame::Stanza(stanza) = frame else {
                            panic!("owner reply must be a stanza");
                        };
                        crate::clustering::codec::RemoteStanza((**stanza).clone())
                    })
                    .collect(),
            };
            let encoded = serde_json::to_vec(&ack).expect("encode owner ACK");
            let decoded: OrderedRelayAck =
                serde_json::from_slice(&encoded).expect("receive owner ACK");
            let (frames, completion) = decoded.into_frame_delivery(
                crate::clustering::NodeId::new("owner".to_owned()),
                shutdown.clone(),
            );
            responses.frames = frames.into_iter().map(Into::into).collect();
            completion.expect("remote receipt follows frames")
        } else {
            completion
        };
        let mut origin_report = crate::ingress::ExecutionReport::default();
        origin_report.retain_relay_frame_completion(completion);
        responses.ingress_reports.push(origin_report);
    }
    assert_eq!(frame_receipt_state(&state).await, (0, 0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let sink_started = started.clone();
    let sink_release = release.clone();
    let mut sink = Box::pin(futures::sink::unfold((), move |(), _: Message| {
        let started = sink_started.clone();
        let release = sink_release.clone();
        async move {
            started.notify_one();
            let _permit = release.acquire().await.expect("release write");
            if transport_lost {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            } else {
                Ok(())
            }
        }
    }));
    let mut reader = futures::stream::pending::<Result<Message, std::io::Error>>();
    let mut writing = Box::pin(write_ingress_response_batch_with_admission(
        &mut sink,
        &mut reader,
        &state,
        &mut conn,
        &mut responses,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    ));
    tokio::select! {
        _ = started.notified() => {},
        _ = &mut writing => panic!("batch must await transport completion"),
    }
    assert_eq!(
        frame_receipt_state(&state).await,
        (0, 0),
        "transport write is still pending"
    );
    release.add_permits(1);
    let report = writing.await;
    if transport_lost {
        assert!(matches!(report.outcome, BatchWriteOutcome::TransportClosed));
        assert_eq!(frame_receipt_state(&state).await, (0, 0));
    } else {
        assert!(matches!(report.outcome, BatchWriteOutcome::Continue));
        assert_eq!(frame_receipt_state(&state).await, (1, 1));
    }
}

#[tokio::test]
async fn ingress_reply_receipt_waits_for_successful_connection_batch_write() {
    connection_reply_receipt_after_transport_write(
        create_test_websocket_state().await,
        false,
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn ingress_reply_transport_loss_leaves_canonical_row_non_terminal() {
    connection_reply_receipt_after_transport_write(
        create_test_websocket_state().await,
        true,
        true,
        false,
    )
    .await;
}

async fn postgres_connection_reply_receipt(transport_lost: bool, nested_owner: bool) {
    postgres_connection_reply_receipt_with_remote(transport_lost, nested_owner, false).await;
}

async fn postgres_connection_reply_receipt_with_remote(
    transport_lost: bool,
    nested_owner: bool,
    remote_owner: bool,
) {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping PostgreSQL connection reply receipt test: WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres admin");
    let schema = format!("ingress_reply_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("schema");
    let mut url = url::Url::parse(&database_url).expect("database URL");
    url.query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"));
    let pool = Arc::new(
        DatabasePool::new(
            DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string()),
            PoolConfig,
        )
        .await
        .expect("postgres pool"),
    );
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(pool),
            ..Default::default()
        },
    )
    .await;
    connection_reply_receipt_after_transport_write_with_remote(
        state,
        transport_lost,
        false,
        nested_owner,
        remote_owner,
    )
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
    admin.close().await;
}

#[tokio::test]
async fn ingress_reply_receipt_waits_for_successful_connection_batch_write_postgres() {
    postgres_connection_reply_receipt(false, false).await;
}

#[tokio::test]
async fn ingress_reply_transport_loss_leaves_canonical_row_non_terminal_postgres() {
    postgres_connection_reply_receipt(true, false).await;
}

#[tokio::test]
async fn ingress_reply_authority_revocation_leaves_canonical_row_non_terminal() {
    use super::super::batch_write::{
        write_ingress_response_batch_with_admission, BatchAuthority, BatchSmPolicy,
        BatchWriteOutcome,
    };
    use axum::extract::ws::Message;
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut responses = super::super::frame::handle_xmpp_frame_with_admission(
        &offered_message(),
        "example.com",
        &state,
        &mut conn,
        &permit,
        &shutdown,
    )
    .await;
    assert_eq!(responses.frames.len(), 1);
    shutdown.cancel();
    let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sink_written = written.clone();
    let mut sink = Box::pin(futures::sink::unfold((), move |(), _: Message| {
        sink_written.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::future::ready(Ok::<(), std::io::Error>(()))
    }));
    let mut reader = futures::stream::pending::<Result<Message, std::io::Error>>();
    let report = write_ingress_response_batch_with_admission(
        &mut sink,
        &mut reader,
        &state,
        &mut conn,
        &mut responses,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert!(matches!(
        report.outcome,
        BatchWriteOutcome::AuthorityRevoked
    ));
    assert_eq!(written.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(frame_receipt_state(&state).await, (0, 0));
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_local_owner_reply_receipt_waits_for_origin_batch_write() {
    connection_reply_receipt_after_transport_write(
        create_test_websocket_state().await,
        false,
        true,
        true,
    )
    .await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_local_owner_reply_transport_loss_preserves_pending_receipts() {
    connection_reply_receipt_after_transport_write(
        create_test_websocket_state().await,
        true,
        true,
        true,
    )
    .await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_local_owner_reply_receipt_waits_for_origin_batch_write_postgres() {
    postgres_connection_reply_receipt(false, true).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_local_owner_reply_transport_loss_preserves_pending_receipts_postgres() {
    postgres_connection_reply_receipt(true, true).await;
}

#[path = "ingress_authority_recovery.rs"]
mod recovery;

#[path = "ingress_authority_timeout.rs"]
mod timeout;

#[cfg(feature = "clustering")]
#[path = "ingress_authority_relay.rs"]
mod relay;

#[path = "ingress_authority_fence.rs"]
mod fence;

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_remote_owner_ack_transport_loss_preserves_pending_receipts() {
    connection_reply_receipt_after_transport_write_with_remote(
        create_test_websocket_state().await,
        true,
        true,
        true,
        true,
    )
    .await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_remote_owner_ack_transport_loss_preserves_pending_receipts_postgres() {
    postgres_connection_reply_receipt_with_remote(true, true, true).await;
}
