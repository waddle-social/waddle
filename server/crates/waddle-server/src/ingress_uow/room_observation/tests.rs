mod reply_fallback;

use chrono::Utc;
use jid::BareJid;
use waddle_extensions::{
    ConfiguredRoomObserver, ExtensionPayload, ObservationGeneration, PluginId,
    RoomObservationOutcome, RoomObservationResult, RoomObservationScope,
    RoomObservationSubscription, Sha256Digest, XmlElement,
};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, SemanticDigest};
use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

use super::work::{eligible_for_claim, retry_delay_ms};
use super::{
    initialize_room_observations, CapturedRoomSource, ObservationError,
    RoomObservationRepository as Repo,
};
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository,
};

fn room() -> BareJid {
    "room@conference.example.org".parse().expect("room")
}
fn sender() -> BareJid {
    "author@example.org".parse().expect("sender")
}

fn configured_observer(generation: u64, identity_byte: char) -> ConfiguredRoomObserver {
    ConfiguredRoomObserver {
        plugin: PluginId::new("observer-fixture").expect("plugin"),
        generation: ObservationGeneration::new(generation).expect("generation"),
        identity: Sha256Digest::new(identity_byte.to_string().repeat(64)).expect("identity"),
        scope: RoomObservationScope::Rooms(vec![room()]),
        max_concurrent: 1,
    }
}

fn subscription(observer: &ConfiguredRoomObserver) -> RoomObservationSubscription {
    RoomObservationSubscription {
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        room: room(),
    }
}

fn intent(observer: &ConfiguredRoomObserver) -> IngressEffectIntent {
    IngressEffectIntent::RoomObserver {
        room: room(),
        requester: sender(),
        sender: "room@conference.example.org/author"
            .parse()
            .expect("occupant"),
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        correction_target: None,
    }
}

fn correction_intent(observer: &ConfiguredRoomObserver, target: &str) -> IngressEffectIntent {
    let mut frozen = intent(observer);
    let IngressEffectIntent::RoomObserver {
        correction_target, ..
    } = &mut frozen
    else {
        unreachable!("fixture intent is a room observer")
    };
    *correction_target = Some(StanzaId::new(target, room().into()));
    frozen
}

fn message(wire_id: &str, stanza_id: &str, origin_id: Option<&str>, body: &str) -> Message {
    let mut message = Message::new(Some(room().into()));
    message.from = Some(
        "room@conference.example.org/author"
            .parse()
            .expect("occupant"),
    );
    message.type_ = MessageType::Groupchat;
    message.id = Some(Id(wire_id.to_string()));
    message.bodies.insert(Lang::new(), body.to_string());
    add_stanza_id(&mut message, &StanzaId::new(stanza_id, room().into()));
    if let Some(origin_id) = origin_id {
        add_origin_id(&mut message, origin_id);
    }
    message
}

fn payload() -> ExtensionPayload {
    let ns =
        waddle_extensions::PayloadNamespace::new("urn:waddle:safety-scores:1").expect("namespace");
    ExtensionPayload::new(
        ns.clone(),
        XmlElement::new(ns, "safety-scores", vec![], vec![]).expect("element"),
    )
    .expect("payload")
}

