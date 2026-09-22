//! #1805: ordinary local delivery must retain the proof needed by the detach drain.
use super::*;
use crate::ingress::{
    execute::execute_effects,
    maintenance::{run_maintenance_pass, MaintenanceBudget},
    ExternalOutcome, RecoveryEnvironment,
};
use crate::server::routes::interpret::{
    deliver_direct_to_full_with_registered_remote, deliver_peer_to_full_with_registered_remote,
    effects::{delivery::PeerDeliveryKind, ImmediateSink, PlanSink},
};

async fn receipt_fault(fixture: &IngressFixture, enabled: bool) {
    match (fixture.db.driver(), enabled) {
        (crate::db::DatabaseDriver::Sqlite, true) => fixture.execute(
            "CREATE TRIGGER actor_receipt_fault BEFORE INSERT ON ingress_effect_receipts BEGIN SELECT RAISE(FAIL, 'injected actor receipt failure'); END", (),
        ).await,
        (crate::db::DatabaseDriver::Postgres, true) => {
            fixture.execute("CREATE FUNCTION actor_receipt_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected actor receipt failure'; END $$", ()).await;
            fixture.execute("CREATE TRIGGER actor_receipt_fault BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION actor_receipt_fault()", ()).await;
        }
        (crate::db::DatabaseDriver::Sqlite, false) => fixture.execute("DROP TRIGGER actor_receipt_fault", ()).await,
        (crate::db::DatabaseDriver::Postgres, false) => {
            fixture.execute("DROP TRIGGER actor_receipt_fault ON ingress_effect_receipts", ()).await;
            fixture.execute("DROP FUNCTION actor_receipt_fault()", ()).await;
        }
    }
}

async fn local_actor_detach_recovery(
    fixture: IngressFixture,
    kind: PeerDeliveryKind,
    write_before_detach: bool,
) {
    let mut socket = detaching_socket(fixture).await;
    assert!(
        crate::server::dual_registration::mirror_register(
            &socket.state.deps.protocol.user_registry,
            socket.recipient.clone(),
            socket
                .state
                .deps
                .protocol
                .connection_registry
                .get_entry(&socket.recipient)
                .expect("registered socket"),
        )
        .await
    );
    if kind == PeerDeliveryKind::PeerStanza {
        socket.conn.ensure_state_machine(
            "example.com",
            &socket.state.deps.protocol.dispatcher,
            socket.recipient.clone(),
            false,
            Blocklist::empty(),
        );
    }
    let mut submission = socket
        .fixture
        .submission(Some("actor-detach"), "local actor delivery");
    let route_identity = EffectMessageIdentity::capture_ordinal(0);
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: socket.recipient.to_bare(),
            fanout: vec![socket.recipient.clone()],
            route_identity: route_identity.clone(),
        });
    let sink = PlanSink::new();
    let deps = socket.state.recovery_deps();
    let mut planned = deps.clone();
    planned.effects = &sink;
    planned.direct_route_identity = Some(route_identity);
    let stanza = Stanza::Message(submission.plan.sanitized_message.clone());
    match kind {
        PeerDeliveryKind::DirectFrame => {
            deliver_direct_to_full_with_registered_remote(&planned, &socket.recipient, &stanza)
                .await;
        }
        PeerDeliveryKind::PeerStanza => {
            deliver_peer_to_full_with_registered_remote(&planned, &socket.recipient, &stanza).await;
        }
        PeerDeliveryKind::RegistryFrame => unreachable!("actor delivery variants only"),
    }
    submission.plan.plan = sink.take().0;
    assert!(socket.rx.is_empty(), "planning cannot enqueue a stanza");
    let decision = commit_submission(&socket.fixture.uow, &submission, 1)
        .await
        .expect("commit local route");
    assert_eq!(decision.external_receipts.len(), 1);
    receipt_fault(&socket.fixture, true).await;
    let report = execute_effects(
        &socket.fixture.uow,
        &socket.fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(
        socket.rx.len(),
        1,
        "real UserActor accepted one queued stanza"
    );
    assert_eq!(socket.fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(socket.fixture.count("ingress_delivery_receipts").await, 0);
    receipt_fault(&socket.fixture, false).await;
    drop(planned);
    drop(deps);

    if write_before_detach {
        fail_live_write(&mut socket).await;
        assert!(
            socket.rx.is_empty(),
            "the live handler consumed the actor frame"
        );
        assert_eq!(socket.conn.sm_state.queue_len(), 1);
    }
    let detached = detach(&mut socket).await;
    assert_eq!(detached.unacked_stanzas.len(), 1);
    assert_eq!(detached.outbound_count, 1);
    assert_eq!(
        socket.fixture.count("sm_ingress_appends").await,
        1,
        "local actor metadata must authorize a durable proof during real detach"
    );
    assert_eq!(socket.fixture.count("ingress_effect_receipts").await, 0);

    let environment: Arc<dyn RecoveryEnvironment> = socket.state.clone();
    let budget = MaintenanceBudget {
        grace: chrono::Duration::zero(),
        recovery: std::time::Duration::from_secs(20),
        recovery_row: std::time::Duration::from_secs(5),
        hard_deadline: std::time::Duration::from_secs(30),
        ..MaintenanceBudget::DEFAULT
    };
    for _ in 0..2 {
        run_maintenance_pass(
            &socket.fixture.db,
            &socket.fixture.uow,
            budget,
            Some(environment.clone()),
        )
        .await;
        assert_eq!(socket.fixture.count("ingress_effect_receipts").await, 1);
        assert_eq!(
            socket
                .fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            1
        );
        assert_eq!(socket.fixture.count("sm_ingress_appends").await, 1);
        let after = socket.sm.peek_session(STREAM).await.unwrap().unwrap();
        assert_eq!(
            after.unacked_stanzas.len(),
            1,
            "recovery must not duplicate the drained frame"
        );
        assert_eq!(after.outbound_count, 1);
        assert_eq!(
            after.unacked_stanzas[0].stanza_xml,
            detached.unacked_stanzas[0].stanza_xml
        );
    }
    let mut resumed = waddle_xmpp::stream_management::StreamManagementState::new();
    resumed.restore_from_session(&detached);
    assert_eq!(resumed.get_stanzas_to_resend(0).len(), 1);
    resumed.acknowledge(1);
    assert!(resumed.get_stanzas_to_resend(1).is_empty());
    assert!(
        socket
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(std::time::Duration::from_secs(15))
            .await
    );
    drop(environment);
    let DetachingSocket { fixture, .. } = socket;
    fixture.close().await;
}

async fn fail_live_write(socket: &mut DetachingSocket) {
    use crate::server::routes::websocket::{
        outbound::{handle_outbound_stanza, OutboundAuthority},
        timers::TransportTimers,
    };
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("live write permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut sink = Box::pin(futures::sink::unfold((), |(), _: Message| async {
        Err::<(), std::io::Error>(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "transport closed after SM recording",
        ))
    }));
    let mut reader = futures::stream::pending::<Result<Message, std::convert::Infallible>>();
    let queued = socket.rx.try_recv().expect("actor-produced frame");
    assert!(
        !handle_outbound_stanza(
            &mut sink,
            &mut reader,
            &socket.state,
            &mut socket.conn,
            &mut TransportTimers::new(),
            queued,
            OutboundAuthority {
                permit: &permit,
                shutdown: &shutdown
            },
        )
        .await,
        "failed transport write closes the live handler"
    );
}

