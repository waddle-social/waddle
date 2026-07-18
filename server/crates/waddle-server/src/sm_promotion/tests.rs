use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use jid::{BareJid, FullJid};
use kameo::actor::{ActorRef, Spawn};
use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{
    PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::registry::{
    ConnectionRegistry, OutboundStanza, RegisterUserResource, UserRegistryActor,
};
use waddle_xmpp::stream_management::persistence::SmUnackedStanzaPurpose;
use waddle_xmpp::stream_management::DetachedSession;
use waddle_xmpp::Stanza;

use super::*;

fn full(s: &str) -> FullJid {
    s.parse().unwrap()
}

fn bare(s: &str) -> BareJid {
    s.parse().unwrap()
}

fn resume_barrier_ping_xml(id: &str, to: &FullJid) -> String {
    let iq = xmpp_parsers::iq::Iq::Get {
        from: Some(to.domain().as_str().parse().expect("valid domain JID")),
        to: Some(jid::Jid::from(to.clone())),
        id: id.to_string(),
        payload: minidom::Element::builder("ping", waddle_xmpp::xep::xep0199::NS_PING).build(),
    };
    let element = Stanza::Iq(Box::new(iq)).to_element();
    let mut buffer = Vec::new();
    element.write_to(&mut buffer).expect("serialize ping IQ");
    String::from_utf8(buffer).expect("serialized ping IQ is UTF-8")
}

/// A fresh, empty actor-authoritative registry for `promote_session_unacked`
/// / `promote_displaced_sessions` tests (ADR-0017 Phase 3 Slice 9). Tests
/// that need a live resource register it through this actor directly (the
/// same `RegisterUserResource` path production dual-registration uses).
fn test_user_registry() -> ActorRef<UserRegistryActor> {
    UserRegistryActor::spawn(UserRegistryActor::new())
}

/// Register `jid` on BOTH the DashMap `ConnectionRegistry` and the
/// actor-authoritative `UserRegistryActor`, sharing the SAME
/// `ConnectionEntry` — mirrors production dual-registration
/// (`server::dual_registration::mirror_register`) so a later
/// `registry.update_presence(...)` mutates atomics the actor's cloned entry
/// also observes. ADR-0017 Phase 3 Slice 9: bare-JID selection now reads
/// the actor alone, so tests exercising live delivery must register on
/// both trees, not just the DashMap.
async fn dual_register(
    registry: &ConnectionRegistry,
    user_registry: &ActorRef<UserRegistryActor>,
    jid: FullJid,
    sender: tokio::sync::mpsc::Sender<OutboundStanza>,
) {
    registry.register(jid.clone(), sender);
    let entry = registry
        .get_entry(&jid)
        .expect("just registered on DashMap");
    user_registry
        .ask(RegisterUserResource { jid, entry })
        .await
        .expect("register on actor tree");
}

fn direct_target(
    wire_id: &str,
    author: &str,
    archive: &str,
) -> waddle_xmpp::tombstone::TombstoneTarget {
    waddle_xmpp::tombstone::TombstoneTarget::Direct {
        wire_id: wire_id.to_string(),
        author: bare(author),
        archive: bare(archive),
    }
}

fn detached_session_with_unacked(
    stream_id: &str,
    jid: FullJid,
    unacked_xml: Vec<String>,
) -> DetachedSession {
    let now = Utc::now();
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: "alice".to_string(),
        jid,
        inbound_count: 0,
        outbound_count: unacked_xml.len() as u32,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: unacked_xml
            .into_iter()
            .enumerate()
            .map(
                |(i, xml)| waddle_xmpp::stream_management::DetachedUnackedStanza {
                    sequence: i as u32 + 1,
                    stanza_xml: xml,
                    original_receipt_at: now,
                    purpose: Default::default(),
                },
            )
            .collect(),
        max_resume_time: Some(60),
        detached_at: Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

/// PendingDeliveryStorage whose reads and writes always fail — simulates a
/// down offline-storage backend for promotion-failure tests.
struct AlwaysFailingPending;

#[async_trait::async_trait]
impl PendingDeliveryStorage for AlwaysFailingPending {
    async fn insert(
        &self,
        _row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        Err(
            waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                "simulated backend failure".into(),
            ),
        )
    }
    async fn list(
        &self,
        _recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        Err(
            waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                "simulated backend failure".into(),
            ),
        )
    }
    async fn claim_for_session(
        &self,
        _recipient: &BareJid,
        _session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        Ok(vec![])
    }
    async fn claim_batch_for_session(
        &self,
        _recipient: &BareJid,
        _session: &waddle_xmpp::pending_delivery::SmSessionId,
        _after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        _limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        Ok(vec![])
    }
    async fn delete_claimed(
        &self,
        _session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn delete_row(
        &self,
        _id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn release_claim(
        &self,
        _session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn release_row(
        &self,
        _id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn record_pushed_at(
        &self,
        _id: &waddle_xmpp::pending_delivery::PendingRowId,
        _sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn delete_acked_in_window(
        &self,
        _session: &waddle_xmpp::pending_delivery::SmSessionId,
        _from_exclusive: u32,
        _to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn list_orphaned_claims(
        &self,
        _live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        _claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        Ok(vec![])
    }
    async fn count(
        &self,
        _recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn delete_older_than(
        &self,
        _cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
    async fn scrub_for_tombstone(
        &self,
        _target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        Ok(0)
    }
}

fn dm_xml(from: &str, to: &str, body: &str) -> String {
    let mut m = xmpp_parsers::message::Message::new(Some(to.parse::<jid::Jid>().unwrap()));
    m.from = Some(from.parse::<jid::Jid>().unwrap());
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn mam_replay_xml(child_name: &str) -> String {
    let mut m = xmpp_parsers::message::Message::new(Some(
        "alice@example.com/web".parse::<jid::Jid>().unwrap(),
    ));
    m.from = Some("alice@example.com".parse::<jid::Jid>().unwrap());
    m.type_ = xmpp_parsers::message::MessageType::Normal;
    let payload = match child_name {
        "result" => {
            xmpp_parsers::minidom::Element::builder("result", waddle_xmpp_core::mam::MAM_NS)
                .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "archive-id-1")
                .append(
                    xmpp_parsers::minidom::Element::builder(
                        "forwarded",
                        waddle_xmpp_core::mam::FORWARD_NS,
                    )
                    .build(),
                )
                .build()
        }
        "fin" => xmpp_parsers::minidom::Element::builder("fin", waddle_xmpp_core::mam::MAM_NS)
            .append(
                xmpp_parsers::minidom::Element::builder("set", waddle_xmpp_core::mam::RSM_NS)
                    .build(),
            )
            .build(),
        other => {
            xmpp_parsers::minidom::Element::builder(other, waddle_xmpp_core::mam::MAM_NS).build()
        }
    };
    m.payloads.push(payload);
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn dm_with_mam_payload_xml(from: &str, to: &str, body: &str) -> String {
    let mut m = xmpp_parsers::message::Message::new(Some(to.parse::<jid::Jid>().unwrap()));
    m.from = Some(from.parse::<jid::Jid>().unwrap());
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    m.payloads.push(
        xmpp_parsers::minidom::Element::builder("result", waddle_xmpp_core::mam::MAM_NS)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "archive-id-1")
            .append(
                xmpp_parsers::minidom::Element::builder(
                    "forwarded",
                    waddle_xmpp_core::mam::FORWARD_NS,
                )
                .build(),
            )
            .build(),
    );
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn sm_promotion_metric_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn prometheus_counter_value(rendered: &str, name: &str) -> u64 {
    rendered
        .lines()
        .find_map(|line| {
            line.strip_prefix(name)
                .and_then(|rest| rest.trim().parse::<u64>().ok())
        })
        .unwrap_or_else(|| panic!("missing prometheus counter {name}"))
}

async fn assert_mam_frame_not_promoted_to_pending_delivery(child_name: &str) {
    let _guard = sm_promotion_metric_test_lock().lock().await;
    waddle_xmpp::prometheus::reset_metrics_for_test();

    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/web"),
        vec![mam_replay_xml(child_name)],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.not_promotable, 1);
    assert_eq!(summary.queued, 0);
    assert_eq!(summary.bounced, 0);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);

    let rendered = waddle_xmpp::prometheus::render_metrics();
    assert_eq!(
        prometheus_counter_value(&rendered, "waddle_sm_promotion_not_promotable_total"),
        1
    );

    waddle_xmpp::prometheus::reset_metrics_for_test();
}

#[tokio::test]
async fn promotes_to_pending_delivery_when_no_alt_resource() {
    // Locked Q6 = B step 2: alt-resource fails (no online
    // resources for alice), so pending_delivery insert fires.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec![dm_xml("bob@elsewhere/x", "alice@example.com", "missed me?")],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.queued, 1);
    assert_eq!(summary.redelivered, 0);
    assert_eq!(summary.bounced, 0);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
}

fn session_with_resume_barrier(
    stream_id: &str,
    recipient: FullJid,
    stanza_xml: String,
) -> DetachedSession {
    let mut session = detached_session_with_unacked(stream_id, recipient, Vec::new());
    session.outbound_count = 1;
    session
        .unacked_stanzas
        .push(waddle_xmpp::stream_management::DetachedUnackedStanza {
            sequence: 1,
            stanza_xml,
            original_receipt_at: Utc::now(),
            purpose: SmUnackedStanzaPurpose::ResumeBarrier,
        });
    session
}

#[tokio::test]
async fn resume_barrier_is_pruned_without_live_or_pending_delivery() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let recipient = full("alice@example.com/laptop");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    dual_register(&registry, &user_registry, recipient.clone(), sender).await;
    let session = session_with_resume_barrier(
        "stream-resume-barrier",
        recipient.clone(),
        resume_barrier_ping_xml("resume-barrier-1", &recipient),
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.not_promotable, 1);
    assert_eq!(summary.redelivered, 0);
    assert_eq!(summary.queued, 0);
    assert_eq!(summary.promoted_sequences, vec![1]);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn application_ping_keeps_legacy_live_delivery_behavior() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let recipient = full("alice@example.com/laptop");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    dual_register(&registry, &user_registry, recipient.clone(), sender).await;
    let session = detached_session_with_unacked(
        "stream-application-ping",
        recipient.clone(),
        vec![resume_barrier_ping_xml("application-ping", &recipient)],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.redelivered, 1);
    assert_eq!(summary.not_promotable, 0);
    assert_eq!(summary.promoted_sequences, vec![1]);
    assert!(receiver.try_recv().is_ok());
}

#[tokio::test]
async fn mistagged_resume_barrier_is_retained_fail_closed() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let mut session = detached_session_with_unacked(
        "stream-mistagged-barrier",
        full("alice@example.com/laptop"),
        vec![dm_xml("bob@elsewhere/x", "alice@example.com", "keep me")],
    );
    session.unacked_stanzas[0].purpose = SmUnackedStanzaPurpose::ResumeBarrier;

    let summary = promote_session_unacked(
        &session,
        &ConnectionRegistry::new(),
        &test_user_registry(),
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.storage_failed, 1);
    assert!(summary.promoted_sequences.is_empty());
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
}

async fn assert_barrier_link_failure(outbound_sequence: Option<u32>, row_count: usize) {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let recipient = bare("alice@example.com");
    let source = SmSessionId::new("stream-corrupt-barrier-link");
    let linked_xml = dm_xml("bob@elsewhere/x", "alice@example.com", "retain linked row");
    let Some(Stanza::Message(linked_message)) = parse_stanza(&linked_xml) else {
        panic!("linked pending payload parses as a message");
    };
    let mut row_ids = Vec::new();
    for _ in 0..row_count {
        let row_id = PendingRowId::fresh();
        storage
            .insert(PendingRow {
                id: row_id.clone(),
                recipient: recipient.clone(),
                original_receipt_at: Utc::now(),
                payload: PendingPayload::Transient(Box::new(linked_message.clone())),
                flushed_in_session: Some(source.clone()),
                outbound_sequence,
            })
            .await
            .expect("seed impossible barrier-linked pending row");
        row_ids.push(row_id);
    }

    let recipient_full = full("alice@example.com/laptop");
    let session = session_with_resume_barrier(
        source.as_str(),
        recipient_full.clone(),
        resume_barrier_ping_xml("resume-barrier-linked", &recipient_full),
    );
    let summary = promote_session_unacked(
        &session,
        &ConnectionRegistry::new(),
        &test_user_registry(),
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.storage_failed, 1);
    assert!(summary.promoted_sequences.is_empty());
    let retained = storage.list(&recipient).await.expect("read retained row");
    assert_eq!(retained.len(), row_count);
    assert!(retained.iter().all(|row| row_ids.contains(&row.id)));
    assert!(retained
        .iter()
        .all(|row| row.flushed_in_session.as_ref() == Some(&source)));
    assert!(retained
        .iter()
        .all(|row| row.outbound_sequence == outbound_sequence));
}

#[tokio::test]
async fn resume_barrier_with_exact_pending_link_is_retained_fail_closed() {
    assert_barrier_link_failure(Some(1), 1).await;
}

#[tokio::test]
async fn resume_barrier_with_unsequenced_source_row_is_retained_fail_closed() {
    assert_barrier_link_failure(None, 1).await;
}

#[tokio::test]
async fn resume_barrier_with_duplicate_pending_links_is_retained_fail_closed() {
    assert_barrier_link_failure(Some(1), 2).await;
}

#[tokio::test]
async fn resume_barrier_with_unreadable_pending_links_is_retained_fail_closed() {
    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(AlwaysFailingPending);
    let recipient = full("alice@example.com/laptop");
    let session = session_with_resume_barrier(
        "stream-unreadable-barrier-links",
        recipient.clone(),
        resume_barrier_ping_xml("resume-barrier-list-failure", &recipient),
    );

    let summary = promote_session_unacked(
        &session,
        &ConnectionRegistry::new(),
        &test_user_registry(),
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.storage_failed, 1);
    assert!(summary.promoted_sequences.is_empty());
}

#[tokio::test]
async fn dm_with_mam_payload_is_still_promoted_to_pending_delivery() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec![dm_with_mam_payload_xml(
            "bob@elsewhere/x",
            "alice@example.com",
            "keep despite extension",
        )],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.not_promotable, 0);
    assert_eq!(summary.queued, 1);
    assert_eq!(summary.bounced, 0);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
}

#[tokio::test]
async fn mam_result_frame_is_not_promoted_to_pending_delivery() {
    assert_mam_frame_not_promoted_to_pending_delivery("result").await;
}

#[tokio::test]
async fn mam_fin_frame_is_not_promoted_to_pending_delivery() {
    assert_mam_frame_not_promoted_to_pending_delivery("fin").await;
}

#[tokio::test]
async fn promotes_to_alt_resource_when_one_is_online() {
    // Locked Q6 = B step 1: another resource of the same user
    // is online with non-negative priority — re-route there
    // instead of queueing.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let alt = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    dual_register(&registry, &user_registry, alt.clone(), tx).await;
    registry.update_presence(&alt, true, 1);

    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec![dm_xml(
            "bob@elsewhere/x",
            "alice@example.com",
            "alt resource",
        )],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.redelivered, 1);
    assert_eq!(summary.queued, 0);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
    assert!(rx.try_recv().is_ok(), "stanza pushed to alt resource");
}

#[tokio::test]
async fn bounces_service_unavailable_when_quota_exceeded() {
    // Locked Q6 = B step 3: pending_delivery quota refused →
    // <service-unavailable/> bounced to sender per XEP-0160 §3
    // step 3 + RFC 6120 §8.3.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::new(QuotaPolicy::CountCap {
            max_rows: 0,
        }));
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    // Register the sender so the bounce can be delivered.
    let sender = full("bob@example.com/x");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    dual_register(&registry, &user_registry, sender.clone(), tx).await;
    registry.update_presence(&sender, true, 1);

    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec![dm_xml(
            "bob@example.com/x",
            "alice@example.com",
            "queue full",
        )],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.bounced, 1);
    assert_eq!(summary.queued, 0);
    let bounce = rx.try_recv().expect("bounce stanza pushed back to sender");
    match &bounce.stanza {
        Stanza::Message(m) => {
            assert_eq!(m.type_, xmpp_parsers::message::MessageType::Error);
        }
        _ => panic!("expected Message bounce"),
    }
}

