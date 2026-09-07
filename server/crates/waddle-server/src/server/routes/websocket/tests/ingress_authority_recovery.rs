use super::*;
use crate::ingress::commit::commit_hooks;
use crate::ingress_uow::IngressUowError;
use waddle_xmpp::stream_management::{DetachedSession, DetachedSessionSnapshot};

async fn row_count(state: &WebSocketState, table: &str) -> i64 {
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    let mut rows = db
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .expect("count");
    rows.next()
        .await
        .expect("row")
        .expect("count row")
        .get(0)
        .expect("integer")
}

async fn assert_ack(state: &WebSocketState, conn: &mut WsConnState, expected: u32) {
    let frames = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmRequest::to_xml(),
        "example.com",
        state,
        conn,
    )
    .await;
    let ack: minidom::Element = frames.first().expect("ACK frame").parse().expect("ACK XML");
    assert!(ack.is("a", waddle_xmpp::stream_management::SM_NS));
    assert_eq!(
        ack.attr("h")
            .expect("h")
            .parse::<u32>()
            .expect("unsigned h"),
        expected
    );
}

fn snapshot(conn: &mut WsConnState) -> DetachedSession {
    conn.sm_state
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice@example.com".into(),
            jid: "alice@example.com/web".parse().expect("jid"),
            occupancy_session: conn.occupancy_session,
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: vec![],
            pending_subscribes_flushed: false,
        })
        .expect("resumable snapshot")
}

async fn resume_snapshot(
    state: &WebSocketState,
    stale: DetachedSession,
    session: &crate::auth::Session,
    expected: u32,
) -> (
    WsConnState,
    mpsc::Receiver<waddle_xmpp::registry::OutboundStanza>,
) {
    store_resumable_detached_session(state, session, stale.clone()).await;
    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&stale.jid);
    resumed.authenticated_session = Some(session.clone());
    let resume = minidom::Element::builder("resume", waddle_xmpp::stream_management::SM_NS)
        .attr(
            minidom::rxml::xml_ncname!("previd").to_owned(),
            stale.stream_id,
        )
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
        .build();
    let wire = super::super::super::transport_xml::element_to_xml(resume);
    let frames = handle_xmpp_frame(&wire, "example.com", state, &mut resumed).await;
    let reply: minidom::Element = frames
        .first()
        .expect("resume frame")
        .parse()
        .expect("resume XML");
    assert!(
        reply.is("resumed", waddle_xmpp::stream_management::SM_NS),
        "{frames:?}"
    );
    assert_eq!(
        reply
            .attr("h")
            .expect("h")
            .parse::<u32>()
            .expect("h integer"),
        expected
    );
    let (sender, receiver) = mpsc::channel(8);
    let mut pending = Some(sender);
    let registered = super::super::super::registration::register_bound_connection_after_frame(
        state,
        "example.com",
        &mut resumed,
        &mut pending,
    )
    .await;
    assert!(matches!(
        registered,
        super::super::super::registration::RegistrationAfterFrame::Registered(_)
    ));
    (resumed, receiver)
}

/// XEP-0198 §4–§6: exhausted serializable attempts do not acknowledge the stanza.
#[tokio::test]
async fn serialization_exhaustion_is_a_resumable_wire_hole() {
    serialization_exhaustion_is_a_resumable_wire_hole_case(create_test_websocket_state().await)
        .await;
}

async fn serialization_exhaustion_is_a_resumable_wire_hole_case(state: Arc<WebSocketState>) {
    let mut conn = connection(&state, true).await;
    assert_ack(&state, &mut conn, 0).await;
    let observed = commit_hooks::SERIALIZATION_FAILURES
        .scope(
            std::cell::Cell::new(100),
            observed_dispatch(&state, &mut conn, &offered_message()),
        )
        .await;
    assert_eq!(
        observed,
        crate::ingress::IngressDecisionClass::SerializationExhaustion
    );
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert_ack(&state, &mut conn, 0).await;
    assert_eq!(row_count(&state, "ingress_messages").await, 0);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 0);
}

/// XEP-0198 §5–§6: retry after a failed commit uses the first fresh ordinal.
#[tokio::test]
async fn ambiguous_uncommitted_resume_retransmission_gets_fresh_binding() {
    recovery_after_ambiguity(create_test_websocket_state().await, false).await;
}

