//! Offline notification fixtures use the same durable admission boundary as handlers.
use super::*;
use crate::ingress::{IngressEffectCapture, IngressStreamIdentity, IngressSubmission};
use crate::server::routes::interpret::effects::{ImmediateSink, IngressPlan, PlanSink};
use waddle_xmpp::ingress::{ConnectionGeneration, DigestContext, DigestInput, NormalizedTarget};
use waddle_xmpp::protocol::OutboundEvent;

pub(super) async fn commit_offline_events(state: &WebSocketState, events: Vec<OutboundEvent>) {
    let [OutboundEvent::QueueOfflineDelivery {
        original_message, ..
    }] = events.as_slice()
    else {
        panic!("one offline delivery event")
    };
    let message = original_message.as_ref().clone();
    let sender = message
        .from
        .clone()
        .expect("sender")
        .try_into_full()
        .expect("full sender");
    let session = create_test_session(state, sender.node().expect("local sender").as_str()).await;
    let principal = session
        .authenticated_principal_ref()
        .expect("authenticated principal");
    let target = NormalizedTarget::Bare(message.to.as_ref().expect("recipient").to_bare());
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: target.clone(),
            server_authorities: vec![sender.to_bare()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let capture = IngressEffectCapture::new();
    let sink = PlanSink::new();
    let mut deps =
        build_interpret_deps(state, None).with_ingress_effect_capture(Some(capture.clone()));
    deps.effects = &sink;
    crate::server::routes::interpret::interpret(events, &deps).await;
    let (effects, room_execution) = sink.take();
    let submission = IngressSubmission {
        identity: IngressStreamIdentity::Ephemeral {
            principal: principal.clone(),
        },
        principal,
        sender,
        target,
        digest_input,
        connection_generation: ConnectionGeneration::INITIAL,
        plan: IngressPlan {
            failure: None,
            rejection: None,
            plan: effects,
            intents: capture.snapshot().intents,
            sanitized_message: message,
            error_reply: None,
            room_execution,
        },
    };
    let decision = state.deps.protocol.ingress.commit(&submission).await;
    assert!(
        decision.message_key.is_some(),
        "offline admission: {:?}",
        decision.class
    );
    deps.effects = &ImmediateSink;
    let report = state
        .deps
        .protocol
        .ingress
        .execute(&decision, &ImmediateSink, &deps)
        .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(report.terminalization_failure.is_none(), "{report:?}");
    assert!(
        report
            .outcomes
            .iter()
            .all(|outcome| outcome.1 == crate::ingress::ExternalOutcome::Done),
        "{report:?}"
    );
}

pub(super) async fn postgres_state(
    label: &str,
) -> Option<(
    crate::ingress::test_support::IngressFixture,
    Arc<WebSocketState>,
)> {
    let fixture = crate::ingress::test_support::IngressFixture::postgres(label).await?;
    let ingress = Arc::new(fixture.authority().await);
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared Postgres pool");
    let state = database_state(Arc::new(pool), Some(ingress)).await;
    Some((fixture, state))
}

pub(super) async fn sqlite_state() -> Arc<WebSocketState> {
    let pool =
        crate::db::DatabasePool::new(crate::db::DatabaseConfig::default(), crate::db::PoolConfig)
            .await
            .expect("SQLite pool");
    database_state(Arc::new(pool), None).await
}

async fn database_state(
    pool: Arc<crate::db::DatabasePool>,
    ingress: Option<Arc<crate::ingress::IngressAuthority>>,
) -> Arc<WebSocketState> {
    let pending = crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
        pool.global().clone(),
        waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
    )
    .await
    .expect("co-located pending storage");
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(pool),
            ingress,
            pending_delivery_storage: Some(Arc::new(pending)),
            ..Default::default()
        },
    )
    .await
}