async fn record_message(tx: &mut crate::ingress_uow::IngressUowTransaction<'_>, key: MessageKey) {
    CanonicalMessageRepository::record_message(
        tx,
        key,
        &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical row");
}

async fn capture(
    tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    key: MessageKey,
    room: &BareJid,
    message: &Message,
    sender: &BareJid,
    intents: &[IngressEffectIntent],
    observed_at: chrono::DateTime<Utc>,
) -> Result<(), ObservationError> {
    EffectIntentRepository::reconcile(tx, key, intents, false)
        .await
        .map_err(|_| ObservationError::Database)?;
    let correction_target = intents.iter().find_map(|intent| match intent {
        IngressEffectIntent::RoomObserver {
            correction_target, ..
        } => correction_target.as_ref(),
        _ => None,
    });
    Repo::capture(
        tx,
        CapturedRoomSource {
            key,
            room,
            message,
            sender,
            intents,
            observed_at,
            correction_target,
        },
    )
    .await
}

#[test]
fn retry_delay_is_bounded() {
    assert_eq!(retry_delay_ms(1), 1_000);
    assert_eq!(retry_delay_ms(2), 2_000);
    assert_eq!(retry_delay_ms(20), 60_000);
}

#[test]
fn locked_reread_does_not_exhaust_a_live_final_attempt() {
    // A competing worker can read a pending attempt 19 before this worker
    // leases attempt 20, then reach the locked row only after that lease.
    assert!(eligible_for_claim("pending", 1_000, None, 1_000));
    assert!(!eligible_for_claim("leased", 1_000, Some(181_000), 1_001));
    assert!(eligible_for_claim("leased", 1_000, Some(181_000), 181_000));
}

async fn capture_rollback_duplicate_and_monotonic_generation(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let key = MessageKey::new();
    let root = message(
        "wire-root",
        "room-stanza-root",
        Some("root-origin"),
        "question?",
    );
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("begin");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &root,
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture");
    drop(tx);
    assert_eq!(fixture.count("extension_room_sources").await, 0);
    assert_eq!(fixture.count("extension_room_observation_work").await, 0);

    let mut tx = fixture.uow.begin().await.expect("retry");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &root,
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture");
    capture(
        &mut tx,
        key,
        &room(),
        &root,
        &sender(),
        &[intent(&observer)],
        now + chrono::Duration::seconds(1),
    )
    .await
    .expect("duplicate");
    let newer = configured_observer(2, 'b');
    Repo::sync_configured(&mut tx, std::slice::from_ref(&newer))
        .await
        .expect("advance");
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("stale skip");
    assert_eq!(
        Repo::sync_configured(&mut tx, &[configured_observer(2, 'c')]).await,
        Err(ObservationError::IdentityConflict)
    );
    tx.commit().await.expect("commit");
    assert_eq!(fixture.count("extension_room_sources").await, 1);
    assert_eq!(fixture.count("extension_room_observation_work").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn capture_rollback_duplicate_and_monotonic_generation_sqlite() {
    capture_rollback_duplicate_and_monotonic_generation(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn capture_rollback_duplicate_and_monotonic_generation_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_capture").await {
        capture_rollback_duplicate_and_monotonic_generation(fixture).await;
    }
}

async fn lease_fence_and_publication_share_receipt_commit(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let intent = intent(&observer);
    let key = MessageKey::new();
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("capture");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &message(
            "wire-root",
            "room-stanza-root",
            Some("root-origin"),
            "question?",
        ),
        &sender(),
        std::slice::from_ref(&intent),
        now,
    )
    .await
    .expect("capture");
    tx.commit().await.expect("commit capture");

    let start = now.timestamp_millis();
    let mut tx = fixture.uow.begin().await.expect("claim");
    let first = Repo::claim(&mut tx, &subscription, start)
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("lease");
    let mut tx = fixture.uow.begin().await.expect("blocked claim");
    assert!(Repo::claim(&mut tx, &subscription, start + 1)
        .await
        .expect("claim")
        .is_none());
    tx.commit().await.expect("commit");
    let mut tx = fixture.uow.begin().await.expect("reclaim");
    let second = Repo::claim(&mut tx, &subscription, start + 180_001)
        .await
        .expect("reclaim")
        .expect("work");
    assert_ne!(first.lease, second.lease);
    tx.commit().await.expect("new lease");

    let outcome = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    let mut tx = fixture.uow.begin().await.expect("finish");
    assert!(!Repo::finish(&mut tx, &first, &outcome, start + 180_002)
        .await
        .expect("stale finish"));
    assert!(Repo::finish(&mut tx, &second, &outcome, start + 180_002)
        .await
        .expect("finish"));
    let receipts = EffectReceiptRepository::keys(&mut tx, key)
        .await
        .expect("receipts");
    assert!(receipts.contains(&crate::ingress::receipt_key(&intent).expect("key")));
    drop(tx);
    assert_eq!(fixture.count("extension_room_publications").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);

    let mut tx = fixture.uow.begin().await.expect("finish again");
    assert!(Repo::finish(&mut tx, &second, &outcome, start + 180_002)
        .await
        .expect("finish"));
    tx.commit().await.expect("result commit");
    assert_eq!(fixture.count("extension_room_publications").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE body = ''")
            .await,
        1,
        "completed work no longer retains the message body"
    );
    let mut tx = fixture.uow.begin().await.expect("publish");
    let publication = Repo::publication(&mut tx, &subscription)
        .await
        .expect("publication")
        .expect("pending");
    let mut other_observer = observer.clone();
    other_observer.plugin = PluginId::new("other-observer").expect("plugin");
    Repo::sync_configured(&mut tx, &[other_observer.clone()])
        .await
        .expect("other plugin");
    let mut mismatched = publication.clone();
    mismatched.subscription.plugin = other_observer.plugin;
    assert_eq!(
        Repo::assert_publication(&mut tx, &mismatched).await,
        Err(ObservationError::PublicationConflict)
    );
    assert!(Repo::assert_publication(&mut tx, &publication)
        .await
        .expect("assert"));
    assert!(Repo::mark_published(&mut tx, &publication.id)
        .await
        .expect("mark"));
    tx.commit().await.expect("publish commit");
    fixture.close().await;
}

#[tokio::test]
async fn lease_fence_and_publication_share_receipt_commit_sqlite() {
    lease_fence_and_publication_share_receipt_commit(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn lease_fence_and_publication_share_receipt_commit_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_result").await {
        lease_fence_and_publication_share_receipt_commit(fixture).await;
    }
}

async fn empty_correction_and_retraction_cancel_prior_work(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let key = MessageKey::new();
    let edit_key = MessageKey::new();
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("root");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &message(
            "wire-root",
            "room-stanza-root",
            Some("root-origin"),
            "question?",
        ),
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture root");
    tx.commit().await.expect("root commit");

    let mut edit = message("wire-edit", "room-stanza-edit", Some("edit-origin"), "");
    edit.payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "wire-root",
        ));
    let mut tx = fixture.uow.begin().await.expect("edit");
    record_message(&mut tx, edit_key).await;
    capture(
        &mut tx,
        edit_key,
        &room(),
        &edit,
        &sender(),
        &[correction_intent(&observer, "room-stanza-root")],
        now + chrono::Duration::seconds(1),
    )
    .await
    .expect("empty edit");
    assert!(
        Repo::claim(&mut tx, &subscription, now.timestamp_millis() + 1_000)
            .await
            .expect("claim")
            .is_none()
    );
    assert_eq!(
        EffectReceiptRepository::keys(&mut tx, key)
            .await
            .expect("root receipts")
            .len(),
        1
    );
    assert_eq!(
        EffectReceiptRepository::keys(&mut tx, edit_key)
            .await
            .expect("edit receipts")
            .len(),
        1
    );
    tx.commit().await.expect("edit commit");
    assert_eq!(
        f_count_scrubbed_work(&fixture).await,
        1,
        "superseded work no longer retains the message body"
    );

    let mut tx = fixture.uow.begin().await.expect("retract");
    Repo::retract(
        &mut tx,
        &room(),
        &StanzaId::new("room-stanza-root", room().into()),
    )
    .await
    .expect("retract");
    tx.commit().await.expect("retract commit");
    fixture.close().await;
}