#[tokio::test]
async fn drops_no_store_hint_stanzas_silently() {
    // Classifier returns pending=None for <no-store/>, so the
    // promotion drops without bouncing.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let mut m =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse::<jid::Jid>().unwrap()));
    m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().unwrap());
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), "ephemeral".to_string());
    waddle_xmpp::xep::xep0334::add_hint(&mut m, waddle_xmpp::xep::xep0334::Hint::NoStore);
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).unwrap();
    let xml = String::from_utf8(buf).unwrap();

    let session =
        detached_session_with_unacked("stream-1", full("alice@example.com/laptop"), vec![xml]);

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.dropped, 1);
    assert_eq!(summary.queued, 0);
    assert_eq!(summary.bounced, 0);
}

#[tokio::test]
async fn full_jid_target_falls_back_to_bare_jid_fanout() {
    // Locked Q6 = B + RFC 6121 §8.5.3: when the addressed full
    // JID has gone offline but other resources of the recipient
    // are online, the unacked stanza must reach SOME resource
    // (not just be dropped). This tests the bare-JID fallback
    // path when classifier returns DeliverToFull.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    // Alice's web resource is online; laptop is detached.
    let alt = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    dual_register(&registry, &user_registry, alt.clone(), tx).await;
    registry.update_presence(&alt, true, 1);

    // Stanza was originally addressed to alice/laptop (full JID).
    let xml = {
        let mut m = xmpp_parsers::message::Message::new(Some(
            "alice@example.com/laptop".parse::<jid::Jid>().unwrap(),
        ));
        m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().unwrap());
        m.type_ = xmpp_parsers::message::MessageType::Chat;
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), "hi".to_string());
        let element: xmpp_parsers::minidom::Element = m.into();
        let mut buf = Vec::new();
        element.write_to(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    };

    let session =
        detached_session_with_unacked("stream-1", full("alice@example.com/laptop"), vec![xml]);

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.redelivered, 1);
    assert_eq!(summary.queued, 0);
    assert!(rx.try_recv().is_ok(), "stanza redelivered to /web");
}

