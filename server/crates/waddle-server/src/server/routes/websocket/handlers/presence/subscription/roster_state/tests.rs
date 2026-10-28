use super::send_roster_push_to_resources;
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_extension_manager, empty_extension_manager, TestStateOverrides,
};
use jid::FullJid;
use std::io;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::stream_management::persistence::{InMemorySmPersistence, SmPersistenceStorage};
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, PromotionCaptureCustody, SmSessionRegistry,
};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::roster::{RosterItem, RosterVersion};
use waddle_xmpp_core::OccupancySessionGeneration;

#[derive(Clone, Default)]
struct CaptureWriter {
    logs: Arc<Mutex<Vec<u8>>>,
    append_rejected: Arc<Notify>,
}

impl io::Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut logs = self.logs.lock().expect("capture lock");
        logs.extend_from_slice(bytes);
        if String::from_utf8_lossy(&logs).contains("Failed to record detached roster push") {
            self.append_rejected.notify_one();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn detached_session(stream_id: &SmSessionId, jid: FullJid) -> DetachedSession {
    DetachedSession {
        promotion_custody: PromotionCaptureCustody::never_claimed(),
        stream_id: stream_id.as_str().into(),
        user_id: jid.to_bare().to_string(),
        jid,
        occupancy_session: OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: Instant::now(),
        carbons_enabled: false,
        roster_interested: true,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A real append failure followed by either a new local interested connection
/// or no connection. The stream operation guard fixes the interleaving without
/// sleeps: the first fanout poll captures targets, then waits for this guard.
async fn failed_append_warning(
    local_connection: bool,
    full_queue: bool,
) -> (serde_json::Value, Option<OutboundStanza>) {
    let writer = CaptureWriter::default();
    let logs = writer.logs.clone();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::WARN)
            .with_writer(writer)
            .finish(),
    );
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let jid: FullJid = "alice@example.com/web".parse().expect("full JID");
    let stream_id = SmSessionId::new("failed-roster-append");
    registry
        .store_session(detached_session(&stream_id, jid.clone()))
        .await
        .expect("store detached session");
    // A stale local target cannot append to a canonical source that vanished.
    storage
        .delete_session(&stream_id)
        .await
        .expect("delete source");
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            sm_session_registry: Some(registry.clone()),
            ..TestStateOverrides::default()
        },
    )
    .await;
    let operation = registry
        .lock_session_operation(stream_id.as_str())
        .await
        .expect("lock stream");
    let user = jid.to_bare();
    let item = RosterItem::new("bob@example.com".parse().expect("contact JID"));
    let version: RosterVersion = "version-after-subscription".parse().expect("version");
    let fanout = send_roster_push_to_resources(&state, &user, &item, &version);
    tokio::pin!(fanout);
    assert!(matches!(futures::poll!(fanout.as_mut()), Poll::Pending));
    let (sender, mut receiver) = mpsc::channel(1);
    if local_connection {
        if full_queue {
            sender
                .try_send(OutboundStanza::new(Stanza::Presence(
                    xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None),
                )))
                .expect("fill fallback queue");
        }
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), sender);
        state
            .deps
            .protocol
            .connection_registry
            .mark_roster_interested(&jid);
    }
    drop(operation);
    fanout.await;
    let logs = String::from_utf8(logs.lock().expect("capture lock").clone()).expect("UTF-8 logs");
    let events: Vec<serde_json::Value> = logs
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON log"))
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["fields"]["message"] == "Failed to record detached roster push")
            .count(),
        1,
        "preserve the original warning count"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event["fields"]["message"] == "Detached roster push local fallback outcome"
            })
            .count(),
        1,
        "record exactly one distinct final outcome"
    );
    let initial_warning = events
        .iter()
        .find(|event| event["fields"]["message"] == "Failed to record detached roster push")
        .unwrap_or_else(|| panic!("missing append warning in {logs}"));
    assert_eq!(
        initial_warning["fields"]["disposition"],
        "pending_local_fallback"
    );
    assert!(initial_warning["fields"]["error"]
        .as_str()
        .expect("error")
        .contains("detached append source is missing"));
    let warning = events
        .iter()
        .find(|event| event["fields"]["message"] == "Detached roster push local fallback outcome")
        .unwrap_or_else(|| panic!("missing fallback outcome in {logs}"));
    assert_eq!(
        initial_warning["fields"]["push_id"],
        warning["fields"]["push_id"]
    );
    assert_eq!(
        initial_warning["fields"]["roster_version"],
        warning["fields"]["roster_version"]
    );
    assert_eq!(
        warning["fields"]["roster_version"],
        "version-after-subscription"
    );
    assert_eq!(warning["fields"]["resource"], "alice@example.com/web");
    assert!(storage
        .get_session(&stream_id)
        .await
        .expect("load source")
        .is_none());
    assert!(storage
        .list_unacked(&stream_id)
        .await
        .expect("load replay")
        .is_empty());
    (warning.clone(), receiver.try_recv().ok())
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_detached_roster_append_reports_local_queue_acceptance() {
    let (warning, queued) = failed_append_warning(true, false).await;
    assert_eq!(warning["fields"]["disposition"], "local_queue_accepted");
    let Stanza::Iq(push) = queued.expect("fallback queued the roster push").stanza else {
        panic!("expected roster IQ");
    };
    assert_eq!(warning["fields"]["push_id"], push.id());
    assert_eq!(
        push.to().expect("push target").to_string(),
        "alice@example.com/web"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_detached_roster_append_reports_unresolved_without_local_fallback() {
    let (warning, queued) = failed_append_warning(false, false).await;
    assert_eq!(warning["fields"]["disposition"], "unresolved_locally");
    assert!(queued.is_none());
    assert!(warning["fields"]["push_id"]
        .as_str()
        .expect("push ID")
        .starts_with("roster-push-"));
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_detached_roster_append_reports_unresolved_when_local_queue_is_full() {
    let (warning, queued) = failed_append_warning(true, true).await;
    assert_eq!(warning["fields"]["disposition"], "unresolved_locally");
    assert!(matches!(
        queued.expect("existing queue entry").stanza,
        Stanza::Presence(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_detached_roster_append_warning_survives_later_resource_cancellation() {
    let writer = CaptureWriter::default();
    let logs = writer.logs.clone();
    let append_rejected = writer.append_rejected.clone();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::WARN)
            .with_writer(writer)
            .finish(),
    );
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    // Subscription fanout sorts resources: A must reject before B blocks.
    let rejected_jid: FullJid = "alice@example.com/a".parse().expect("rejected JID");
    let blocked_jid: FullJid = "alice@example.com/b".parse().expect("blocked JID");
    let rejected_stream = SmSessionId::new("rejected-roster-stream");
    let blocked_stream = SmSessionId::new("blocked-roster-stream");
    registry
        .store_session(detached_session(&rejected_stream, rejected_jid.clone()))
        .await
        .expect("store rejected session");
    registry
        .store_session(detached_session(&blocked_stream, blocked_jid))
        .await
        .expect("store blocked session");
    storage
        .delete_session(&rejected_stream)
        .await
        .expect("delete rejected source");
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            sm_session_registry: Some(registry.clone()),
            ..TestStateOverrides::default()
        },
    )
    .await;
    let blocked_operation = registry
        .lock_session_operation(blocked_stream.as_str())
        .await
        .expect("hold later resource operation");
    let user = rejected_jid.to_bare();
    let item = RosterItem::new("bob@example.com".parse().expect("contact JID"));
    let version: RosterVersion = "version-before-cancellation".parse().expect("version");
    let mut fanout = Box::pin(send_roster_push_to_resources(
        &state, &user, &item, &version,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            () = append_rejected.notified() => {}
            () = &mut fanout => panic!("fanout must remain blocked on the later resource"),
        }
    })
    .await
    .expect("append rejection must be logged before the later resource unblocks");
    assert!(matches!(futures::poll!(fanout.as_mut()), Poll::Pending));
    // Mirror handler timeout cancellation while B's stream operation is held.
    drop(fanout);
    let logs = String::from_utf8(logs.lock().expect("capture lock").clone()).expect("UTF-8 logs");
    let events: Vec<serde_json::Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON log"))
        .collect();
    let warning = events
        .iter()
        .find(|event| event["fields"]["message"] == "Failed to record detached roster push")
        .unwrap_or_else(|| panic!("missing immediate rejection warning in {logs}"));
    assert_eq!(warning["fields"]["resource"], "alice@example.com/a");
    assert_eq!(
        warning["fields"]["roster_version"],
        "version-before-cancellation"
    );
    assert_eq!(warning["fields"]["disposition"], "pending_local_fallback");
    assert!(warning["fields"]["push_id"]
        .as_str()
        .expect("push ID")
        .starts_with("roster-push-"));
    assert!(!events.iter().any(|event| {
        event["fields"]["message"] == "Detached roster push local fallback outcome"
            && event["fields"]["resource"] == "alice@example.com/a"
    }));
    assert!(storage
        .list_unacked(&blocked_stream)
        .await
        .expect("blocked replay")
        .is_empty());
    drop(blocked_operation);
}