#[tokio::test]
async fn empty_correction_and_retraction_cancel_prior_work_sqlite() {
    empty_correction_and_retraction_cancel_prior_work(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn empty_correction_and_retraction_cancel_prior_work_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_empty_edit").await {
        empty_correction_and_retraction_cancel_prior_work(fixture).await;
    }
}

async fn f_count_scrubbed_work(fixture: &IngressFixture) -> i64 {
    fixture
        .count("extension_room_observation_work WHERE body = ''")
        .await
}

#[tokio::test]
async fn distinct_room_sources_can_hold_distinct_leases() {
    let fixture = IngressFixture::sqlite().await;
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("capture");
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    for (wire, stanza, origin) in [
        ("wire-first", "stanza-first", "origin-first"),
        ("wire-second", "stanza-second", "origin-second"),
    ] {
        let key = MessageKey::new();
        record_message(&mut tx, key).await;
        capture(
            &mut tx,
            key,
            &room(),
            &message(wire, stanza, Some(origin), "body"),
            &sender(),
            &[intent(&observer)],
            now,
        )
        .await
        .expect("capture source");
    }
    tx.commit().await.expect("capture commit");
    let mut tx = fixture.uow.begin().await.expect("claims");
    let first = Repo::claim(&mut tx, &subscription, now.timestamp_millis())
        .await
        .expect("first claim")
        .expect("first work");
    let second = Repo::claim(&mut tx, &subscription, now.timestamp_millis())
        .await
        .expect("second claim")
        .expect("second work");
    assert_ne!(first.id, second.id);
    assert_ne!(first.lease, second.lease);
    tx.commit().await.expect("claims commit");
    fixture.close().await;
}