/// XEP-0198 §4–§5: a lost commit result resumes at the durable handled count.
#[tokio::test]
async fn ambiguous_committed_resume_covers_message_without_retransmission() {
    recovery_after_ambiguity(create_test_websocket_state().await, true).await;
}

async fn recovery_after_ambiguity(state: Arc<WebSocketState>, landed: bool) {
    let mut conn = connection(&state, true).await;
    let stale = snapshot(&mut conn);
    let session = conn.authenticated_session.clone().expect("session");
    assert_ack(&state, &mut conn, 0).await;
    let wire = offered_message();
    let dispatch = handle_xmpp_frame(&wire, "example.com", &state, &mut conn);
    let frames = if landed {
        commit_hooks::AMBIGUOUS_COMMIT.scope(true, dispatch).await
    } else {
        commit_hooks::FAILURE
            .scope(
                std::cell::RefCell::new(Some(IngressUowError::AmbiguousCommit)),
                dispatch,
            )
            .await
    };
    assert!(frames.is_empty());
    assert_eq!(conn.sm_state.get_inbound_count(), 0);
    assert_eq!(
        row_count(&state, "ingress_messages").await,
        i64::from(landed)
    );
    drop(conn);
    let (mut resumed, _receiver) =
        resume_snapshot(&state, stale, &session, u32::from(landed)).await;
    if !landed {
        let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut resumed).await;
        assert!(
            frames.iter().any(|frame| frame.contains("bad-request")),
            "frames={frames:?}"
        );
    }
    assert_ack(&state, &mut resumed, 1).await;
    assert_eq!(row_count(&state, "ingress_messages").await, 1);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 1);
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    let mut rows = db
        .query(
            "SELECT CAST(ingress_ordinal AS BIGINT) FROM ingress_sm_refs",
            (),
        )
        .await
        .expect("ordinal");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("binding")
            .get::<i64>(0)
            .expect("ordinal"),
        1
    );
}

/// XEP-0198 §4–§5: the checkpoint covers deferred IQ completion even without detach.
#[tokio::test]
async fn deferred_iq_ack_survives_crash_without_detach_and_new_positions_do_not_collide() {
    deferred_iq_ack_survives_crash_without_detach_and_new_positions_do_not_collide_case(
        create_test_websocket_state().await,
    )
    .await;
}

async fn deferred_iq_ack_survives_crash_without_detach_and_new_positions_do_not_collide_case(
    state: Arc<WebSocketState>,
) {
    let mut conn = connection(&state, true).await;
    let stale = snapshot(&mut conn);
    let session = conn.authenticated_session.clone().expect("session");
    let iq = conn.sm_inbound_completion.reserve(&conn.sm_state);
    for _ in 0..2 {
        handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn).await;
    }
    assert_ack(&state, &mut conn, 0).await;
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 2);
    conn.sm_inbound_completion.complete(iq, &mut conn.sm_state);
    assert_ack(&state, &mut conn, 3).await;
    drop(conn);
    let (mut resumed, _receiver) = resume_snapshot(&state, stale, &session, 3).await;
    for h in [4, 5] {
        let frames =
            handle_xmpp_frame(&offered_message(), "example.com", &state, &mut resumed).await;
        assert!(
            frames.iter().any(|frame| frame.contains("bad-request")),
            "frames={frames:?}"
        );
        assert_ack(&state, &mut resumed, h).await;
    }
    assert_eq!(row_count(&state, "ingress_messages").await, 4);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 4);
}

/// XEP-0198 §4: the exposed counter and durable checkpoint wrap modulo 2^32.
#[tokio::test]
async fn wire_checkpoint_wraps_after_max_unsigned_int() {
    wire_checkpoint_wraps_after_max_unsigned_int_case(create_test_websocket_state().await).await;
}

async fn wire_checkpoint_wraps_after_max_unsigned_int_case(state: Arc<WebSocketState>) {
    let mut conn = connection(&state, true).await;
    let mut detached = snapshot(&mut conn);
    detached.inbound_count = u32::MAX;
    conn.sm_state.restore_from_session(&detached);
    assert_ack(&state, &mut conn, u32::MAX).await;
    handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn).await;
    assert_ack(&state, &mut conn, 0).await;
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&SmSessionId::new("authority-connection"))
            .await
            .expect("checkpoint")
            .expect("stream")
            .to_storage(),
        0
    );
    assert_eq!(row_count(&state, "ingress_messages").await, 1);
}

