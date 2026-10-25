use super::{
    Arc, DatabaseSmPersistence, PersistedSession, PersistedUnackedStanza, SmPersistenceError,
    SmPersistenceStorage, SmSessionId, Stanza, Utc,
};
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

#[tokio::test]
async fn startup_quarantines_partially_corrupt_queues_without_publishing_replay_holes() {
    for corrupt in [
        "UPDATE sm_unacked SET stanza_xml = 'not XML' WHERE stream_id = ? AND sequence = ?",
        "UPDATE sm_unacked SET stanza_xml = 'not XML' WHERE stream_id = ? AND sequence >= ?",
        "UPDATE sm_unacked SET original_receipt_at_ms = 9223372036854775807 WHERE stream_id = ? AND sequence = ?",
        "UPDATE sm_unacked SET ingress_receipts = X'FF' WHERE stream_id = ? AND sequence = ?",
        "UPDATE sm_unacked SET sequence = 'invalid' WHERE stream_id = ? AND sequence = ?",
        "UPDATE sm_unacked SET sequence = -1 WHERE stream_id = ? AND sequence = ?",
        "UPDATE sm_unacked SET sequence = 4294967296 WHERE stream_id = ? AND sequence = ?",
    ] {
        for corrupt_sequence in 1..=3 {
            let storage = Arc::new(DatabaseSmPersistence::open(None).await.unwrap());
            for (stream, jid, count) in [
                ("alpha", "alice@example.com/web", 1),
                ("beta", "bob@example.com/web", 3),
                ("empty", "empty@example.com/web", 0),
                ("gamma", "carol@example.com/web", 1),
            ] {
                let mut source = session(stream);
                source.jid = jid.parse().unwrap();
                source.outbound_count = count;
                storage.upsert_session(source).await.unwrap();
                for sequence in 1..=count {
                    storage.append_unacked(PersistedUnackedStanza {
                        ingress_receipts: Vec::new(),
                        stream_id: SmSessionId::new(stream),
                        sequence,
                        stanza: Box::new(Stanza::Message(xmpp_parsers::message::Message::new(
                            None::<jid::Jid>,
                        ))),
                        original_receipt_at: Utc::now(),
                    }).await.unwrap();
                }
            }
            storage.execute(corrupt, crate::db_params!["beta", i64::from(corrupt_sequence)])
                .await.unwrap();
            let bulk = storage.list_all_sessions_with_unacked().await.unwrap();
            assert_eq!(bulk.iter().map(|(source, _)| source.stream_id.as_str())
                .collect::<Vec<_>>(), vec!["alpha", "empty", "gamma"], "{corrupt}");
            assert!(matches!(storage.list_unacked(&SmSessionId::new("beta")).await,
                Err(SmPersistenceError::Corrupt { .. })), "{corrupt}");

            let registry = InMemorySmSessionRegistry::with_capacity(3)
                .with_persistence(storage.clone());
            assert_eq!(registry.restore_from_persistence().await.unwrap(), 3, "{corrupt}");
            assert!(registry.claim_session("beta").await.unwrap().is_none(), "{corrupt}");
            assert!(storage.get_session(&SmSessionId::new("beta")).await.unwrap().is_none());
            assert!(storage.list_unacked(&SmSessionId::new("beta")).await.unwrap().is_empty());
            for stream in ["alpha", "gamma"] {
                let restored = registry.claim_session(stream).await.unwrap().unwrap();
                assert_eq!(restored.unacked_stanzas.iter().map(|entry| entry.sequence)
                    .collect::<Vec<_>>(), vec![1], "{stream}: {corrupt}");
                let durable = storage.list_unacked(&SmSessionId::new(stream)).await.unwrap();
                assert_eq!(durable.len(), 1);
                assert_eq!(durable[0].stream_id.as_str(), stream);
            }
            let empty = registry.claim_session("empty").await.unwrap().unwrap();
            assert!(empty.unacked_stanzas.is_empty());
        }
    }
}