#[tokio::test]
async fn bare_jid_target_fans_out_to_all_routable_resources() {
    // Locked Q6 step 1 + RFC 6121 §8.5.2: bare-JID-addressed
    // stanzas fan out to every non-negative-priority resource,
    // not just the highest-priority one. (Copilot review on
    // PR #346: prior code only delivered to one.)
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let web = full("alice@example.com/web");
    let mobile = full("alice@example.com/mobile");
    let (tx_web, mut rx_web) = tokio::sync::mpsc::channel(8);
    let (tx_mobile, mut rx_mobile) = tokio::sync::mpsc::channel(8);
    dual_register(&registry, &user_registry, web.clone(), tx_web).await;
    dual_register(&registry, &user_registry, mobile.clone(), tx_mobile).await;
    registry.update_presence(&web, true, 1);
    registry.update_presence(&mobile, true, 1);

    let session = detached_session_with_unacked(
        "stream-laptop",
        full("alice@example.com/laptop"),
        vec![dm_xml("bob@elsewhere/x", "alice@example.com", "fanout")],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.redelivered, 1);
    // Both resources receive the stanza.
    assert!(rx_web.try_recv().is_ok(), "web received fanout");
    assert!(rx_mobile.try_recv().is_ok(), "mobile received fanout");
}

#[tokio::test]
async fn iq_unacked_promoted_to_alt_resource_when_addressed_resource_online() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let target = full("alice@example.com/laptop");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(target.clone(), tx);

    // Build an IQ result addressed to alice/laptop.
    let iq_xml = {
        let iq = xmpp_parsers::iq::Iq::Result {
            from: Some("server.example/srv".parse().expect("valid full jid")),
            to: Some("alice@example.com/laptop".parse().expect("valid full jid")),
            id: "iq-1".to_string(),
            payload: None,
        };
        let element: xmpp_parsers::minidom::Element = iq.into();
        let mut buf = Vec::new();
        element.write_to(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    };

    let session =
        detached_session_with_unacked("stream-1", full("alice@example.com/laptop"), vec![iq_xml]);

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.redelivered, 1);
    assert_eq!(
        summary.unparseable, 0,
        "IQ must not be classified Unparseable"
    );
    assert!(
        rx.try_recv().is_ok(),
        "IQ redelivered to addressed resource"
    );
}

#[tokio::test]
async fn iq_unacked_dropped_when_no_resource_online() {
    // IQs cannot be queued offline (XEP-0160 §1 line 63).
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let iq_xml = {
        let iq = xmpp_parsers::iq::Iq::Result {
            from: Some("server.example/srv".parse().expect("valid full jid")),
            to: Some("alice@example.com/laptop".parse().expect("valid full jid")),
            id: "iq-1".to_string(),
            payload: None,
        };
        let element: xmpp_parsers::minidom::Element = iq.into();
        let mut buf = Vec::new();
        element.write_to(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    };

    let session =
        detached_session_with_unacked("stream-1", full("alice@example.com/laptop"), vec![iq_xml]);

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.dropped, 1);
    assert_eq!(summary.queued, 0, "IQs never go to offline storage");
}

#[tokio::test]
async fn skips_unparseable_stanzas() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec!["not actually XML".to_string()],
    );
    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;
    assert_eq!(summary.unparseable, 1);
    assert_eq!(summary.queued, 0);
}