/// XEP-0198 §6: principal loss on a stream without resume terminates with not-authorized.
#[tokio::test]
async fn ephemeral_principal_loss_is_not_authorized_and_writes_no_rows() {
    ephemeral_principal_loss_is_not_authorized_and_writes_no_rows_case(
        create_test_websocket_state().await,
    )
    .await;
}

async fn ephemeral_principal_loss_is_not_authorized_and_writes_no_rows_case(
    state: Arc<WebSocketState>,
) {
    let mut conn = connection(&state, false).await;
    let frames = commit_hooks::FAILURE
        .scope(
            std::cell::RefCell::new(Some(IngressUowError::PrincipalAssertionFailed)),
            handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn),
        )
        .await;
    let error: minidom::Element = frames.first().expect("stream error").parse().expect("XML");
    assert!(error.is("error", waddle_xmpp::ns::STREAM));
    assert!(error
        .get_child("not-authorized", xmpp_parsers::ns::XMPP_STREAMS)
        .is_some());
    assert_eq!(row_count(&state, "ingress_messages").await, 0);
    assert_eq!(row_count(&state, "ingress_effect_intents").await, 0);
}

async fn observed_dispatch(
    state: &WebSocketState,
    conn: &mut WsConnState,
    wire: &str,
) -> crate::ingress::IngressDecisionClass {
    commit_hooks::OBSERVED_CLASS
        .scope(std::cell::Cell::new(None), async {
            handle_xmpp_frame(wire, "example.com", state, conn).await;
            commit_hooks::OBSERVED_CLASS
                .with(std::cell::Cell::get)
                .expect("actual transaction decision")
        })
        .await
}

/// XEP-0198 §4: accepted, consistent, and repaired decisions each advance only after commit.
#[tokio::test]
async fn accepted_consistent_repaired_and_divergent_aliases_advance_the_wire_checkpoint() {
    accepted_consistent_repaired_and_divergent_aliases_advance_the_wire_checkpoint_case(
        create_test_websocket_state().await,
    )
    .await;
}

async fn accepted_consistent_repaired_and_divergent_aliases_advance_the_wire_checkpoint_case(
    state: Arc<WebSocketState>,
) {
    use crate::ingress::IngressDecisionClass;
    let mut conn = connection(&state, true).await;
    create_test_session(&state, "bob").await;
    let (initial_sender, mut initial_receiver) = tokio::sync::mpsc::channel(8);
    register_test_connection(
        &state,
        &"bob@example.com/original-device"
            .parse()
            .expect("original recipient"),
        initial_sender,
    )
    .await;
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("recipient")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "same accepted payload".into());
    // No-store removes clock-dependent archive/activity projections: this fixture
    // holds every planned obligation stable until the audience is deliberately changed.
    message
        .payloads
        .push(minidom::Element::builder("no-store", waddle_xmpp::xep::xep0334::NS_HINTS).build());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "reconciliation-wire");
    let wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message.clone()));
    assert_ack(&state, &mut conn, 0).await;
    assert_eq!(
        observed_dispatch(&state, &mut conn, &wire).await,
        IngressDecisionClass::Accepted
    );
    assert_ack(&state, &mut conn, 1).await;
    initial_receiver
        .try_recv()
        .expect("first acceptance reaches original recipient");
    assert_eq!(
        observed_dispatch(&state, &mut conn, &wire).await,
        IngressDecisionClass::ExistingConsistent
    );
    assert_ack(&state, &mut conn, 2).await;
    let archives = row_count(&state, "mam_messages").await;
    assert_eq!(
        archives, 0,
        "XEP-0334 no-store suppresses archive projections"
    );
    // Remove a non-authoritative routing obligation, simulating an omitted projection.
    // ArchiveAuthoritative remains intact and continues assigning canonical identity.
    {
        let db = state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .expect("database");
        db.execute("DELETE FROM ingress_effect_receipts WHERE kind = 1", ())
            .await
            .expect("remove route receipt");
        let removed = db
            .execute("DELETE FROM ingress_effect_intents WHERE kind = 1", ())
            .await
            .expect("remove route intent");
        assert!(removed > 0, "first commit must record a RouteDirect intent");
    }
    assert_eq!(
        observed_dispatch(&state, &mut conn, &wire).await,
        IngressDecisionClass::ExistingRepaired
    );
    assert_ack(&state, &mut conn, 3).await;
    assert_eq!(row_count(&state, "ingress_messages").await, 1);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 3);
    assert_eq!(row_count(&state, "mam_messages").await, archives);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    register_test_connection(
        &state,
        &"bob@example.com/new-device".parse().expect("new recipient"),
        sender,
    )
    .await;
    assert_eq!(
        observed_dispatch(&state, &mut conn, &wire).await,
        IngressDecisionClass::ExistingDivergent
    );
    assert_ack(&state, &mut conn, 4).await;
    assert!(
        receiver.try_recv().is_err(),
        "retry must not fan out to a newly connected nonsender"
    );
    assert_eq!(row_count(&state, "ingress_messages").await, 1);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 4);
    assert_eq!(row_count(&state, "mam_messages").await, archives);
    message
        .bodies
        .insert(Default::default(), "conflicting content".into());
    let changed_wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    commit_hooks::OBSERVED_CLASS
        .scope(std::cell::Cell::new(None), async {
            let frames = handle_xmpp_frame(&changed_wire, "example.com", &state, &mut conn).await;
            assert_eq!(
                commit_hooks::OBSERVED_CLASS.with(std::cell::Cell::get),
                Some(IngressDecisionClass::AliasConflict)
            );
            assert!(frames.iter().any(|frame| {
                let element: minidom::Element = frame.parse().expect("error XML");
                element
                    .get_child("error", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
                    .is_some_and(|error| {
                        error
                            .get_child("conflict", xmpp_parsers::ns::XMPP_STANZAS)
                            .is_some()
                    })
            }));
        })
        .await;
    assert_ack(&state, &mut conn, 5).await;
    assert_eq!(row_count(&state, "ingress_messages").await, 2);
    assert_eq!(row_count(&state, "ingress_origin_aliases").await, 1);
    assert_eq!(row_count(&state, "mam_messages").await, archives);
}