#[tokio::test]
async fn sqlite_user_actor_direct_detach_recovery() {
    local_actor_detach_recovery(
        IngressFixture::sqlite().await,
        PeerDeliveryKind::DirectFrame,
        false,
    )
    .await;
}
#[tokio::test]
async fn postgres_user_actor_direct_detach_recovery() {
    if let Some(fixture) = IngressFixture::postgres("actor_dir").await {
        local_actor_detach_recovery(fixture, PeerDeliveryKind::DirectFrame, false).await;
    }
}
#[tokio::test]
async fn sqlite_user_actor_peer_detach_recovery() {
    local_actor_detach_recovery(
        IngressFixture::sqlite().await,
        PeerDeliveryKind::PeerStanza,
        false,
    )
    .await;
}
#[tokio::test]
async fn postgres_user_actor_peer_detach_recovery() {
    if let Some(fixture) = IngressFixture::postgres("actor_peer").await {
        local_actor_detach_recovery(fixture, PeerDeliveryKind::PeerStanza, false).await;
    }
}

#[tokio::test]
async fn sqlite_user_actor_direct_failed_write_recovery() {
    local_actor_detach_recovery(
        IngressFixture::sqlite().await,
        PeerDeliveryKind::DirectFrame,
        true,
    )
    .await;
}
#[tokio::test]
async fn postgres_user_actor_direct_failed_write_recovery() {
    if let Some(fixture) = IngressFixture::postgres("ua_wr_dir").await {
        local_actor_detach_recovery(fixture, PeerDeliveryKind::DirectFrame, true).await;
    }
}
#[tokio::test]
async fn sqlite_user_actor_peer_failed_write_recovery() {
    local_actor_detach_recovery(
        IngressFixture::sqlite().await,
        PeerDeliveryKind::PeerStanza,
        true,
    )
    .await;
}
#[tokio::test]
async fn postgres_user_actor_peer_failed_write_recovery() {
    if let Some(fixture) = IngressFixture::postgres("ua_wr_peer").await {
        local_actor_detach_recovery(fixture, PeerDeliveryKind::PeerStanza, true).await;
    }
}
