use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use jid::{BareJid, FullJid};
use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::QuotaPolicy;
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::stream_management::DetachedSession;
use waddle_xmpp::Stanza;

use super::*;

fn full(s: &str) -> FullJid {
    s.parse().unwrap()
}

fn bare(s: &str) -> BareJid {
    s.parse().unwrap()
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
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/web"),
        vec![mam_replay_xml(child_name)],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec![dm_xml("bob@elsewhere/x", "alice@example.com", "missed me?")],
    );

    let summary = promote_session_unacked(
        &session,
        &registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
    )
    .await;

    assert_eq!(summary.queued, 1);
    assert_eq!(summary.redelivered, 0);
    assert_eq!(summary.bounced, 0);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
}

#[tokio::test]
async fn dm_with_mam_payload_is_still_promoted_to_pending_delivery() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    let alt = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(alt.clone(), tx);
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    // Register the sender so the bounce can be delivered.
    let sender = full("bob@example.com/x");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(sender.clone(), tx);
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    // Alice's web resource is online; laptop is detached.
    let alt = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(alt.clone(), tx);
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    let web = full("alice@example.com/web");
    let mobile = full("alice@example.com/mobile");
    let (tx_web, mut rx_web) = tokio::sync::mpsc::channel(8);
    let (tx_mobile, mut rx_mobile) = tokio::sync::mpsc::channel(8);
    registry.register(web.clone(), tx_web);
    registry.register(mobile.clone(), tx_mobile);
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    let session = detached_session_with_unacked(
        "stream-1",
        full("alice@example.com/laptop"),
        vec!["not actually XML".to_string()],
    );
    let summary = promote_session_unacked(
        &session,
        &registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    };

    let summary = promote_session_unacked(
        &session,
        &registry,
        &storage,
        &Blocklist::empty(),
        "example.com",
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
    use async_trait::async_trait;
    use waddle_xmpp::pending_delivery::storage::PendingStorageError;
    use waddle_xmpp::pending_delivery::{InsertOutcome, PendingRow, PendingRowId, SmSessionId};

    struct AlwaysFailingPending;
    #[async_trait]
    impl PendingDeliveryStorage for AlwaysFailingPending {
        async fn insert(&self, _row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
            Err(PendingStorageError::Other(
                "simulated backend failure".into(),
            ))
        }
        async fn list(&self, _recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError> {
            Ok(vec![])
        }
        async fn claim_for_session(
            &self,
            _recipient: &BareJid,
            _session: &SmSessionId,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            Ok(vec![])
        }
        async fn delete_claimed(&self, _session: &SmSessionId) -> Result<u64, PendingStorageError> {
            Ok(0)
        }
        async fn delete_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
            Ok(0)
        }
        async fn release_claim(&self, _session: &SmSessionId) -> Result<u64, PendingStorageError> {
            Ok(0)
        }
        async fn release_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
            Ok(0)
        }
        async fn record_pushed_at(
            &self,
            _id: &PendingRowId,
            _sequence: u32,
        ) -> Result<u64, PendingStorageError> {
            Ok(0)
        }
        async fn delete_acked_through(
            &self,
            _session: &SmSessionId,
            _sequence_max: u32,
        ) -> Result<u64, PendingStorageError> {
            Ok(0)
        }
        async fn list_orphaned_claims(
            &self,
            _live_sessions: &[SmSessionId],
        ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError> {
            Ok(vec![])
        }
        async fn count(&self, _recipient: &BareJid) -> Result<u32, PendingStorageError> {
            Ok(0)
        }
        async fn delete_older_than(
            &self,
            _cutoff: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, PendingStorageError> {
            Ok(0)
        }
    }

    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(AlwaysFailingPending);
    let registry = ConnectionRegistry::new();
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
        &storage,
        &Blocklist::empty(),
        "example.com",
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