/// XEP-0198 §4–§6: PostgreSQL counterpart of the connection scenario.
#[tokio::test]
async fn serialization_exhaustion_is_a_resumable_wire_hole_postgres() {
    postgres_case(serialization_exhaustion_is_a_resumable_wire_hole_case).await;
}

/// XEP-0198 §4–§6: PostgreSQL counterpart of the connection scenario.
#[tokio::test]
async fn deferred_iq_ack_survives_crash_without_detach_and_new_positions_do_not_collide_postgres() {
    postgres_case(
        deferred_iq_ack_survives_crash_without_detach_and_new_positions_do_not_collide_case,
    )
    .await;
}

/// XEP-0198 §4–§6: PostgreSQL counterpart of the connection scenario.
#[tokio::test]
async fn wire_checkpoint_wraps_after_max_unsigned_int_postgres() {
    postgres_case(wire_checkpoint_wraps_after_max_unsigned_int_case).await;
}

/// XEP-0198 §4–§6: PostgreSQL counterpart of the connection scenario.
#[tokio::test]
async fn ephemeral_principal_loss_is_not_authorized_and_writes_no_rows_postgres() {
    postgres_case(ephemeral_principal_loss_is_not_authorized_and_writes_no_rows_case).await;
}

/// XEP-0198 §4–§6: PostgreSQL counterpart of the connection scenario.
#[tokio::test]
async fn accepted_consistent_repaired_and_divergent_aliases_advance_the_wire_checkpoint_postgres() {
    postgres_case(
        accepted_consistent_repaired_and_divergent_aliases_advance_the_wire_checkpoint_case,
    )
    .await;
}

/// XEP-0198 §4–§5: PostgreSQL ambiguity before a commit is retransmitted.
#[tokio::test]
async fn ambiguous_uncommitted_resume_retransmission_gets_fresh_binding_postgres() {
    postgres_case(|state| recovery_after_ambiguity(state, false)).await;
}

/// XEP-0198 §4–§5: PostgreSQL committed ambiguity resumes from the checkpoint.
#[tokio::test]
async fn ambiguous_committed_resume_covers_message_without_retransmission_postgres() {
    postgres_case(|state| recovery_after_ambiguity(state, true)).await;
}