#[tokio::test]
async fn promoted_pending_row_carries_per_stanza_original_receipt_at() {
    // Issue #209 PR #361: the Q6 SM-expiry promotion must stamp
    // each `pending_delivery` row's `original_receipt_at` with
    // the per-stanza value from the source DetachedUnackedStanza,
    // NOT a wall-clock fallback at expiry time. This is what
    // makes the eventual XEP-0203 `<delay/>` on the offline
    // replay carry the real failed-delivery time per
    // XEP-0203 §4.1 + XEP-0198 §5 line 364.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let receipt_time =
        chrono::DateTime::<Utc>::from_timestamp_millis(1_700_000_000_000).expect("valid millis");
    let session = waddle_xmpp::stream_management::DetachedSession {
        stream_id: "stream-receipt-test".to_string(),
        user_id: "alice".to_string(),
        jid: full("alice@example.com/laptop"),
        inbound_count: 0,
        outbound_count: 1,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
            sequence: 1,
            stanza_xml: dm_xml("bob@elsewhere/x", "alice@example.com", "missed me"),
            original_receipt_at: receipt_time,
            purpose: Default::default(),
        }],
        max_resume_time: Some(60),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;
    assert_eq!(summary.queued, 1);

    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].original_receipt_at, receipt_time,
        "promoted row's original_receipt_at MUST be the per-stanza value, \
             not Utc::now() at expiry"
    );
}

#[tokio::test]
async fn storage_failure_records_storage_failed_not_dropped() {
    // Copilot review on PR #346: pending_delivery insert backend
    // failures must NOT be silently collapsed into Dropped, since
    // the caller would then call confirm_drained and permanently
    // lose the unacked stanza. The PromotionSummary must surface
    // a separate `storage_failed` counter so the caller can keep
    // the durable SM row for restart-time retry.
    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(AlwaysFailingPending);
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec![dm_xml(
            "bob@elsewhere/x",
            "alice@example.com",
            "transient backend down",
        )],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.storage_failed, 1, "storage failure surfaced");
    assert_eq!(summary.dropped, 0, "must not collapse into Dropped");
    assert_eq!(summary.queued, 0);
    assert!(
        summary.has_storage_failure(),
        "has_storage_failure() must be true so caller skips confirm_drained"
    );
}

#[tokio::test]
async fn promotion_prefers_existing_self_stamp_time_over_queue_receipt_time() {
    // Multi-hop Q6 chain (issue #1178 round-3 review): a message
    // received at T0 was Q6-redelivered with an accurate
    // <delay from='example.com' stamp='T0'/> and recorded into the
    // destination's unacked queue at redelivery time T1. When THAT
    // session also expires, the promoted pending_delivery row must
    // carry T0 — the self-stamp on the in-flight stanza — not the
    // queue's T1, or the eventual flush stamps the redelivery time
    // (Archived rows rehydrate from MAM, which has no self-stamp, so
    // the row time is what ends up on the wire).
    use chrono::TimeZone;

    let t0 = Utc.with_ymd_and_hms(2026, 7, 1, 10, 0, 0).unwrap();
    let mut m =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse::<jid::Jid>().unwrap()));
    m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().unwrap());
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), "second hop".to_string());
    waddle_xmpp::xep::xep0203::add_delay_stamp(&mut m, t0, "example.com");
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).unwrap();
    let xml = String::from_utf8(buf).unwrap();

    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    // detached_session_with_unacked stamps original_receipt_at with
    // Utc::now() — the (later) redelivery-time T1.
    let session = detached_session_with_unacked(
        "stream-second-hop",
        full("alice@example.com/laptop"),
        vec![xml],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;

    assert_eq!(summary.queued, 1);
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].original_receipt_at, t0,
        "promoted row must carry the self-stamp's original time, \
         not the unacked queue's redelivery-time receipt"
    );
}