#[tokio::test]
async fn explicit_generation_revocation_disposes_work_and_its_ingress_receipt() {
    let fixture = IngressFixture::sqlite().await;
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let key = MessageKey::new();
    let frozen_intent = intent(&observer);
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("capture");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &message("wire-root", "room-stanza-root", Some("root-origin"), "body"),
        &sender(),
        std::slice::from_ref(&frozen_intent),
        now,
    )
    .await
    .expect("capture");
    tx.commit().await.expect("capture commit");

    let mut revoked = configured_observer(2, 'b');
    revoked.scope = RoomObservationScope::Rooms(vec![]);
    let mut tx = fixture.uow.begin().await.expect("revoke");
    Repo::sync_configured(&mut tx, &[revoked.clone()])
        .await
        .expect("revoke");
    assert!(Repo::claim(&mut tx, &subscription, now.timestamp_millis())
        .await
        .expect("claim")
        .is_none());
    assert!(
        Repo::due_rooms(&mut tx, &revoked, None, now.timestamp_millis(), 16)
            .await
            .expect("due rooms")
            .is_empty()
    );
    assert!(EffectReceiptRepository::keys(&mut tx, key)
        .await
        .expect("receipts")
        .contains(&crate::ingress::receipt_key(&frozen_intent).expect("key")));
    tx.commit().await.expect("revoke commit");
    fixture.close().await;
}

#[tokio::test]
async fn due_room_cursor_wraps_within_configured_scope() {
    let fixture = IngressFixture::sqlite().await;
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let key = MessageKey::new();
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("capture");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &message("wire-root", "room-stanza-root", Some("root-origin"), "body"),
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture");
    let due = Repo::due_rooms(&mut tx, &observer, None, now.timestamp_millis(), 1)
        .await
        .expect("first page");
    assert_eq!(due, vec![room()]);
    let wrapped = Repo::due_rooms(&mut tx, &observer, Some(&room()), now.timestamp_millis(), 1)
        .await
        .expect("wrapped page");
    assert_eq!(wrapped, vec![room()]);
    tx.commit().await.expect("capture commit");
    fixture.close().await;
}

#[tokio::test]
async fn distinct_sources_can_reuse_the_same_client_wire_id() {
    let fixture = IngressFixture::sqlite().await;
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("sources");
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    for (stanza, origin) in [
        ("room-stanza-first", "origin-first"),
        ("room-stanza-second", "origin-second"),
    ] {
        let key = MessageKey::new();
        record_message(&mut tx, key).await;
        capture(
            &mut tx,
            key,
            &room(),
            &message("reused-client-id", stanza, Some(origin), "body"),
            &sender(),
            &[intent(&observer)],
            now,
        )
        .await
        .expect("independent accepted source");
    }
    tx.commit().await.expect("source commit");
    assert_eq!(fixture.count("extension_room_sources").await, 2);
    assert_eq!(fixture.count("extension_room_observation_work").await, 2);
    fixture.close().await;
}

async fn correction_of_earlier_revision_keeps_the_canonical_source(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("source chain");
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("sync");
    let root_key = MessageKey::new();
    record_message(&mut tx, root_key).await;
    capture(
        &mut tx,
        root_key,
        &room(),
        &message("wire-root", "room-root", Some("origin-root"), "root"),
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("root");
    for (wire, stanza, replaces_wire, target_stanza, body) in [
        (
            "wire-edit-1",
            "room-edit-1",
            "wire-root",
            "room-root",
            "edit one",
        ),
        (
            "wire-edit-2",
            "room-edit-2",
            "wire-edit-1",
            "room-edit-1",
            "edit two",
        ),
        (
            "wire-edit-3",
            "room-edit-3",
            "wire-edit-1",
            "room-edit-1",
            "edit three",
        ),
    ] {
        let key = MessageKey::new();
        record_message(&mut tx, key).await;
        let mut edit = message(wire, stanza, Some(wire), body);
        edit.payloads
            .push(waddle_xmpp::xep::xep0308::build_replace_element(
                replaces_wire,
            ));
        capture(
            &mut tx,
            key,
            &room(),
            &edit,
            &sender(),
            &[correction_intent(&observer, target_stanza)],
            now,
        )
        .await
        .expect("accepted correction");
    }
    tx.commit().await.expect("chain commit");
    assert_eq!(
        fixture
            .count("extension_room_sources WHERE revision = 3")
            .await,
        1
    );
    assert_eq!(fixture.count("extension_room_source_revisions").await, 4);
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'pending' AND revision = 3")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'stale' AND body = ''")
            .await,
        3
    );
    fixture.close().await;
}

