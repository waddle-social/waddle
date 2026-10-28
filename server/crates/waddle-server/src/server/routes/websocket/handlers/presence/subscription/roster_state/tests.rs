use super::send_roster_push_to_resources;
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_extension_manager, empty_extension_manager, TestStateOverrides,
};
use jid::FullJid;
use std::io;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Instant;
use tokio::sync::mpsc;
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
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("capture lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
    let logs = writer.0.clone();
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
        .store_session(DetachedSession {
            promotion_custody: PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.as_str().into(),
            user_id: "alice@example.com".into(),
            jid: jid.clone(),
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
        })
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
    let warning = logs
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON log"))
        .find(|event| event["fields"]["message"] == "Failed to record detached roster push")
        .unwrap_or_else(|| panic!("missing append warning in {logs}"));
    assert!(warning["fields"]["error"]
        .as_str()
        .expect("error")
        .contains("detached append source is missing"));
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
    (warning, receiver.try_recv().ok())
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
