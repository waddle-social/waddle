//! T1-F16-03: `room_observation/work.rs` quarantines `unknown_after_send`
//! (expired `started` lease AND `UnresolvedFailure`) as `status = 'terminal',
//! terminal_category = 'unknown_after_send'` without re-dispatching, while
//! safely reclaiming pre-start expired `leased` work.

use chrono::Utc;
use jid::BareJid;
use waddle_extensions::{
    ConfiguredRoomObserver, ObservationFailure, ObservationGeneration, PluginId,
    RoomObservationOutcome, RoomObservationScope, RoomObservationSubscription, Sha256Digest,
};
use waddle_server::config::LineageConfig;
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::ingress_uow::{
    initialize_room_observations, CanonicalMessageRepository, CapturedRoomSource,
    EffectIntentRepository, IngressUnitOfWork, RoomObservationRepository,
};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, SemanticDigest};
use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

fn room() -> BareJid {
    "room@conference.example.org".parse().expect("room")
}

fn sender() -> BareJid {
    "author@example.org".parse().expect("sender")
}

fn observer() -> ConfiguredRoomObserver {
    ConfiguredRoomObserver {
        plugin: PluginId::try_new("observer-quarantine").expect("plugin"),
        generation: ObservationGeneration::try_new(1).expect("gen"),
        identity: Sha256Digest::try_new("a".repeat(64)).expect("identity"),
        scope: RoomObservationScope::Rooms(vec![room()]),
        max_concurrent: 1,
    }
}

fn subscription(obs: &ConfiguredRoomObserver) -> RoomObservationSubscription {
    RoomObservationSubscription {
        plugin: obs.plugin.clone(),
        generation: obs.generation,
        identity: obs.identity.clone(),
        room: room(),
    }
}

fn make_message(stanza_id: &str) -> Message {
    let mut msg = Message::new(Some(room().into()));
    msg.from = Some(
        "room@conference.example.org/author"
            .parse()
            .expect("occupant"),
    );
    msg.type_ = MessageType::Groupchat;
    msg.id = Some(Id(format!("wire-{stanza_id}")));
    msg.bodies.insert(Lang::new(), "hello room".to_owned());
    add_stanza_id(&mut msg, &StanzaId::new(stanza_id, room().into()));
    add_origin_id(&mut msg, &format!("orig-{stanza_id}"));
    msg
}

async fn setup_db() -> (Database, IngressUnitOfWork) {
    let db = Database::in_memory("quarantine_test")
        .await
        .expect("in_memory db");
    let runner = MigrationRunner::single();
    runner.run(&db).await.expect("migrations");
    initialize_room_observations(&db)
        .await
        .expect("init room observations");
    let uow = IngressUnitOfWork::open(db.clone(), LineageConfig::default()).expect("uow");
    (db, uow)
}

async fn seed_work(
    uow: &IngressUnitOfWork,
    obs: &ConfiguredRoomObserver,
    stanza_id: &str,
    now_ms: i64,
) {
    let key = MessageKey::new();
    let msg = make_message(stanza_id);
    let intent = IngressEffectIntent::RoomObserver {
        room: room(),
        requester: sender(),
        sender: "room@conference.example.org/author"
            .parse()
            .expect("occupant"),
        plugin: obs.plugin.clone(),
        generation: obs.generation,
        identity: obs.identity.clone(),
        correction_target: None,
    };
    let mut tx = uow.begin().await.expect("begin seed");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical");
    RoomObservationRepository::sync_configured(&mut tx, std::slice::from_ref(obs), now_ms)
        .await
        .expect("sync");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("reconcile");
    RoomObservationRepository::capture(
        &mut tx,
        CapturedRoomSource {
            key,
            room: &room(),
            message: &msg,
            sender: &sender(),
            intents: &[intent],
            observed_at: chrono::DateTime::from_timestamp_millis(now_ms).expect("ts"),
            correction_target: None,
        },
    )
    .await
    .expect("capture");
    tx.commit().await.expect("commit seed");
}

