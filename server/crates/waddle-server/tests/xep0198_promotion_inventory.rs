//! XEP-0198 unavailable-resource custody survives a process restart.
pub mod ingress_support;

use ingress_support::IngressFixture;
use std::sync::Arc;
use waddle_server::{db::DatabaseDriver, sm_persistence::DatabaseSmPersistence};
use waddle_xmpp::{
    pending_delivery::SmSessionId,
    stream_management::persistence::SmPersistenceStorage,
    stream_management::{
        DetachedSessionSnapshot, InMemorySmSessionRegistry, SmSessionRegistry,
        StreamManagementState,
    },
    telemetry::attributes::SmEvictionPath,
    Stanza,
};

fn message(id: &str) -> Stanza {
    let mut message =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse().unwrap()));
    message.from = Some("bob@example.com/phone".parse().unwrap());
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "durable work".to_owned(),
    );
    Stanza::Message(message)
}

async fn durable_inventory(fixture: IngressFixture) {
    let storage = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .unwrap(),
    );
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let mut sm = StreamManagementState::new();
    sm.enable("promotion-inventory".to_owned(), true, Some(300));
    let _ = sm.record_outbound(
        waddle_xmpp::parser::stanza_to_string(match message("prefix") {
            Stanza::Message(message) => message,
            _ => unreachable!(),
        })
        .unwrap(),
        SmEvictionPath::DirectOutbound,
    );
    let mut session = sm
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice".to_owned(),
            jid: "alice@example.com/web".parse().unwrap(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .unwrap();
    session.promotion_custody =
        waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed();
    let stream = SmSessionId::new(&session.stream_id);
    let inventory = registry.register_promotion_session(&session).await.unwrap();
    registry
        .append_promotion_stanza(&session, &message("suffix"), 2, chrono::Utc::now())
        .await
        .unwrap();
    let persisted = storage.get_session(&stream).await.unwrap().unwrap();
    assert_eq!(persisted.outbound_count, 2);
    assert_eq!(persisted.max_resume_time, Some(0));
    assert_eq!(persisted.max_resume_duration, std::time::Duration::ZERO);
    assert_eq!(storage.list_unacked(&stream).await.unwrap().len(), 2);

    // Force a failure after payload INSERT but before watermark UPDATE.
    let rejection = match fixture.db.driver() {
        DatabaseDriver::Sqlite => "CREATE TRIGGER reject_inventory_watermark BEFORE UPDATE OF outbound_count ON sm_sessions WHEN NEW.outbound_count = 3 BEGIN SELECT RAISE(ABORT, 'injected watermark failure'); END",
        DatabaseDriver::Postgres => "ALTER TABLE sm_sessions ADD CONSTRAINT reject_inventory_watermark CHECK (outbound_count <> 3)",
    };
    fixture
        .db
        .guard()
        .await
        .unwrap()
        .execute(rejection, ())
        .await
        .unwrap();
    assert!(registry
        .append_promotion_stanza(&session, &message("rolled-back"), 3, chrono::Utc::now())
        .await
        .is_err());
    assert_eq!(
        storage
            .get_session(&stream)
            .await
            .unwrap()
            .unwrap()
            .outbound_count,
        2
    );
    assert_eq!(
        storage.list_unacked(&stream).await.unwrap().len(),
        2,
        "failed watermark commit must roll back the payload too"
    );

    // A successful sink may already have retired a row before a crash.
    registry
        .delete_unacked_sequences(&session.stream_id, &[2])
        .await
        .unwrap();
    registry
        .append_promotion_stanza(&session, &message("suffix"), 2, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        storage.list_unacked(&stream).await.unwrap().len(),
        1,
        "retrying an accepted sequence must not recreate a retired row"
    );
    let authoritative = registry
        .register_promotion_session(&inventory)
        .await
        .unwrap();
    assert_eq!(authoritative.unacked_stanzas.len(), 1);
    drop(registry);

    let restarted = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(restarted.restore_from_persistence().await.unwrap(), 1);
    assert!(restarted
        .peek_session(&session.stream_id)
        .await
        .unwrap()
        .is_none());
    let retry = restarted.drain_expired().await.unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].unacked_stanzas.len(), 1);
    assert!(retry[0].unacked_stanzas[0].stanza_xml.contains("prefix"));
    assert!(restarted.confirm_drained(&session.stream_id).await);
    assert!(storage.get_session(&stream).await.unwrap().is_none());
    assert!(storage.list_unacked(&stream).await.unwrap().is_empty());

    // Best-effort bulk metadata scans must not hide readable message rows
    // from the strict durable retraction sweep.
    let mut poisoned = session.clone();
    poisoned.stream_id = "poisoned-promotion".to_owned();
    poisoned.promotion_custody =
        waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed();
    let poison_registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    poison_registry
        .register_promotion_session(&poisoned)
        .await
        .unwrap();
    fixture
        .db
        .guard()
        .await
        .unwrap()
        .execute(
            "UPDATE sm_sessions SET occupancy_session = ? WHERE stream_id = ?",
            waddle_server::db_params!["invalid", poisoned.stream_id.as_str()],
        )
        .await
        .unwrap();
    assert!(storage.list_all_sessions().await.unwrap().is_empty());
    let target = waddle_xmpp::tombstone::TombstoneTarget::Direct {
        wire_id: "prefix".to_owned(),
        author: "bob@example.com".parse().unwrap(),
        archive: "alice@example.com".parse().unwrap(),
    };
    let scrubber = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(
        scrubber
            .snapshot_unacked_for_tombstone(&target)
            .await
            .unwrap()
            .len(),
        1
    );
    scrubber.scrub_unacked_for_tombstone(&target).await.unwrap();
    assert!(
        storage
            .list_unacked(&SmSessionId::new(&poisoned.stream_id))
            .await
            .unwrap()
            .is_empty(),
        "metadata poison must not retain retracted message content"
    );
    drop(storage);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_nonresumable_inventory_and_atomic_suffix_survive_restart() {
    durable_inventory(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_nonresumable_inventory_and_atomic_suffix_survive_restart() {
    let Some(fixture) = IngressFixture::postgres("sminv").await else {
        return;
    };
    durable_inventory(fixture).await;
}
