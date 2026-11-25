//! T2-M4-05: BVA proving that the very first `unknown_after_send` at attempt 1
//! (< MAX_ATTEMPTS = 20) quarantines immediately with zero retry attempts.

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
    "bva@conference.example.org".parse().expect("room")
}

fn sender() -> BareJid {
    "alice@example.org".parse().expect("sender")
}

#[tokio::test]
async fn attempt_one_unknown_after_send_quarantines_immediately_without_attempt_two() {
    let db = Database::in_memory("quarantine_attempt_one")
        .await
        .expect("db");
    MigrationRunner::single().run(&db).await.expect("migrate");
    initialize_room_observations(&db)
        .await
        .expect("init room obs");
    let uow = IngressUnitOfWork::open(db.clone(), LineageConfig::default()).expect("uow");

    let obs = ConfiguredRoomObserver {
        plugin: PluginId::try_new("bva-observer").expect("plugin"),
        generation: ObservationGeneration::try_new(1).expect("gen"),
        identity: Sha256Digest::try_new("b".repeat(64)).expect("id"),
        scope: RoomObservationScope::Rooms(vec![room()]),
        max_concurrent: 1,
    };
    let sub = RoomObservationSubscription {
        plugin: obs.plugin.clone(),
        generation: obs.generation,
        identity: obs.identity.clone(),
        room: room(),
    };
    let now = Utc::now().timestamp_millis();

    for (idx, use_unresolved) in [(1, false), (2, true)] {
        let key = MessageKey::new();
        let mut msg = Message::new(Some(room().into()));
        msg.from = Some("bva@conference.example.org/alice".parse().expect("occ"));
        msg.type_ = MessageType::Groupchat;
        msg.id = Some(Id(format!("wire-{idx}")));
        msg.bodies.insert(Lang::new(), "test".to_owned());
        add_stanza_id(
            &mut msg,
            &StanzaId::new(format!("stanza-{idx}"), room().into()),
        );
        add_origin_id(&mut msg, &format!("orig-{idx}"));

        let intent = IngressEffectIntent::RoomObserver {
            room: room(),
            requester: sender(),
            sender: "bva@conference.example.org/alice".parse().expect("occ"),
            plugin: obs.plugin.clone(),
            generation: obs.generation,
            identity: obs.identity.clone(),
            correction_target: None,
        };
        let mut tx = uow.begin().await.expect("seed");
        CanonicalMessageRepository::record_message(
            &mut tx,
            key,
            &SemanticDigest::from_storage(1, [u8::try_from(idx).expect("u8"); 32]).expect("digest"),
            None,
        )
        .await
        .expect("canonical");
        RoomObservationRepository::sync_configured(&mut tx, std::slice::from_ref(&obs), now)
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
                observed_at: chrono::DateTime::from_timestamp_millis(now).expect("ts"),
                correction_target: None,
            },
        )
        .await
        .expect("capture");
        tx.commit().await.expect("commit");

        let mut tx = uow.begin().await.expect("claim");
        let work = RoomObservationRepository::claim(&mut tx, &sub, now)
            .await
            .expect("claim")
            .expect("work");
        assert_eq!(work.attempt, 1);
        assert!(RoomObservationRepository::start(&mut tx, &work, now)
            .await
            .expect("start"));
        if use_unresolved {
            assert!(RoomObservationRepository::finish(
                &mut tx,
                &work,
                &RoomObservationOutcome::UnresolvedFailure(ObservationFailure::DeadlineExceeded),
                now,
            )
            .await
            .expect("finish"));
        }
        tx.commit().await.expect("commit started");

        // At now + 180_000, no retry attempt 2 may EVER be leased.
        let mut tx = uow.begin().await.expect("after expiry");
        assert!(
            RoomObservationRepository::claim(&mut tx, &sub, now + 180_000)
                .await
                .expect("claim")
                .is_none()
        );
        tx.commit().await.expect("commit");
    }

    let conn = db.guard().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM extension_room_observation_work WHERE status = 'terminal' AND terminal_category = 'unknown_after_send' AND attempt = 1",
            (),
        )
        .await
        .expect("query");
    let count: i64 = rows
        .next()
        .await
        .expect("next")
        .expect("row")
        .get(0)
        .expect("i64");
    assert_eq!(count, 2, "both items must quarantine at exact attempt 1");
}
