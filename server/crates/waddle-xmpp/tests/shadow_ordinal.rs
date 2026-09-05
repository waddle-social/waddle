use std::time::Duration;

use chrono::Utc;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    InMemorySmPersistence, PersistedSession, SmPersistenceStorage,
};
use waddle_xmpp::stream_management::{
    DetachedSessionSnapshot, ShadowOrdinal, StreamManagementState,
};

fn snapshot() -> DetachedSessionSnapshot {
    DetachedSessionSnapshot {
        user_id: "alice".to_string(),
        jid: "alice@example.com/web".parse().expect("valid full jid"),
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
    }
}

fn persisted_session(stream_id: &str, shadow_ordinal: ShadowOrdinal) -> PersistedSession {
    PersistedSession {
        stream_id: SmSessionId::new(stream_id),
        user_id: "alice".to_string(),
        jid: "alice@example.com/web".parse().expect("valid full jid"),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 3,
        shadow_ordinal,
        outbound_count: 5,
        last_acked: 4,
        replay_gap_through: None,
        max_resume_time: Some(300),
        detached_at: Utc::now(),
        max_resume_duration: Duration::from_secs(300),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
    }
}

#[test]
fn fresh_enable_starts_at_zero_and_detach_restore_round_trips() {
    let mut state = StreamManagementState::new();
    state.shadow_ordinal = ShadowOrdinal::from_storage(9);

    state.enable("shadow-stream".to_string(), true, Some(300));
    assert_eq!(state.shadow_ordinal, ShadowOrdinal::ZERO);

    state.shadow_ordinal = ShadowOrdinal::from_storage(17);
    let detached = state
        .to_detached_session(snapshot())
        .expect("resumable stream should detach");
    assert_eq!(detached.shadow_ordinal, ShadowOrdinal::from_storage(17));

    let mut restored = StreamManagementState::new();
    restored.restore_from_session(&detached);
    assert_eq!(restored.shadow_ordinal, ShadowOrdinal::from_storage(17));
}

#[tokio::test]
async fn persisted_sessions_round_trip_shadow_ordinal() {
    let store = InMemorySmPersistence::new();
    let stream_id = SmSessionId::new("persisted-shadow");
    let session = persisted_session(stream_id.as_str(), ShadowOrdinal::from_storage(29));

    store
        .upsert_session(session.clone())
        .await
        .expect("persist session");

    let loaded = store
        .get_session(&stream_id)
        .await
        .expect("load session")
        .expect("session present");

    assert_eq!(loaded.shadow_ordinal, ShadowOrdinal::from_storage(29));
}