#[tokio::test]
async fn restart_outlasting_resume_window_promotes_queue_into_pending_delivery() {
    // Issue #1098 acceptance: a server restart that outlasts the
    // XEP-0198 resume window must not lose the dead session's unacked
    // queue. The restore path hydrates the (already-expired) session,
    // the janitor-shaped drain → promote → confirm chain lands the
    // stanza in pending delivery storage, and only then are the
    // durable SM rows erased.
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, PersistedSession, PersistedUnackedStanza, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let now = Utc::now();
    sm_storage
        .upsert_session(PersistedSession {
            stream_id: SmSessionId::new("stream-dead"),
            user_id: "alice".to_string(),
            jid: full("alice@example.com/laptop"),
            inbound_count: 0,
            outbound_count: 1,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(60),
            detached_at: now - chrono::Duration::seconds(600),
            max_resume_duration: std::time::Duration::from_secs(60),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        })
        .await
        .unwrap();
    let mut queued =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse::<jid::Jid>().unwrap()));
    queued.from = Some("bob@elsewhere/x".parse::<jid::Jid>().unwrap());
    queued.type_ = xmpp_parsers::message::MessageType::Chat;
    queued
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "while down".to_string());
    sm_storage
        .append_unacked(PersistedUnackedStanza {
            stream_id: SmSessionId::new("stream-dead"),
            sequence: 1,
            stanza: Box::new(Stanza::Message(queued)),
            original_receipt_at: now - chrono::Duration::seconds(610),
            purpose: Default::default(),
        })
        .await
        .unwrap();

    // Restart-style bring-up: fresh registry over the same storage.
    let sm_registry = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    assert_eq!(sm_registry.restore_from_persistence().await.unwrap(), 1);

    // Janitor pass: drain expired, promote, confirm.
    let drained = sm_registry.drain_expired().await.unwrap();
    assert_eq!(drained.len(), 1);
    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let summary = promote_session_unacked(
        &drained[0],
        &registry,
        &user_registry,
        &pending,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;
    assert_eq!(summary.queued, 1);
    assert!(!summary.has_storage_failure());
    sm_registry.confirm_drained("stream-dead").await;

    assert_eq!(pending.count(&bare("alice@example.com")).await.unwrap(), 1);
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-dead"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn displaced_sessions_are_promoted_and_confirmed() {
    // Issue #1097 acceptance: sessions displaced from the SM registry
    // (max_sessions overflow eviction or fresh-bind invalidation) run
    // the full promote → confirm chain. The queued DM lands in
    // pending delivery storage and the displaced session's durable SM
    // rows are erased only afterwards.
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::with_capacity(1)
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    let mut oldest = detached_session_with_unacked(
        "stream-oldest",
        full("alice@example.com/web"),
        vec![dm_xml("bob@elsewhere/x", "alice@example.com", "displaced")],
    );
    oldest.detached_at = Instant::now() - std::time::Duration::from_secs(30);
    assert!(sm_registry.store_session(oldest).await.unwrap().is_empty());

    // Filling past capacity displaces the oldest session.
    let displaced = sm_registry
        .store_session(detached_session_with_unacked(
            "stream-newer",
            full("carol@example.com/web"),
            Vec::new(),
        ))
        .await
        .unwrap();
    assert_eq!(displaced.len(), 1);

    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    promote_displaced_sessions(
        displaced,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: blocking.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;

    // The displaced queue landed in pending delivery — no message lost.
    assert_eq!(pending.count(&bare("alice@example.com")).await.unwrap(), 1);
    // Confirmed: durable SM rows for the displaced session are gone.
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-oldest"))
        .await
        .unwrap()
        .is_none());
    // The surviving session's rows remain.
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-newer"))
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn ownership_moved_detach_promotes_displaced_queue_instead_of_dropping_it() {
    // S2 regression (ownership-moved detach path): conn A dies; the
    // same client fresh-binds on conn B BEFORE A's cleanup stores its
    // detached session, so B's invalidation pass finds nothing. A's
    // cleanup then stores, loses the `unregister_if_owner` race, and
    // previously erased the stored session durably while discarding
    // the returned queue. The displace + promote chain must instead
    // land the queue in pending delivery and only then erase the rows.
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    assert!(sm_registry
        .store_session(detached_session_with_unacked(
            "stream-owner-moved",
            full("alice@example.com/laptop"),
            vec![dm_xml("bob@elsewhere/x", "alice@example.com", "keep me")],
        ))
        .await
        .unwrap()
        .is_empty());

    // Ownership race lost → displace (memory only, rows preserved).
    let displaced = sm_registry
        .displace_stored_session_if_unclaimed("stream-owner-moved")
        .await
        .unwrap()
        .expect("stored session must be displaced");
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-owner-moved"))
        .await
        .unwrap()
        .is_some());

    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    promote_displaced_sessions(
        vec![displaced],
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: blocking.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;

    // No message lost, and confirm erased the durable rows.
    assert_eq!(pending.count(&bare("alice@example.com")).await.unwrap(), 1);
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-owner-moved"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn displaced_promotion_storage_failure_keeps_session_drainable_for_retry() {
    // S4 regression: a promotion failure preserved the durable rows
    // but the session was already gone from the in-memory map, and
    // `drain_expired` scans only memory — so the janitor's "retried on
    // the next pass" promise was false and the queue was stranded
    // until a restart. On failure the session must be re-inserted
    // (forced expired) so the next drain retries, and the retry must
    // succeed once storage recovers.
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::with_capacity(1)
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    let mut oldest = detached_session_with_unacked(
        "stream-retry",
        full("alice@example.com/web"),
        vec![dm_xml("bob@elsewhere/x", "alice@example.com", "retry me")],
    );
    oldest.detached_at = Instant::now() - std::time::Duration::from_secs(30);
    assert!(sm_registry.store_session(oldest).await.unwrap().is_empty());
    let displaced = sm_registry
        .store_session(detached_session_with_unacked(
            "stream-newer",
            full("carol@example.com/web"),
            Vec::new(),
        ))
        .await
        .unwrap();
    assert_eq!(displaced.len(), 1);

    // Promotion fails: pending-delivery backend is down.
    let failing: Arc<dyn PendingDeliveryStorage> = Arc::new(AlwaysFailingPending);
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    promote_displaced_sessions(
        displaced,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &failing,
            blocking_storage: blocking.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;

    // Durable rows preserved (existing contract)...
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-retry"))
        .await
        .unwrap()
        .is_some());
    // ...AND the session is drainable again without a restart.
    let drained = sm_registry.drain_expired().await.unwrap();
    assert_eq!(
        drained.len(),
        1,
        "failed promotion must leave the session in memory for the janitor's next drain"
    );
    assert_eq!(drained[0].stream_id, "stream-retry");

    // Storage recovers: the retry promotes and confirms.
    let recovered: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let summary = promote_session_unacked(
        &drained[0],
        &registry,
        &user_registry,
        &recovered,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;
    assert_eq!(summary.queued, 1);
    assert!(!summary.has_storage_failure());
    sm_registry.confirm_drained("stream-retry").await;
    assert_eq!(
        recovered.count(&bare("alice@example.com")).await.unwrap(),
        1
    );
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-retry"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn displaced_promotion_blocklist_failure_keeps_session_drainable_for_retry() {
    // S4 (blocklist-load branch): same retry contract as the storage-
    // failure branch — fail-closed XEP-0191 skip must not strand the
    // queue until restart.
    use async_trait::async_trait;
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};

    struct FailingBlocking;
    #[async_trait]
    impl BlockingStorage for FailingBlocking {
        async fn list_blocked_jids(
            &self,
            _user: &BareJid,
        ) -> Result<Vec<BareJid>, BlockingStorageError> {
            Err(BlockingStorageError::new(std::io::Error::other(
                "simulated blocklist backend failure",
            )))
        }
    }

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    let jid = full("alice@example.com/phone");
    assert!(sm_registry
        .store_session(detached_session_with_unacked(
            "stream-blocklist-retry",
            jid.clone(),
            vec![dm_xml("bob@elsewhere/x", "alice@example.com", "held")],
        ))
        .await
        .unwrap()
        .is_empty());
    let removed = sm_registry.invalidate_sessions_for_jid(&jid).await.unwrap();
    assert_eq!(removed.len(), 1);

    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let blocking = FailingBlocking;
    promote_displaced_sessions(
        removed,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: &blocking,
            server_domain: "example.com",
        },
    )
    .await;

    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-blocklist-retry"))
        .await
        .unwrap()
        .is_some());
    let drained = sm_registry.drain_expired().await.unwrap();
    assert_eq!(
        drained.len(),
        1,
        "blocklist-load failure must leave the session drainable for retry"
    );
    assert_eq!(drained[0].stream_id, "stream-blocklist-retry");
    assert_eq!(drained[0].unacked_stanzas.len(), 1);
}

#[tokio::test]
async fn cancelled_displaced_promotion_reinserts_current_and_unstarted_sessions() {
    use async_trait::async_trait;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};

    struct HangingBlocking {
        reached: tokio::sync::Notify,
    }

    #[async_trait]
    impl BlockingStorage for HangingBlocking {
        async fn list_blocked_jids(
            &self,
            _user: &BareJid,
        ) -> Result<Vec<BareJid>, BlockingStorageError> {
            self.reached.notify_one();
            std::future::pending().await
        }
    }

    let storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = Arc::new(
        InMemorySmSessionRegistry::with_capacity(3)
            .with_persistence(Arc::clone(&storage) as Arc<dyn SmPersistenceStorage>),
    );
    let mut first = detached_session_with_unacked(
        "cancelled-promotion-first",
        full("alice@example.com/web"),
        vec![dm_xml("bob@elsewhere/x", "alice@example.com", "held")],
    );
    first.max_resume_time = Some(0);
    let mut second = detached_session_with_unacked(
        "cancelled-promotion-second",
        full("carol@example.com/web"),
        vec![dm_xml("bob@elsewhere/x", "carol@example.com", "held too")],
    );
    second.max_resume_time = Some(0);
    sm_registry.store_session(first).await.unwrap();
    sm_registry.store_session(second).await.unwrap();
    let displaced = sm_registry.drain_expired().await.unwrap();
    assert_eq!(displaced.len(), 2);

    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let connections = Arc::new(ConnectionRegistry::new());
    let user_registry = test_user_registry();
    let blocking = Arc::new(HangingBlocking {
        reached: tokio::sync::Notify::new(),
    });
    let task_registry = sm_registry.clone();
    let task_pending = pending.clone();
    let task_connections = connections.clone();
    let task_user_registry = user_registry.clone();
    let task_blocking = blocking.clone();
    let promotion = tokio::spawn(async move {
        promote_displaced_sessions(
            displaced,
            DisplacedPromotionDeps {
                sm_registry: &task_registry,
                connection_registry: &task_connections,
                user_registry: &task_user_registry,
                pending_storage: &task_pending,
                blocking_storage: task_blocking.as_ref(),
                server_domain: "example.com",
            },
        )
        .await;
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        blocking.reached.notified(),
    )
    .await
    .expect("promotion should reach the cancellable blocklist read");
    promotion.abort();
    assert!(promotion
        .await
        .expect_err("promotion should be cancelled")
        .is_cancelled());

    let retried = sm_registry.drain_expired().await.expect("drain retry");
    let retried_ids = retried
        .iter()
        .map(|session| session.stream_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        retried_ids,
        std::collections::HashSet::from([
            "cancelled-promotion-first",
            "cancelled-promotion-second",
        ])
    );
    for stream_id in ["cancelled-promotion-first", "cancelled-promotion-second"] {
        assert!(sm_registry
            .locally_owned_claim_ids()
            .expect("local ownership")
            .iter()
            .any(|owned| owned == stream_id));
        assert!(sm_registry.confirm_drained(stream_id).await);
        assert!(storage
            .get_session(&waddle_xmpp::pending_delivery::SmSessionId::new(stream_id))
            .await
            .expect("durable lookup")
            .is_none());
    }
}