pub(super) async fn postgres_case<F, Fut>(run: F)
where
    F: FnOnce(Arc<WebSocketState>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping PostgreSQL authority recovery: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres admin");
    let schema = format!("ingress_recovery_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("schema");
    let mut url = url::Url::parse(&database_url).expect("URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
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
    run(state).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
    admin.close().await;
}

/// XEP-0198 §4: IQ and presence positions remain contiguous with authority messages.
#[tokio::test]
async fn mixed_iq_presence_and_messages_have_one_contiguous_checkpoint() {
    mixed_stanza_case(create_test_websocket_state().await).await;
}

/// XEP-0198 §4: PostgreSQL counterpart of mixed stanza counting.
#[tokio::test]
async fn mixed_iq_presence_and_messages_have_one_contiguous_checkpoint_postgres() {
    postgres_case(mixed_stanza_case).await;
}

async fn mixed_stanza_case(state: Arc<WebSocketState>) {
    let mut conn = connection(&state, true).await;
    let ping = minidom::Element::builder("iq", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "mixed-ping")
        .append(minidom::Element::builder("ping", xmpp_parsers::ns::PING).build())
        .build();
    let iq = super::super::super::transport_xml::element_to_xml(ping);
    let presence = super::super::super::transport_xml::stanza_to_xml(&Stanza::Presence(
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable),
    ));
    let message = offered_message();
    for (index, wire) in [&message, &iq, &presence, &message].into_iter().enumerate() {
        handle_xmpp_frame(wire, "example.com", &state, &mut conn).await;
        assert_ack(
            &state,
            &mut conn,
            u32::try_from(index + 1).expect("counter"),
        )
        .await;
    }
    assert_eq!(row_count(&state, "ingress_messages").await, 2);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 2);
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&SmSessionId::new("authority-connection"))
            .await
            .expect("checkpoint")
            .expect("stream")
            .to_storage(),
        4
    );
}

/// XEP-0198 §4 and §6: capture exhaustion commits a resource-constraint reply.
#[tokio::test]
async fn capture_overflow_is_an_advancing_committed_rejection() {
    capture_overflow_case(create_test_websocket_state().await).await;
}

/// XEP-0198 §4 and §6: PostgreSQL counterpart of bounded capture exhaustion.
#[tokio::test]
async fn capture_overflow_is_an_advancing_committed_rejection_postgres() {
    postgres_case(capture_overflow_case).await;
}

async fn capture_overflow_case(state: Arc<WebSocketState>) {
    let mut conn = connection(&state, true).await;
    create_test_session(&state, "bob").await;
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("recipient")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "capture limit".into());
    let wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    assert_ack(&state, &mut conn, 0).await;
    commit_hooks::OBSERVED_CLASS
        .scope(std::cell::Cell::new(None), async {
            let frames = crate::ingress::TEST_CAPTURE_LIMIT
                .scope(
                    0,
                    handle_xmpp_frame(&wire, "example.com", &state, &mut conn),
                )
                .await;
            assert_eq!(
                commit_hooks::OBSERVED_CLASS.with(std::cell::Cell::get),
                Some(crate::ingress::IngressDecisionClass::CaptureOverflow)
            );
            assert!(frames.iter().any(|frame| {
                let element: minidom::Element = frame.parse().expect("error XML");
                element
                    .get_child("error", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
                    .is_some_and(|error| {
                        error
                            .get_child("resource-constraint", xmpp_parsers::ns::XMPP_STANZAS)
                            .is_some()
                    })
            }));
        })
        .await;
    assert_ack(&state, &mut conn, 1).await;
    assert_eq!(row_count(&state, "ingress_messages").await, 1);
    assert_eq!(row_count(&state, "ingress_effect_intents").await, 1);
    assert_eq!(row_count(&state, "ingress_sm_refs").await, 1);
    assert_eq!(row_count(&state, "mam_messages").await, 0);
}

/// XEP-0198 §4 and §6: standard room denials are committed before being acknowledged.
#[tokio::test]
async fn authorization_and_policy_room_denials_advance_h() {
    room_denials_case(create_test_websocket_state().await).await;
}

/// XEP-0198 §4 and §6: PostgreSQL counterpart of committed room denials.
#[tokio::test]
async fn authorization_and_policy_room_denials_advance_h_postgres() {
    postgres_case(room_denials_case).await;
}

