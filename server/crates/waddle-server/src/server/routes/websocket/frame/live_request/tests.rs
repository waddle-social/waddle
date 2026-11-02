use super::{
    capture, validate, ConnectionParts, FrameHandoff, FrameProvenance, UnsupportedFrame,
    CAPTURE_RESULT,
};
use crate::auth::session::Session;
use crate::clustering::NodeLifecycle;
use crate::ingress::{IngressPrincipal, IngressStreamIdentity, IngressSubmission};
use crate::server::routes::interpret::effects::{IngressPlan, RoomExecutionPath};
use crate::server::routes::interpret::OrderedRelayInboundSequence;
use crate::server::routes::websocket::frame::handle_xmpp_frame_with_admission;
use crate::server::routes::websocket::state::WsConnState;
use crate::server::routes::websocket::test_state::create_test_session;
use crate::server::routes::websocket::tests::create_test_websocket_state;
use crate::server::routes::websocket::{ConnectionPhase, Stanza};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, NormalizedTarget, SmIngressId, TransportGeneration,
    WireHandledCount,
};
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::SmClaimFence;
use xmpp_parsers::message::{Message, MessageType};

fn parts(connection: &WsConnState) -> ConnectionParts<'_> {
    ConnectionParts {
        phase: &connection.phase,
        authenticated_session: &connection.authenticated_session,
        ingress_generation: &connection.ingress_generation,
        sm_state: &connection.sm_state,
        sm_ingress_fence: &connection.sm_ingress_fence,
        sm_inbound_completion: &connection.sm_inbound_completion,
    }
}

#[test]
fn live_request_is_not_cloneable() {
    // If a Clone implementation is added, both implementations become
    // applicable and the inferred marker is ambiguous at compile time.
    trait AmbiguousIfClone<Marker> {
        fn marker() {}
    }
    impl<T: ?Sized> AmbiguousIfClone<()> for T {}
    impl<T: ?Sized + Clone> AmbiguousIfClone<u8> for T {}
    let _ = <FrameProvenance<'_> as AmbiguousIfClone<_>>::marker;
}

fn fixture() -> (WsConnState, IngressSubmission, OrderedRelayInboundSequence) {
    let mut connection = WsConnState::new();
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
    connection.phase = ConnectionPhase::ready(sender, false);
    let session = Session::new("alice@example.com", "alice", "alice");
    let principal = session.authenticated_principal_ref().expect("principal");
    connection.authenticated_session = Some(session);
    let owner = NodeIdentity::new("test-node", "test-incarnation");
    let claim_epoch = ClaimEpoch(3);
    connection.sm_ingress_fence = Some(SmClaimFence::new(owner.clone(), claim_epoch));
    connection
        .sm_state
        .enable("live-stream".into(), true, Some(300));
    let sequence = connection
        .sm_inbound_completion
        .reserve(&connection.sm_state);

    let target = NormalizedTarget::Bare("bob@example.com".parse().expect("target"));
    let mut message = Message::new(Some("bob@example.com".parse().expect("to")));
    message.from = Some("alice@example.com/web".parse().expect("from"));
    message.type_ = MessageType::Chat;
    message.bodies.insert(Default::default(), "hello".into());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "live-origin");
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let submission = IngressSubmission {
        identity: IngressStreamIdentity::Resumable {
            stream_id: SmSessionId::new("live-stream"),
            sm_ingress_id: SmIngressId::new(),
            #[cfg(feature = "clustering")]
            owner,
            #[cfg(feature = "clustering")]
            claim_epoch,
            reserved_wire_position: WireHandledCount::from_storage(sequence.0),
            checkpoint_h: WireHandledCount::from_storage(
                connection.sm_inbound_completion.checkpoint_for(sequence),
            ),
        },
        principal: IngressPrincipal::Authenticated(principal),
        sender: "alice@example.com/web".parse().expect("sender"),
        target,
        plan: IngressPlan {
            failure: None,
            rejection: None,
            plan: Vec::new(),
            intents: Vec::new(),
            sanitized_message: message,
            room_canonical_message: None,
            error_reply: None,
            room_execution: RoomExecutionPath::None,
        },
        digest_input,
        connection_generation: TransportGeneration::Connection(connection.ingress_generation),
    };
    (connection, submission, sequence)
}

#[test]
fn capture_requires_the_current_principal_stream_and_reservation() {
    let lifecycle = NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = CancellationToken::new();
    let admission = Some((&permit, &shutdown));
    let (mut connection, submission, sequence) = fixture();
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &submission
        ),
        Ok(())
    );

    let mut wrong_principal = submission.clone();
    wrong_principal.principal = IngressPrincipal::Authenticated(
        Session::new("alice@example.com", "alice", "alice")
            .authenticated_principal_ref()
            .expect("other session"),
    );
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &wrong_principal
        ),
        Err(UnsupportedFrame::Principal)
    );

    let mut wrong_sender = submission.clone();
    wrong_sender.sender = "alice@example.com/other".parse().expect("other resource");
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &wrong_sender
        ),
        Err(UnsupportedFrame::Principal)
    );

    let mut wrong_stream = submission.clone();
    if let IngressStreamIdentity::Resumable { stream_id, .. } = &mut wrong_stream.identity {
        *stream_id = SmSessionId::new("other-stream");
    }
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &wrong_stream
        ),
        Err(UnsupportedFrame::Stream)
    );

    #[cfg(feature = "clustering")]
    {
        let mut wrong_claim = submission.clone();
        if let IngressStreamIdentity::Resumable { claim_epoch, .. } = &mut wrong_claim.identity {
            *claim_epoch = ClaimEpoch(4);
        }
        assert_eq!(
            validate(
                &parts(&connection),
                admission,
                Some(sequence),
                false,
                &wrong_claim
            ),
            Err(UnsupportedFrame::Stream)
        );
    }

    let mut wrong_checkpoint = submission.clone();
    if let IngressStreamIdentity::Resumable { checkpoint_h, .. } = &mut wrong_checkpoint.identity {
        *checkpoint_h = WireHandledCount::from_storage(sequence.0.wrapping_add(1));
    }
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &wrong_checkpoint
        ),
        Err(UnsupportedFrame::Reservation)
    );
    connection.sm_inbound_completion.abandon(sequence);
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &submission
        ),
        Err(UnsupportedFrame::Reservation)
    );
}