#[tokio::test]
async fn fresh_bind_invalidation_delivers_displaced_queue_to_new_session() {
    // Issue #1097 acceptance (fresh-bind path): the resource just
    // re-bound (registered in the ConnectionRegistry), so promoting
    // the invalidated old detached session live-delivers its queue to
    // the new session via the promotion chain's alt-resource step —
    // then confirms, erasing the stale durable rows.
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    let jid = full("alice@example.com/phone");
    assert!(sm_registry
        .store_session(detached_session_with_unacked(
            "stream-stale",
            jid.clone(),
            vec![dm_xml(
                "bob@elsewhere/x",
                "alice@example.com",
                "for the new bind"
            )],
        ))
        .await
        .unwrap()
        .is_empty());

    // The fresh bind registers its connection BEFORE invalidation runs
    // (finalize_sm_after_registry_registration ordering).
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    dual_register(&registry, &user_registry, jid.clone(), tx).await;
    registry.update_presence(&jid, true, 0);

    let removed = sm_registry.invalidate_sessions_for_jid(&jid).await.unwrap();
    assert_eq!(removed.len(), 1);

    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    promote_displaced_sessions(
        removed,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: blocking.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;

    assert!(
        rx.try_recv().is_ok(),
        "displaced queue must be live-delivered to the freshly bound session"
    );
    assert_eq!(pending.count(&bare("alice@example.com")).await.unwrap(), 0);
    assert!(sm_storage
        .get_session(&SmSessionId::new("stream-stale"))
        .await
        .unwrap()
        .is_none());
}

fn dm_xml_with_id(from: &str, to: &str, id: &str, body: &str) -> String {
    let mut m = xmpp_parsers::message::Message::new(Some(to.parse::<jid::Jid>().unwrap()));
    m.from = Some(from.parse::<jid::Jid>().unwrap());
    m.id = Some(xmpp_parsers::message::Id(id.to_string()));
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn transient_bodies(rows: &[waddle_xmpp::pending_delivery::PendingRow]) -> Vec<String> {
    let mut bodies: Vec<String> = rows
        .iter()
        .filter_map(|row| match &row.payload {
            waddle_xmpp::pending_delivery::PendingPayload::Transient(message) => {
                message.bodies.values().next().cloned()
            }
            waddle_xmpp::pending_delivery::PendingPayload::Archived(_) => None,
        })
        .collect();
    bodies.sort();
    bodies
}

#[tokio::test]
async fn promotion_scrubs_stanzas_matching_recent_tombstone() {
    // R2 (round-2 review): the promotion chain must re-check the
    // registry's recent-tombstone record before writing a drained
    // session's unacked stanzas into pending_delivery, so a retraction
    // racing an in-flight promotion cannot resurrect retracted content.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let session = detached_session_with_unacked(
        "stream-tomb",
        full("alice@example.com/laptop"),
        vec![
            dm_xml_with_id(
                "bob@elsewhere/x",
                "alice@example.com",
                "retract-me",
                "secret",
            ),
            dm_xml_with_id("bob@elsewhere/x", "alice@example.com", "keep-me", "safe"),
        ],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        // Recorded AFTER the session's stanzas were received, so the
        // backward-in-time scope applies.
        &[waddle_xmpp::stream_management::RecentTombstoneRecord {
            key: direct_target("retract-me", "bob@elsewhere", "alice@example.com"),
            recorded_at_utc: Utc::now(),
        }],
    )
    .await;

    assert_eq!(summary.scrubbed, 1, "retracted stanza counted as scrubbed");
    assert_eq!(summary.queued, 1, "the non-matching stanza still promotes");
    assert_eq!(
        summary.dropped, 0,
        "scrubbed must not be counted as dropped"
    );
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(
        transient_bodies(&rows),
        vec!["safe".to_string()],
        "retracted content must not reach pending_delivery"
    );
}

#[tokio::test]
async fn tombstone_does_not_scrub_stanza_received_after_its_recording() {
    // Round-3 review finding 2: a tombstone applies backward in time
    // only. A NEW message that legitimately reuses the same wire id
    // (counter-style client ids) in the same conversation scope,
    // received AFTER the retraction was recorded, must promote
    // normally instead of being silently lost.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    // The helper stamps original_receipt_at = Utc::now(); the tombstone
    // predates it by an hour.
    let session = detached_session_with_unacked(
        "stream-reuse",
        full("alice@example.com/laptop"),
        vec![dm_xml_with_id(
            "bob@elsewhere/x",
            "alice@example.com",
            "retract-me",
            "fresh reuse",
        )],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[waddle_xmpp::stream_management::RecentTombstoneRecord {
            key: direct_target("retract-me", "bob@elsewhere", "alice@example.com"),
            recorded_at_utc: Utc::now() - chrono::Duration::hours(1),
        }],
    )
    .await;

    assert_eq!(
        summary.scrubbed, 0,
        "a stanza received after the tombstone's recording must not be scrubbed"
    );
    assert_eq!(summary.queued, 1, "the reused-id stanza promotes normally");
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(
        transient_bodies(&rows),
        vec!["fresh reuse".to_string()],
        "the legitimate new message must reach pending_delivery"
    );
}

#[tokio::test]
async fn tombstone_scrubs_stanza_whose_receipt_reads_slightly_after_recording() {
    // Round-4 review: recorded_at_utc and original_receipt_at can come
    // from different clocks (persistence restore across a restart,
    // multi-node stamps, NTP step-back). A retracted stanza whose
    // receipt stamp reads seconds "after" the tombstone recording due
    // to skew must still be scrubbed — the backward-only scope carries
    // a skew slack.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    // Helper stamps original_receipt_at = Utc::now(); the tombstone
    // reads 30s in the past — inside the slack, so this models a
    // receipt stamp skewed up to 30s ahead of the scrubbing clock.
    let session = detached_session_with_unacked(
        "stream-skew",
        full("alice@example.com/laptop"),
        vec![dm_xml_with_id(
            "bob@elsewhere/x",
            "alice@example.com",
            "retract-me",
            "secret",
        )],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &user_registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
        &[waddle_xmpp::stream_management::RecentTombstoneRecord {
            key: direct_target("retract-me", "bob@elsewhere", "alice@example.com"),
            recorded_at_utc: Utc::now() - chrono::Duration::seconds(30),
        }],
    )
    .await;

    assert_eq!(
        summary.scrubbed, 1,
        "a receipt stamp within the skew slack must still be scrubbed"
    );
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert!(
        transient_bodies(&rows).is_empty(),
        "retracted content must not reach pending_delivery under clock skew"
    );
}

#[tokio::test]
async fn retraction_racing_in_flight_promotion_does_not_deliver_retracted_content() {
    // R2 end-to-end race: the janitor's drain moves the session off
    // both maps into a local; the retraction scrub then finds it
    // nowhere (and its pending row is not yet inserted); the promotion
    // finally runs. The recent-tombstone re-check must drop the
    // retracted stanza while the rest of the queue promotes, and
    // confirm_drained may then erase the SM rows safely.
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    sm_registry
        .store_session(detached_session_with_unacked(
            "stream-race",
            full("alice@example.com/laptop"),
            vec![
                dm_xml_with_id(
                    "bob@elsewhere/x",
                    "alice@example.com",
                    "retract-me",
                    "secret",
                ),
                dm_xml_with_id("bob@elsewhere/x", "alice@example.com", "keep-me", "safe"),
            ],
        ))
        .await
        .unwrap();

    // Step 1: drain (session now off both maps, held in a local).
    let drained = sm_registry.drain_all_for_shutdown().await.unwrap();
    assert_eq!(drained.len(), 1);

    // Step 2: retraction scrub lands mid-promotion.
    sm_registry
        .scrub_unacked_for_tombstone(&direct_target(
            "retract-me",
            "bob@elsewhere",
            "alice@example.com",
        ))
        .await
        .unwrap();

    // Step 3: promotion runs with the registry's recent tombstones.
    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let recent = sm_registry.recent_tombstones().unwrap();
    let summary = promote_session_unacked(
        &drained[0],
        &registry,
        &user_registry,
        &pending,
        &Blocklist::empty(),
        "example.com",
        &recent,
    )
    .await;
    assert_eq!(summary.scrubbed, 1);
    assert_eq!(summary.queued, 1);
    assert!(!summary.has_storage_failure());
    sm_registry.confirm_drained("stream-race").await;

    let rows = pending.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(
        transient_bodies(&rows),
        vec!["safe".to_string()],
        "the retracted stanza must be absent from pending storage; the \
         other stanza must still be promoted"
    );
}

/// BlockingStorage stub whose Nth `list_blocked_jids` call records a
/// tombstone on the shared SM registry — simulates a XEP-0424/0425
/// retraction landing MID-BATCH, after earlier sessions in the same
/// drained batch were already promoted.
struct MidBatchRetractingBlocking {
    sm_registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    calls: std::sync::atomic::AtomicU32,
    retract_on_call: u32,
    target: waddle_xmpp::tombstone::TombstoneTarget,
}

#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for MidBatchRetractingBlocking {
    async fn list_blocked_jids(
        &self,
        _user: &BareJid,
    ) -> Result<Vec<BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        use waddle_xmpp::stream_management::SmSessionRegistry;

        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call == self.retract_on_call {
            self.sm_registry
                .scrub_unacked_for_tombstone(&self.target)
                .await
                .expect("mid-batch scrub must succeed");
        }
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn mid_batch_retraction_still_scrubs_later_sessions_in_displaced_promotion() {
    // Round-3 review finding 1: the recent-tombstone list must be
    // fetched PER SESSION inside the batch loop, not once per batch.
    // A retraction landing after the first session of a drained batch
    // was promoted (sessions are off-map, so the scrub phases cannot
    // see them) must still scrub the second session's matching stanza.
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

    let sm_registry = Arc::new(InMemorySmSessionRegistry::new());
    let sessions = vec![
        detached_session_with_unacked(
            "stream-first",
            full("alice@example.com/laptop"),
            vec![dm_xml("bob@elsewhere/x", "alice@example.com", "hello")],
        ),
        detached_session_with_unacked(
            "stream-second",
            full("carol@example.com/laptop"),
            vec![
                dm_xml_with_id(
                    "bob@elsewhere/x",
                    "carol@example.com",
                    "retract-me",
                    "secret",
                ),
                dm_xml_with_id("bob@elsewhere/x", "carol@example.com", "keep-me", "safe"),
            ],
        ),
    ];
    // The retraction lands during the SECOND session's blocklist load —
    // strictly after the first session's promotion completed.
    let blocking = MidBatchRetractingBlocking {
        sm_registry: Arc::clone(&sm_registry),
        calls: std::sync::atomic::AtomicU32::new(0),
        retract_on_call: 2,
        target: direct_target("retract-me", "bob@elsewhere", "carol@example.com"),
    };
    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();

    promote_displaced_sessions(
        sessions,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: &blocking,
            server_domain: "example.com",
        },
    )
    .await;

    assert_eq!(
        transient_bodies(&pending.list(&bare("alice@example.com")).await.unwrap()),
        vec!["hello".to_string()],
        "the first session (promoted before the retraction) is unaffected"
    );
    assert_eq!(
        transient_bodies(&pending.list(&bare("carol@example.com")).await.unwrap()),
        vec!["safe".to_string()],
        "a retraction landing mid-batch must still scrub the later \
         session's matching stanza"
    );
}