async fn room_denials_case(state: Arc<WebSocketState>) {
    use crate::ingress::IngressDecisionClass;
    let mut conn = connection(&state, true).await;
    let room: jid::BareJid = "denial@muc.example.com".parse().expect("room");
    get_or_create_room_actor(
        &state,
        &room,
        RoomConfig::default(),
        "space".into(),
        "denial".into(),
    )
    .await
    .expect("existing room");
    let mut decline = xmpp_parsers::message::Message::new(Some(room.into()));
    decline.type_ = xmpp_parsers::message::MessageType::Normal;
    decline.payloads.push(
        minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(
                minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .attr(
                        minidom::rxml::xml_ncname!("to").to_owned(),
                        "bob@example.com",
                    )
                    .build(),
            )
            .build(),
    );
    let mut absent = xmpp_parsers::message::Message::new(Some(
        "absent@muc.example.com/nobody"
            .parse()
            .expect("absent occupant"),
    ));
    absent.type_ = xmpp_parsers::message::MessageType::Chat;
    absent.bodies.insert(
        Default::default(),
        "missing private-message destination".into(),
    );
    for (h, message, expected, condition) in [
        (
            1,
            decline,
            IngressDecisionClass::AuthorizationDenied,
            "forbidden",
        ),
        (
            2,
            absent,
            IngressDecisionClass::PolicyDenied,
            "item-not-found",
        ),
    ] {
        assert_ack(&state, &mut conn, h - 1).await;
        let wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
        commit_hooks::OBSERVED_CLASS
            .scope(std::cell::Cell::new(None), async {
                let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
                assert_eq!(
                    commit_hooks::OBSERVED_CLASS.with(std::cell::Cell::get),
                    Some(expected)
                );
                assert!(
                    frames.iter().any(|frame| {
                        let element: minidom::Element = frame.parse().expect("error XML");
                        element
                            .get_child("error", waddle_xmpp_core::xep0201::CLIENT_STANZA_NS)
                            .is_some_and(|error| {
                                error
                                    .get_child(condition, xmpp_parsers::ns::XMPP_STANZAS)
                                    .is_some()
                            })
                    }),
                    "expected {condition}: {frames:?}"
                );
            })
            .await;
        assert_ack(&state, &mut conn, h).await;
        assert_eq!(row_count(&state, "ingress_messages").await, i64::from(h));
        assert_eq!(row_count(&state, "ingress_sm_refs").await, i64::from(h));
    }
    assert_eq!(row_count(&state, "mam_messages").await, 0);
}

#[path = "ingress_authority_matrix.rs"]
mod matrix;

/// XEP-0198 §5–§6: a retained snapshot cannot resume without ingress authority.
#[tokio::test]
async fn resume_without_ingress_enrollment_fails_closed() {
    resume_without_ingress_enrollment_case(create_test_websocket_state().await).await;
}

#[tokio::test]
async fn resume_without_ingress_enrollment_fails_closed_postgres() {
    postgres_case(resume_without_ingress_enrollment_case).await;
}

async fn resume_without_ingress_enrollment_case(state: Arc<WebSocketState>) {
    let mut conn = connection(&state, true).await;
    let stale = snapshot(&mut conn);
    let session = conn.authenticated_session.clone().expect("session");
    store_resumable_detached_session(&state, &session, stale.clone()).await;
    drop(conn);
    state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database")
        .execute("DELETE FROM ingress_sm_streams", ())
        .await
        .expect("remove ingress enrollment");
    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&stale.jid);
    resumed.authenticated_session = Some(session);
    let resume = minidom::Element::builder("resume", waddle_xmpp::stream_management::SM_NS)
        .attr(
            minidom::rxml::xml_ncname!("previd").to_owned(),
            stale.stream_id,
        )
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
        .build();
    let wire = super::super::super::transport_xml::element_to_xml(resume);
    let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut resumed).await;
    assert_eq!(frames.len(), 1);
    let reply: minidom::Element = frames[0].parse().expect("resume response XML");
    assert!(reply.is("failed", waddle_xmpp::stream_management::SM_NS));
    assert!(reply
        .get_child("internal-server-error", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());
    assert!(!resumed.sm_state.enabled);
    assert_eq!(row_count(&state, "ingress_sm_streams").await, 0);
}