async fn count_rows(db: &Database, query: &str) -> i64 {
    let conn = db.guard().await.expect("conn");
    let mut rows = conn.query(query, ()).await.expect("query");
    rows.next()
        .await
        .expect("next")
        .expect("row")
        .get::<i64>(0)
        .expect("count")
}

#[tokio::test]
async fn expired_started_lease_and_unresolved_failure_quarantine_without_redispatch() {
    let (db, uow) = setup_db().await;
    let obs = observer();
    let sub = subscription(&obs);
    let now = Utc::now().timestamp_millis();

    // Case 1: started lease expires -> claim() quarantines immediately.
    seed_work(&uow, &obs, "stanza-expired", now).await;
    let mut tx = uow.begin().await.expect("begin claim 1");
    let work = RoomObservationRepository::claim(&mut tx, &sub, now)
        .await
        .expect("claim")
        .expect("work");
    assert_eq!(work.attempt, 1);
    assert!(RoomObservationRepository::start(&mut tx, &work, now)
        .await
        .expect("start"));
    tx.commit().await.expect("commit started");

    // Advance past LEASE_MS (180_000ms): claim() quarantines and returns None.
    let expired_at = now + 180_000;
    let mut tx = uow.begin().await.expect("begin expired claim");
    assert!(RoomObservationRepository::claim(&mut tx, &sub, expired_at)
        .await
        .expect("claim quarantines")
        .is_none());
    assert!(
        RoomObservationRepository::due_rooms(&mut tx, &obs, None, expired_at + 180_000, 10)
            .await
            .expect("due rooms")
            .is_empty()
    );
    tx.commit().await.expect("commit quarantine");

    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM extension_room_observation_work WHERE status = 'terminal' AND terminal_category = 'unknown_after_send' AND body = '' AND lease_id IS NULL AND settled_at_ms IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM extension_room_observation_receipts WHERE category = 'unknown_after_send'"
        )
        .await,
        1
    );

    // Case 2: UnresolvedFailure in finish() quarantines immediately.
    seed_work(&uow, &obs, "stanza-unresolved", expired_at).await;
    let mut tx = uow.begin().await.expect("begin unresolved");
    let work2 = RoomObservationRepository::claim(&mut tx, &sub, expired_at)
        .await
        .expect("claim 2")
        .expect("work 2");
    assert!(
        RoomObservationRepository::start(&mut tx, &work2, expired_at)
            .await
            .expect("start 2")
    );
    assert!(RoomObservationRepository::finish(
        &mut tx,
        &work2,
        &RoomObservationOutcome::UnresolvedFailure(ObservationFailure::RuntimeFailure),
        expired_at,
    )
    .await
    .expect("finish unresolved"));
    tx.commit().await.expect("commit unresolved");

    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM extension_room_observation_work WHERE status = 'terminal' AND terminal_category = 'unknown_after_send'"
        )
        .await,
        2
    );

    // Positive control: a `leased` row that expired BEFORE `start()` IS safely reclaimed.
    seed_work(&uow, &obs, "stanza-prestart", expired_at).await;
    let mut tx = uow.begin().await.expect("begin prestart");
    let leased_only = RoomObservationRepository::claim(&mut tx, &sub, expired_at)
        .await
        .expect("claim prestart")
        .expect("leased work");
    assert_eq!(leased_only.attempt, 1);
    tx.commit().await.expect("commit leased only");

    let mut tx = uow.begin().await.expect("begin reclaim");
    let reclaimed = RoomObservationRepository::claim(&mut tx, &sub, expired_at + 180_000)
        .await
        .expect("reclaim prestart")
        .expect("pre-start expired lease must be reclaimed");
    assert_eq!(reclaimed.attempt, 2);
    tx.commit().await.expect("commit reclaim");
}