/// PendingDeliveryStorage wrapper that fires a XEP-0424 retraction on
/// the shared SM registry immediately BEFORE its first `insert`
/// commits — models the finding-B TOCTOU: the retraction lands after
/// the per-session recent-tombstones snapshot was taken (the session
/// is off both registry maps, so scrub phases 1-4 see nothing, and
/// the pending row is not inserted yet, so the retraction's own
/// pending scrub removes nothing either), and the promotion then
/// inserts the retracted stanza.
struct RetractDuringInsertPending {
    inner: InMemoryPendingDeliveryStorage,
    sm_registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    target: waddle_xmpp::tombstone::TombstoneTarget,
    fired: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl PendingDeliveryStorage for RetractDuringInsertPending {
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        if !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            use waddle_xmpp::stream_management::SmSessionRegistry;
            // The retraction's registry scrub: records the recent
            // tombstone; the drained session is off both maps so no
            // in-memory phase matches.
            self.sm_registry
                .scrub_unacked_for_tombstone(&self.target)
                .await
                .expect("racing scrub must succeed");
            // The retraction's own pending-delivery scrub: runs before
            // this insert commits, so it removes nothing.
            self.inner
                .scrub_for_tombstone(&self.target)
                .await
                .expect("racing pending scrub must succeed");
        }
        self.inner.insert(row).await
    }
    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }
    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }
    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }
    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }
    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }
    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }
    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }
    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }
    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }
    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }
    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }
    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }
    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

