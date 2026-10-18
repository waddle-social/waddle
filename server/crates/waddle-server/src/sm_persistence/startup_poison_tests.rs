use super::*;
use std::time::Duration;
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};

fn session(stream: &str) -> PersistedSession {
    PersistedSession {
        stream_id: SmSessionId::new(stream),
        user_id: "alice".to_string(),
        jid: "alice@example.com/web".parse().unwrap(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 1,
        last_acked: 0,
        replay_gap_through: None,
        max_resume_time: Some(600),
        detached_at: Utc::now(),
        max_resume_duration: Duration::from_secs(600),
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

#[tokio::test]
async fn startup_quarantines_corrupt_sqlite_headers_and_preserves_healthy_neighbor() {
    for corrupt in [
        "UPDATE sm_sessions SET full_jid = 'alice@example.com' WHERE stream_id = ?",
        "UPDATE sm_sessions SET presence_show = 'bogus' WHERE stream_id = ?",
    ] {
        let storage = Arc::new(DatabaseSmPersistence::open(None).await.unwrap());
        for stream in ["healthy", "poison"] {
            storage.upsert_session(session(stream)).await.unwrap();
            storage
                .append_unacked(PersistedUnackedStanza {
                    ingress_receipts: Vec::new(),
                    stream_id: SmSessionId::new(stream),
                    sequence: 1,
                    stanza: Box::new(Stanza::Message(xmpp_parsers::message::Message::new(
                        None::<jid::Jid>,
                    ))),
                    original_receipt_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        storage
            .execute(corrupt, crate::db_params!["poison".to_string()])
            .await
            .unwrap();
        let bulk = storage.list_all_sessions_with_unacked().await.unwrap();
        assert_eq!(bulk.len(), 1);
        assert_eq!(bulk[0].0.stream_id.as_str(), "healthy");
        assert_eq!(storage.list_all_session_ids().await.unwrap().len(), 2);
        assert!(matches!(
            storage.get_session(&SmSessionId::new("poison")).await,
            Err(SmPersistenceError::Corrupt { .. })
        ));

        let registry =
            InMemorySmSessionRegistry::with_capacity(1).with_persistence(storage.clone());
        assert_eq!(registry.restore_from_persistence().await.unwrap(), 1);

        for query in [
            "SELECT COUNT(*) FROM sm_sessions WHERE stream_id = ?",
            "SELECT COUNT(*) FROM sm_unacked WHERE stream_id = ?",
        ] {
            let mut rows = storage
                .query(query, crate::db_params!["poison".to_string()])
                .await
                .unwrap();
            let row = rows.next().await.unwrap().unwrap();
            assert_eq!(row.get::<i64>(0).unwrap(), 0);
        }
        assert!(storage
            .get_session(&SmSessionId::new("healthy"))
            .await
            .unwrap()
            .is_some());
        let healthy_queue = storage
            .list_unacked(&SmSessionId::new("healthy"))
            .await
            .unwrap();
        assert_eq!(healthy_queue.len(), 1);
        assert_eq!(healthy_queue[0].sequence, 1);
        assert!(registry.peek_session("healthy").await.unwrap().is_some());
    }
}