#[test]
fn capture_declines_unsupported_inputs_and_revoked_admission() {
    let lifecycle = NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = CancellationToken::new();
    let admission = Some((&permit, &shutdown));
    let (connection, submission, sequence) = fixture();
    assert_eq!(
        validate(
            &parts(&connection),
            None,
            Some(sequence),
            false,
            &submission
        ),
        Err(UnsupportedFrame::Admission)
    );
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            true,
            &submission
        ),
        Err(UnsupportedFrame::Room)
    );

    let mut no_origin = submission.clone();
    no_origin.plan.sanitized_message.payloads.clear();
    no_origin.digest_input = DigestInput::from_parsed(
        &no_origin.plan.sanitized_message,
        &DigestContext {
            target: no_origin.target.clone(),
            server_authorities: vec![no_origin.sender.to_bare()],
            stanza_lang: None,
        },
    )
    .expect("digest without origin");
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &no_origin
        ),
        Err(UnsupportedFrame::Origin)
    );

    let mut ephemeral = submission.clone();
    let IngressPrincipal::Authenticated(principal) = &ephemeral.principal else {
        panic!("authenticated fixture");
    };
    ephemeral.identity = IngressStreamIdentity::Ephemeral {
        principal: principal.clone(),
    };
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &ephemeral
        ),
        Err(UnsupportedFrame::Stream)
    );

    let mut rejected = submission.clone();
    rejected.plan.failure =
        Some(crate::server::routes::interpret::effects::PlanFailure::CarbonInventoryRead);
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &rejected
        ),
        Err(UnsupportedFrame::Plan)
    );

    shutdown.cancel();
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &submission
        ),
        Err(UnsupportedFrame::Admission)
    );
    let shutdown = CancellationToken::new();
    let admission = Some((&permit, &shutdown));

    lifecycle.begin_drain();
    assert_eq!(
        validate(
            &parts(&connection),
            admission,
            Some(sequence),
            false,
            &submission
        ),
        Err(UnsupportedFrame::Admission)
    );
    assert!(matches!(
        capture(
            parts(&connection),
            admission,
            Some(sequence),
            false,
            submission
        ),
        FrameHandoff::Legacy(_)
    ));
}

#[tokio::test]
async fn native_frame_uses_the_live_handoff_and_existing_commit() {
    let state = create_test_websocket_state().await;
    let mut connection = WsConnState::new();
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
    connection.phase = ConnectionPhase::ready(sender.clone(), false);
    connection.authenticated_session = Some(create_test_session(&state, "alice").await);
    create_test_session(&state, "bob").await;
    connection.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        sender,
        false,
        Default::default(),
    );
    let stream = SmSessionId::new("live-frame-handoff");
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim(stream.as_str())
            .await
            .expect("claim"),
    );
    state
        .deps
        .protocol
        .ingress
        .enroll_stream(&stream)
        .await
        .expect("enroll");
    connection.sm_ingress_fence = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(stream.as_str());
    connection
        .sm_state
        .enable(stream.as_str().to_owned(), true, Some(300));
    let mut message = Message::new(Some("bob@example.com".parse().expect("target")));
    message.type_ = MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "a real planned message".into());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "live-frame-origin");
    let wire =
        crate::server::routes::websocket::transport_xml::stanza_to_xml(&Stanza::Message(message));
    let lifecycle = NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = CancellationToken::new();
    CAPTURE_RESULT
        .scope(std::cell::Cell::new(None), async {
            let _ = handle_xmpp_frame_with_admission(
                &wire,
                "example.com",
                &state,
                &mut connection,
                &permit,
                &shutdown,
            )
            .await;
            assert_eq!(CAPTURE_RESULT.with(std::cell::Cell::get), Some(Ok(())));
        })
        .await;
    assert_eq!(connection.sm_state.inbound_count(), 1);
    let mut without_origin = Message::new(Some("bob@example.com".parse().expect("target")));
    without_origin.type_ = MessageType::Chat;
    without_origin
        .bodies
        .insert(Default::default(), "legacy message".into());
    let wire = crate::server::routes::websocket::transport_xml::stanza_to_xml(&Stanza::Message(
        without_origin,
    ));
    CAPTURE_RESULT
        .scope(std::cell::Cell::new(None), async {
            let _ = handle_xmpp_frame_with_admission(
                &wire,
                "example.com",
                &state,
                &mut connection,
                &permit,
                &shutdown,
            )
            .await;
            assert_eq!(
                CAPTURE_RESULT.with(std::cell::Cell::get),
                Some(Err(UnsupportedFrame::Origin))
            );
        })
        .await;
    assert_eq!(connection.sm_state.inbound_count(), 2);
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&stream)
            .await
            .expect("checkpoint")
            .expect("stream")
            .to_storage(),
        2
    );
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db");
    let mut rows = db
        .query("SELECT COUNT(*) FROM ingress_sm_refs", ())
        .await
        .expect("refs");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("number"),
        2
    );
}
