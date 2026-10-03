use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{
    initialize_room_observations, CanonicalMessageRepository, CapturedRoomSource,
    EffectIntentRepository,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use waddle_extensions::{
    ObservationGeneration, PluginId, RoomObservationOutcome, RoomObservationScope, Sha256Digest,
};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, SemanticDigest};

async fn callback_is_not_replayed_after_cancel(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let room: BareJid = "room@conference.example.org".parse().expect("room");
    let observer = ConfiguredRoomObserver {
        plugin: PluginId::new("observer-fixture").expect("plugin"),
        generation: ObservationGeneration::new(1).expect("generation"),
        identity: Sha256Digest::new("a".repeat(64)).expect("identity"),
        scope: RoomObservationScope::Rooms(vec![room.clone()]),
        max_concurrent: 1,
    };
    let subscription = subscription(&observer, room.clone());
    let sender: BareJid = "author@example.org".parse().expect("sender");
    let key = MessageKey::new();
    let intent = IngressEffectIntent::RoomObserver {
        room: room.clone(),
        requester: sender.clone(),
        sender: room.with_resource_str("author").expect("occupant"),
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        correction_target: None,
    };
    let mut message = xmpp_parsers::message::Message::new(Some(room.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "body".into());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "origin");
    waddle_xmpp_core::xep0359::add_stanza_id(
        &mut message,
        &waddle_xmpp_core::xep0359::StanzaId::new("stanza", room.clone().into()),
    );
    let now = chrono::Utc::now();
    let mut tx = fixture.uow.begin().await.expect("capture");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
        None,
    )
    .await
    .expect("message");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("intent");
    RoomObservationRepository::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("config");
    RoomObservationRepository::capture(
        &mut tx,
        CapturedRoomSource {
            key,
            room: &room,
            message: &message,
            sender: &sender,
            intents: &[intent],
            observed_at: now,
            correction_target: None,
        },
    )
    .await
    .expect("capture");
    tx.commit().await.expect("capture commit");
    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = RoomObservationRepository::claim(&mut tx, &subscription, now.timestamp_millis())
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("claim commit");
    let calls = AtomicUsize::new(0);
    let entered = tokio::sync::Notify::new();

    let mut tx = fixture.uow.begin().await.expect("expired start");
    let started =
        RoomObservationRepository::start(&mut tx, &work, now.timestamp_millis() + 180_000)
            .await
            .expect("expired");
    assert!(!started);
    assert!(invoke_after_commit(tx, started, async || {
        calls.fetch_add(1, Ordering::SeqCst);
        RoomObservationOutcome::NotInvoked
    })
    .await
    .expect("declined")
    .is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Dropping the boundary before commit never enters the callback; rollback
    // leaves the lease available to the original token.
    let mut tx = fixture.uow.begin().await.expect("rollback start");
    let started = RoomObservationRepository::start(&mut tx, &work, now.timestamp_millis())
        .await
        .expect("start");
    let unpolled = invoke_after_commit(tx, started, async || {
        calls.fetch_add(1, Ordering::SeqCst);
        RoomObservationOutcome::NotInvoked
    });
    drop(unpolled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let mut tx = fixture.uow.begin().await.expect("start");
    let started = RoomObservationRepository::start(&mut tx, &work, now.timestamp_millis())
        .await
        .expect("start");
    assert!(started);
    {
        let invocation = invoke_after_commit(tx, started, async || {
            // A second connection sees the marker before guest code runs.
            assert_eq!(
                fixture
                    .count("extension_room_observation_work WHERE status = 'started'")
                    .await,
                1
            );
            calls.fetch_add(1, Ordering::SeqCst);
            entered.notify_one();
            std::future::pending().await
        });
        tokio::pin!(invocation);
        tokio::select! {
            result = &mut invocation => panic!("blocked callback ended: {result:?}"),
            _ = entered.notified() => {}
        }
        // A concurrent scheduler cannot steal even an expired started lease.
        let mut tx = fixture.uow.begin().await.expect("concurrent recovery");
        assert!(RoomObservationRepository::claim(
            &mut tx,
            &subscription,
            now.timestamp_millis() + 1_000_000
        )
        .await
        .expect("claim")
        .is_none());
        tx.commit().await.expect("recovery commit");
    } // Cancels the callback, modelling actor shutdown after invocation.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut tx = fixture.uow.begin().await.expect("restart recovery");
    assert!(RoomObservationRepository::claim(
        &mut tx,
        &subscription,
        now.timestamp_millis() + 2_000_000
    )
    .await
    .expect("claim")
    .is_none());
    assert!(RoomObservationRepository::due_rooms(
        &mut tx,
        &observer,
        None,
        now.timestamp_millis() + 2_000_000,
        10
    )
    .await
    .expect("due")
    .is_empty());
    tx.commit().await.expect("recovery commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("extension_room_publications").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn callback_is_not_replayed_after_cancel_sqlite() {
    callback_is_not_replayed_after_cancel(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn callback_is_not_replayed_after_cancel_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_callback_cancel").await {
        callback_is_not_replayed_after_cancel(fixture).await;
    }
}