#[tokio::test]
async fn correction_of_earlier_revision_keeps_the_canonical_source_sqlite() {
    correction_of_earlier_revision_keeps_the_canonical_source(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn correction_of_earlier_revision_keeps_the_canonical_source_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_edit_chain").await {
        correction_of_earlier_revision_keeps_the_canonical_source(fixture).await;
    }
}

async fn stale_replica_correction_invalidates_new_generation_result(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let old = configured_observer(1, 'a');
    let current = configured_observer(2, 'b');
    let current_subscription = subscription(&current);
    let now = Utc::now();
    let root_key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("root");
    record_message(&mut tx, root_key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&current))
        .await
        .expect("current config");
    capture(
        &mut tx,
        root_key,
        &room(),
        &message("wire-root", "room-root", Some("root-origin"), "root body"),
        &sender(),
        &[intent(&current)],
        now,
    )
    .await
    .expect("root capture");
    tx.commit().await.expect("root commit");

    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = Repo::claim(&mut tx, &current_subscription, now.timestamp_millis())
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("claim commit");
    let mut tx = fixture.uow.begin().await.expect("finish");
    assert!(Repo::finish(
        &mut tx,
        &work,
        &RoomObservationOutcome::Completed(RoomObservationResult {
            payloads: vec![payload()],
            usage: None,
        }),
        now.timestamp_millis(),
    )
    .await
    .expect("finish"));
    tx.commit().await.expect("finish commit");
    assert_eq!(
        fixture
            .count("extension_room_publications WHERE status = 'pending'")
            .await,
        1
    );

    let edit_key = MessageKey::new();
    let mut edit = message("wire-edit", "room-edit", Some("edit-origin"), "new body");
    edit.payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "wire-root",
        ));
    let mut tx = fixture.uow.begin().await.expect("stale replica edit");
    record_message(&mut tx, edit_key).await;
    capture(
        &mut tx,
        edit_key,
        &room(),
        &edit,
        &sender(),
        &[correction_intent(&old, "room-root")],
        now + chrono::Duration::seconds(1),
    )
    .await
    .expect("accepted old-generation correction");
    tx.commit().await.expect("edit commit");
    let no_observer_key = MessageKey::new();
    let mut later = message(
        "wire-later",
        "room-later",
        Some("later-origin"),
        "later body",
    );
    later
        .payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "wire-edit",
        ));
    let authoritative_target = StanzaId::new("room-edit", room().into());
    let mut tx = fixture.uow.begin().await.expect("no-observer correction");
    record_message(&mut tx, no_observer_key).await;
    Repo::capture(
        &mut tx,
        CapturedRoomSource {
            key: no_observer_key,
            room: &room(),
            message: &later,
            sender: &sender(),
            intents: &[],
            observed_at: now + chrono::Duration::seconds(2),
            correction_target: Some(&authoritative_target),
        },
    )
    .await
    .expect("accepted correction without local observer intent");
    tx.commit().await.expect("no-observer edit commit");
    assert_eq!(
        fixture
            .count("extension_room_sources WHERE revision = 2")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_publications WHERE status = 'pending'")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("extension_room_publications WHERE status = 'stale'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'pending'")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE body = ''")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn stale_replica_correction_invalidates_new_generation_result_sqlite() {
    stale_replica_correction_invalidates_new_generation_result(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn stale_replica_correction_invalidates_new_generation_result_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_stale_replica").await {
        stale_replica_correction_invalidates_new_generation_result(fixture).await;
    }
}