#[tokio::test]
async fn retraction_landing_after_tombstone_snapshot_still_scrubs_promoted_rows() {
    // FINDING B (retraction-vs-promotion TOCTOU): the recent-tombstone
    // snapshot is fetched per session BEFORE promote_session_unacked.
    // A retraction landing between that fetch and the pending insert
    // is invisible everywhere: the session is off both registry maps
    // (scrub phases find nothing) and its pending row isn't inserted
    // yet (the retraction's pending scrub removes nothing). Without a
    // post-promotion re-check the retracted stanza reaches
    // pending_delivery and delivers at the next login.
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    let sm_registry = Arc::new(InMemorySmSessionRegistry::new());
    let sessions = vec![detached_session_with_unacked(
        "stream-toctou",
        full("alice@example.com/laptop"),
        vec![
            dm_xml_with_id(
                "bob@elsewhere/x",
                "alice@example.com",
                "retract-me",
                "secret",
            ),
            dm_xml_with_id("bob@elsewhere/x", "alice@example.com", "keep-me", "safe"),
        ],
    )];
    let pending_impl = Arc::new(RetractDuringInsertPending {
        inner: InMemoryPendingDeliveryStorage::unlimited(),
        sm_registry: Arc::clone(&sm_registry),
        target: direct_target("retract-me", "bob@elsewhere", "alice@example.com"),
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::clone(&pending_impl) as _;
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());

    promote_displaced_sessions(
        sessions,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: blocking.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;

    let rows = pending.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(
        transient_bodies(&rows),
        vec!["safe".to_string()],
        "a retraction recorded after the pre-promotion tombstone snapshot \
         must still scrub the promoted pending row before the drain is \
         confirmed"
    );
}

/// PendingDeliveryStorage that fails exactly one insert (the Nth call)
/// while armed, delegating everything to a real in-memory store —
/// simulates a transient partial storage failure mid-promotion.
struct FlakyPending {
    inner: InMemoryPendingDeliveryStorage,
    armed: std::sync::atomic::AtomicBool,
    insert_calls: std::sync::atomic::AtomicU32,
    fail_on_call: u32,
}

impl FlakyPending {
    fn failing_on(call: u32) -> Self {
        Self {
            inner: InMemoryPendingDeliveryStorage::unlimited(),
            armed: std::sync::atomic::AtomicBool::new(true),
            insert_calls: std::sync::atomic::AtomicU32::new(0),
            fail_on_call: call,
        }
    }

    fn disarm(&self) {
        self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl PendingDeliveryStorage for FlakyPending {
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        let call = self
            .insert_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) && call == self.fail_on_call {
            return Err(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "simulated transient backend failure".into(),
                ),
            );
        }
        self.inner.insert(row).await
    }
    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }
    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }
    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }
    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }
    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }
    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }
    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }
    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }
    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }
    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }
    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }
    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }
    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

#[tokio::test]
async fn partial_storage_failure_retries_only_failed_stanzas_without_duplicates() {
    // R4 (round-2 review): a partial storage failure used to re-promote
    // the WHOLE queue on every janitor tick — already-Queued stanzas
    // included — because promotion kept no per-stanza progress. After a
    // partial failure the successfully promoted stanzas' durable
    // sm_unacked rows must be deleted and dropped from the reinserted
    // session, so retries cover only the failed stanzas and duplication
    // is bounded to a crash window, not per tick.
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    let jid = full("alice@example.com/web");
    assert!(sm_registry
        .store_session(detached_session_with_unacked(
            "stream-partial",
            jid.clone(),
            vec![
                dm_xml("bob@elsewhere/x", "alice@example.com", "msg-1"),
                dm_xml("bob@elsewhere/x", "alice@example.com", "msg-2"),
                dm_xml("bob@elsewhere/x", "alice@example.com", "msg-3"),
            ],
        ))
        .await
        .unwrap()
        .is_empty());
    let displaced = sm_registry.invalidate_sessions_for_jid(&jid).await.unwrap();
    assert_eq!(displaced.len(), 1);

    // Tick 1: stanza 2 of 3 fails to insert.
    let flaky = Arc::new(FlakyPending::failing_on(2));
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::clone(&flaky) as _;
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    promote_displaced_sessions(
        displaced,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: blocking.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;

    // Stanzas 1 & 3 landed in pending storage...
    let rows = pending.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(
        transient_bodies(&rows),
        vec!["msg-1".to_string(), "msg-3".to_string()]
    );
    // ...and their durable sm_unacked rows are gone; only the failed
    // stanza's row survives for the retry.
    let stream_id = SmSessionId::new("stream-partial");
    let surviving: Vec<u32> = sm_storage
        .list_unacked(&stream_id)
        .await
        .unwrap()
        .iter()
        .map(|row| row.sequence)
        .collect();
    assert_eq!(
        surviving,
        vec![2],
        "successfully promoted stanzas' durable rows must be deleted after tick 1"
    );

    // Tick 2: janitor drains the reinserted session — it must retry
    // ONLY the failed stanza.
    let drained = sm_registry.drain_expired().await.unwrap();
    assert_eq!(drained.len(), 1);
    let retry_sequences: Vec<u32> = drained[0]
        .unacked_stanzas
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(
        retry_sequences,
        vec![2],
        "the reinserted session must retain only the failed stanza"
    );

    // Storage recovers: retry queues stanza 2 exactly once.
    flaky.disarm();
    promote_displaced_sessions(
        drained,
        DisplacedPromotionDeps {
            sm_registry: &sm_registry,
            connection_registry: &registry,
            user_registry: &user_registry,
            pending_storage: &pending,
            blocking_storage: blocking.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;

    let rows = pending.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(
        transient_bodies(&rows),
        vec![
            "msg-1".to_string(),
            "msg-2".to_string(),
            "msg-3".to_string()
        ],
        "after recovery every stanza must be queued exactly once — no duplicates"
    );
    assert!(
        sm_storage.get_session(&stream_id).await.unwrap().is_none(),
        "full success confirms the drain, erasing the durable session row"
    );
}

#[tokio::test]
async fn capacity_churn_loses_no_messages_across_restart_style_read() {
    // Issue #1097 acceptance (churn): fill the registry to capacity,
    // keep storing so the oldest sessions are evicted, promote every
    // displaced session, then do a restart-style read: every evicted
    // user's message must be in pending delivery storage and no
    // stale durable SM rows may remain for confirmed streams.
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    let sm_storage = Arc::new(InMemorySmPersistence::new());
    let sm_registry = InMemorySmSessionRegistry::with_capacity(2)
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let user_registry = test_user_registry();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());

    for i in 0..5u32 {
        let user = format!("user{i}@example.com");
        let mut session = detached_session_with_unacked(
            &format!("stream-{i}"),
            format!("{user}/web").parse().unwrap(),
            vec![dm_xml("bob@elsewhere/x", &user, &format!("msg-{i}"))],
        );
        // Strictly increasing ages so eviction order is deterministic.
        session.detached_at = Instant::now() - std::time::Duration::from_secs(60 - u64::from(i));
        let displaced = sm_registry.store_session(session).await.unwrap();
        promote_displaced_sessions(
            displaced,
            DisplacedPromotionDeps {
                sm_registry: &sm_registry,
                connection_registry: &registry,
                user_registry: &user_registry,
                pending_storage: &pending,
                blocking_storage: blocking.as_ref(),
                server_domain: "example.com",
            },
        )
        .await;
    }

    // 5 stored into capacity 2 → 3 evictions, all promoted offline.
    let mut evicted_recipients = 0u32;
    let mut retained_streams = 0u32;
    for i in 0..5u32 {
        let user: BareJid = format!("user{i}@example.com").parse().unwrap();
        let queued = pending.count(&user).await.unwrap();
        let still_detached = sm_registry
            .peek_session(&format!("stream-{i}"))
            .await
            .unwrap()
            .is_some();
        assert!(
            (queued == 1) ^ still_detached,
            "user{i}: message must be either queued for delivery or still \
             replayable from its detached session — never lost, never both"
        );
        evicted_recipients += queued;
        retained_streams += u32::from(still_detached);
    }
    assert_eq!(evicted_recipients, 3);
    assert_eq!(retained_streams, 2);

    // Restart-style read: a fresh registry over the same durable
    // storage hydrates exactly the retained (unconfirmed) sessions.
    let restarted = InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_storage) as Arc<dyn SmPersistenceStorage>);
    assert_eq!(restarted.restore_from_persistence().await.unwrap(), 2);
}
