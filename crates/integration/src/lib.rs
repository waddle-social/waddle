#[cfg(all(test, feature = "native"))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Utc;
    use tempfile::TempDir;
    use tokio::time::timeout;

    use waddle_core::event::{
        BroadcastEventBus, Channel, ChatMessage, ChatState, Event, EventBus, EventPayload,
        EventSource, MessageType, MucAffiliation, MucOccupant, MucRole, PresenceShow, RosterItem,
        Subscription,
    };
    use waddle_mam::MamManager;
    use waddle_messaging::{MessageManager, MucManager};
    use waddle_presence::PresenceManager;
    use waddle_roster::RosterManager;
    use waddle_storage::{Database, Row, SqlValue};

    const TIMEOUT: Duration = Duration::from_millis(500);

    async fn setup_db(dir: &TempDir) -> Arc<impl Database + use<>> {
        let db_path = dir.path().join("test.db");
        let db = waddle_storage::open_database(&db_path)
            .await
            .expect("failed to open database");
        Arc::new(db)
    }

    fn make_event(channel: &str, payload: EventPayload) -> Event {
        Event::new(
            Channel::new(channel).unwrap(),
            EventSource::System("test".into()),
            payload,
        )
    }

    fn make_xmpp_event(channel: &str, payload: EventPayload) -> Event {
        Event::new(Channel::new(channel).unwrap(), EventSource::Xmpp, payload)
    }

    fn make_chat_message(id: &str, from: &str, to: &str, body: &str) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            body: body.to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Chat,
            thread: None,
        }
    }

    // ── 1. Connection/Auth ───────────────────────────────────────────
    // Verify that ConnectionEstablished propagates to all managers and
    // triggers the correct downstream behaviours.

    #[tokio::test]
    async fn connection_established_triggers_roster_fetch_and_presence_wait() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));
        let presence = Arc::new(PresenceManager::new(bus.clone()));

        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        roster.handle_event(&connected).await;
        presence.handle_event(&connected).await;

        // Roster manager should emit RosterFetchRequested
        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(event.payload, EventPayload::RosterFetchRequested));

        // Presence should still be Unavailable (waiting for roster)
        let own = presence.own_presence();
        assert!(matches!(own.show, PresenceShow::Unavailable));
        assert_eq!(own.jid, "alice@example.com");
    }

    #[tokio::test]
    async fn connection_lost_propagates_to_all_managers() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let presence = Arc::new(PresenceManager::new(bus.clone()));
        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

        // Establish connection first
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        presence.handle_event(&connected).await;
        messaging.handle_event(&connected).await;

        // Now lose it
        let lost = make_event(
            "system.connection.lost",
            EventPayload::ConnectionLost {
                reason: "network error".to_string(),
                will_retry: true,
            },
        );
        presence.handle_event(&lost).await;
        messaging.handle_event(&lost).await;

        assert!(matches!(
            presence.own_presence().show,
            PresenceShow::Unavailable
        ));

        // Messaging should be offline - sends should enqueue
        let msg = messaging
            .send_message("bob@example.com", "offline msg")
            .await
            .unwrap();
        assert!(!msg.id.is_empty());

        // Verify it was enqueued (no ui event emitted)
        let mut sub = bus.subscribe("ui.message.send").unwrap();
        let result = timeout(Duration::from_millis(50), sub.recv()).await;
        assert!(result.is_err(), "offline send should not emit ui event");
    }

    // ── 2. Roster Sync ──────────────────────────────────────────────
    // Connection → RosterManager fetch → RosterReceived → persists
    // → PresenceManager gets roster → sends initial presence

    #[tokio::test]
    async fn roster_sync_flow_connection_to_initial_presence() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));
        let presence = Arc::new(PresenceManager::new(bus.clone()));

        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        // Step 1: Connection established
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        roster.handle_event(&connected).await;
        presence.handle_event(&connected).await;

        // Drain the RosterFetchRequested event
        let _ = timeout(TIMEOUT, ui_sub.recv()).await;

        // Step 2: Server sends full roster
        let items = vec![
            RosterItem {
                jid: "bob@example.com".to_string(),
                name: Some("Bob".to_string()),
                subscription: Subscription::Both,
                groups: vec!["Friends".to_string()],
            },
            RosterItem {
                jid: "carol@example.com".to_string(),
                name: None,
                subscription: Subscription::To,
                groups: vec![],
            },
        ];
        let roster_received = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived {
                items: items.clone(),
            },
        );
        roster.handle_event(&roster_received).await;
        presence.handle_event(&roster_received).await;

        // Step 3: Verify roster persisted
        let stored = roster.get_roster().await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].jid, "bob@example.com");
        assert_eq!(stored[0].name, Some("Bob".to_string()));
        assert!(matches!(stored[0].subscription, Subscription::Both));
        assert_eq!(stored[0].groups, vec!["Friends"]);
        assert_eq!(stored[1].jid, "carol@example.com");

        // Step 4: Presence manager should have sent initial presence
        let own = presence.own_presence();
        assert!(matches!(own.show, PresenceShow::Available));

        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out waiting for initial presence")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::PresenceSetRequested {
                show: PresenceShow::Available,
                status: None,
            }
        ));
    }

    #[tokio::test]
    async fn roster_push_updates_persist_incrementally() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));

        // Seed with initial roster
        let initial = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived {
                items: vec![RosterItem {
                    jid: "bob@example.com".to_string(),
                    name: Some("Bob".to_string()),
                    subscription: Subscription::Both,
                    groups: vec![],
                }],
            },
        );
        roster.handle_event(&initial).await;

        // Roster push: add new contact
        let push = make_xmpp_event(
            "xmpp.roster.updated",
            EventPayload::RosterUpdated {
                item: RosterItem {
                    jid: "dave@example.com".to_string(),
                    name: Some("Dave".to_string()),
                    subscription: Subscription::None,
                    groups: vec!["Work".to_string()],
                },
            },
        );
        roster.handle_event(&push).await;

        let stored = roster.get_roster().await.unwrap();
        assert_eq!(stored.len(), 2);

        // Roster push: remove contact
        let remove = make_xmpp_event(
            "xmpp.roster.removed",
            EventPayload::RosterRemoved {
                jid: "bob@example.com".to_string(),
            },
        );
        roster.handle_event(&remove).await;

        let stored = roster.get_roster().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].jid, "dave@example.com");
    }

    // ── 3. 1:1 Messaging ────────────────────────────────────────────
    // MessageManager send/receive with persistence and events

    #[tokio::test]
    async fn one_to_one_messaging_send_receive_persist() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

        // Bring online
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;

        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        // Send a message
        let sent = messaging
            .send_message("bob@example.com", "Hello Bob!")
            .await
            .unwrap();
        assert!(!sent.id.is_empty());
        assert_eq!(sent.to, "bob@example.com");

        // Verify send event emitted
        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::MessageSendRequested { ref to, ref body, .. }
            if to == "bob@example.com" && body == "Hello Bob!"
        ));

        // Simulate receiving a reply
        let reply = make_chat_message("reply-1", "bob@example.com", "alice@example.com", "Hey!");
        let received_event = make_xmpp_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: reply.clone(),
            },
        );
        messaging.handle_event(&received_event).await;

        // Verify both messages persisted
        let messages = messaging
            .get_messages("bob@example.com", 50, None)
            .await
            .unwrap();
        assert_eq!(messages.len(), 2);

        let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
        assert!(bodies.contains(&"Hello Bob!"));
        assert!(bodies.contains(&"Hey!"));
    }

    #[tokio::test]
    async fn message_delivery_receipt_flow() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

        // Send while offline (enqueues)
        let sent = messaging
            .send_message("bob@example.com", "queued message")
            .await
            .unwrap();

        // Come online - drains queue
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;

        // Simulate server echo (MessageSent)
        let echo = make_xmpp_event(
            "xmpp.message.sent",
            EventPayload::MessageSent {
                message: make_chat_message(
                    &sent.id,
                    "alice@example.com",
                    "bob@example.com",
                    "queued message",
                ),
            },
        );
        messaging.handle_event(&echo).await;

        // Simulate delivery receipt
        let receipt = make_xmpp_event(
            "xmpp.message.delivered",
            EventPayload::MessageDelivered {
                id: sent.id.clone(),
                to: "bob@example.com".to_string(),
            },
        );
        messaging.handle_event(&receipt).await;

        // Verify queue item is confirmed
        let rows: Vec<waddle_storage::Row> = db
            .query(
                "SELECT status FROM offline_queue ORDER BY id ASC LIMIT 1",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            rows[0].get(0),
            Some(&waddle_storage::SqlValue::Text("confirmed".to_string()))
        );
    }

    #[tokio::test]
    async fn chat_state_notifications_across_messaging() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

        // Bring online
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;

        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        // Send composing state
        messaging
            .send_chat_state("bob@example.com", ChatState::Composing)
            .await
            .unwrap();

        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::ChatStateSendRequested {
                ref to,
                state: ChatState::Composing,
            } if to == "bob@example.com"
        ));

        // Receive chat state from peer
        let peer_state = make_xmpp_event(
            "xmpp.chatstate.received",
            EventPayload::ChatStateReceived {
                from: "bob@example.com".to_string(),
                state: ChatState::Active,
            },
        );
        messaging.handle_event(&peer_state).await;
        // No panic = success; chat state handling is log-only
    }

    // ── 4. MUC Messaging ────────────────────────────────────────────
    // MucManager join/leave/send with occupant tracking and message persistence

    #[tokio::test]
    async fn muc_join_message_occupant_leave_flow() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let muc = Arc::new(MucManager::new(db.clone(), bus.clone()));

        // Join a room
        muc.join_room("room@conference.example.com", "Alice")
            .await
            .unwrap();

        // Server confirms join
        let joined = make_xmpp_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "room@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        muc.handle_event(&joined).await;

        let rooms = muc.get_joined_rooms().await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert!(rooms[0].joined);

        // Occupants join
        let bob_join = make_xmpp_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room@conference.example.com".to_string(),
                occupant: MucOccupant {
                    nick: "Bob".to_string(),
                    jid: Some("bob@example.com".to_string()),
                    affiliation: MucAffiliation::Member,
                    role: MucRole::Participant,
                },
            },
        );
        muc.handle_event(&bob_join).await;

        let occupants = muc.get_occupants("room@conference.example.com");
        assert_eq!(occupants.len(), 1);
        assert_eq!(occupants[0].nick, "Bob");

        // Receive a room message
        let msg = ChatMessage {
            id: "muc-msg-1".to_string(),
            from: "room@conference.example.com/Bob".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Hello room!".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Groupchat,
            thread: None,
        };
        let msg_event = make_xmpp_event(
            "xmpp.muc.message.received",
            EventPayload::MucMessageReceived {
                room: "room@conference.example.com".to_string(),
                message: msg,
            },
        );
        muc.handle_event(&msg_event).await;

        let messages = muc
            .get_room_messages("room@conference.example.com", 50, None)
            .await
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "Hello room!");

        // Subject change
        let subject = make_xmpp_event(
            "xmpp.muc.subject.changed",
            EventPayload::MucSubjectChanged {
                room: "room@conference.example.com".to_string(),
                subject: "Sprint Planning".to_string(),
            },
        );
        muc.handle_event(&subject).await;

        let rooms = muc.get_joined_rooms().await.unwrap();
        assert_eq!(rooms[0].subject, Some("Sprint Planning".to_string()));

        // Bob leaves (role=None)
        let bob_leave = make_xmpp_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room@conference.example.com".to_string(),
                occupant: MucOccupant {
                    nick: "Bob".to_string(),
                    jid: Some("bob@example.com".to_string()),
                    affiliation: MucAffiliation::Member,
                    role: MucRole::None,
                },
            },
        );
        muc.handle_event(&bob_leave).await;
        assert!(muc.get_occupants("room@conference.example.com").is_empty());

        // We leave
        let left = make_xmpp_event(
            "xmpp.muc.left",
            EventPayload::MucLeft {
                room: "room@conference.example.com".to_string(),
            },
        );
        muc.handle_event(&left).await;

        let joined = muc.get_joined_rooms().await.unwrap();
        assert!(joined.is_empty());
    }

    #[tokio::test]
    async fn muc_send_emits_event() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let muc = Arc::new(MucManager::new(db.clone(), bus.clone()));
        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        muc.send_message("room@conference.example.com", "Hey everyone!")
            .await
            .unwrap();

        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::MucSendRequested { ref room, ref body }
            if room == "room@conference.example.com" && body == "Hey everyone!"
        ));
    }

    // ── 5. MAM Sync ─────────────────────────────────────────────────
    // MamManager sync_since with event-based paginated query/response
    // and connection → presence → MAM trigger flow

    #[tokio::test]
    async fn mam_sync_triggered_by_own_presence_after_connection() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = TempDir::new().unwrap();
                let db = setup_db(&dir).await;
                let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

                let mam = Arc::new(MamManager::new(db.clone(), bus.clone()));
                let presence = Arc::new(PresenceManager::new(bus.clone()));

                let mut ui_sub = bus.subscribe("ui.**").unwrap();

                // Step 1: Connection
                let connected = make_event(
                    "system.connection.established",
                    EventPayload::ConnectionEstablished {
                        jid: "alice@example.com".to_string(),
                    },
                );
                mam.handle_event(&connected).await;
                presence.handle_event(&connected).await;

                // No MAM query yet
                let no_query = timeout(Duration::from_millis(50), ui_sub.recv()).await;
                assert!(no_query.is_err(), "MAM should wait for own presence");

                // Step 2: Roster received → presence sends initial Available
                let roster_event = make_xmpp_event(
                    "xmpp.roster.received",
                    EventPayload::RosterReceived { items: vec![] },
                );
                presence.handle_event(&roster_event).await;

                // Drain the PresenceSetRequested from presence manager
                let _pres_event = timeout(TIMEOUT, ui_sub.recv()).await;

                // Step 3: OwnPresenceChanged triggers MAM sync
                let own_presence = make_xmpp_event(
                    "xmpp.presence.own_changed",
                    EventPayload::OwnPresenceChanged {
                        show: PresenceShow::Available,
                        status: None,
                    },
                );

                // Spawn handle_event so we can respond to the MAM query
                let mam_clone = mam.clone();
                let handle = tokio::task::spawn_local(async move {
                    mam_clone.handle_event(&own_presence).await;
                });

                // Wait for the MAM query request
                let query_event = timeout(TIMEOUT, ui_sub.recv())
                    .await
                    .expect("timed out waiting for MAM query")
                    .unwrap();

                let query_id = match &query_event.payload {
                    EventPayload::MamQueryRequested { query_id, .. } => query_id.clone(),
                    other => panic!("expected MamQueryRequested, got {other:?}"),
                };

                // Simulate MAM result
                let msg = make_chat_message(
                    "arch-1",
                    "bob@example.com",
                    "alice@example.com",
                    "Missed message",
                );
                bus.publish(Event::new(
                    Channel::new("xmpp.mam.result.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamResultReceived {
                        query_id: query_id.clone(),
                        messages: vec![msg],
                        complete: false,
                    },
                ))
                .unwrap();

                // Simulate MAM fin
                bus.publish(Event::new(
                    Channel::new("xmpp.mam.fin.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamFinReceived {
                        iq_id: query_id,
                        complete: true,
                        last_id: Some("arch-1".to_string()),
                    },
                ))
                .unwrap();

                timeout(Duration::from_secs(5), handle)
                    .await
                    .expect("MAM sync timed out")
                    .expect("MAM sync should not panic");

                // Verify message persisted
                let rows: Vec<waddle_storage::Row> = db
                    .query("SELECT COUNT(*) FROM messages", &[])
                    .await
                    .unwrap();
                assert_eq!(rows[0].get(0), Some(&waddle_storage::SqlValue::Integer(1)));

                // Verify sync state updated
                let state: Vec<waddle_storage::Row> = db
                    .query(
                        "SELECT last_stanza_id FROM mam_sync_state WHERE jid = '__global__'",
                        &[],
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    state[0].get(0),
                    Some(&waddle_storage::SqlValue::Text("arch-1".to_string()))
                );
            })
            .await;
    }

    #[tokio::test]
    async fn mam_unavailable_presence_does_not_trigger_sync() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let mam = Arc::new(MamManager::new(db.clone(), bus.clone()));

        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        // Connection
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        mam.handle_event(&connected).await;

        // OwnPresenceChanged with Unavailable should NOT trigger sync
        let unavailable = make_xmpp_event(
            "xmpp.presence.own_changed",
            EventPayload::OwnPresenceChanged {
                show: PresenceShow::Unavailable,
                status: None,
            },
        );
        mam.handle_event(&unavailable).await;

        let no_query = timeout(Duration::from_millis(50), ui_sub.recv()).await;
        assert!(
            no_query.is_err(),
            "unavailable presence should not trigger MAM sync"
        );
    }

    // ── 6. Offline Queue Drain ──────────────────────────────────────
    // MessageManager offline enqueue → reconnect → drain FIFO → status lifecycle

    #[tokio::test]
    async fn offline_queue_enqueue_drain_and_reconcile() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

        // Send messages while offline (both enqueued)
        let msg1 = messaging
            .send_message("bob@example.com", "first")
            .await
            .unwrap();
        let msg2 = messaging
            .send_message("carol@example.com", "second")
            .await
            .unwrap();

        // Verify messages persisted even while offline
        let stored = messaging
            .get_messages("bob@example.com", 50, None)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].body, "first");

        // Verify queue has 2 pending items
        let rows: Vec<waddle_storage::Row> = db
            .query(
                "SELECT payload FROM offline_queue WHERE status = 'pending' ORDER BY id ASC",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);

        // Subscribe to drained events
        let mut ui_sub = bus.subscribe("ui.message.send").unwrap();

        // Come online - triggers drain
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;

        // Verify FIFO order of drained events
        let first_event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            first_event.payload,
            EventPayload::MessageSendRequested { ref body, .. } if body == "first"
        ));

        let second_event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            second_event.payload,
            EventPayload::MessageSendRequested { ref body, .. } if body == "second"
        ));

        // Simulate server echoing the messages back (MessageSent)
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &msg1.id,
                        "alice@example.com",
                        "bob@example.com",
                        "first",
                    ),
                },
            ))
            .await;

        // Check first message moved to "sent"
        let rows: Vec<waddle_storage::Row> = db
            .query("SELECT status FROM offline_queue ORDER BY id ASC", &[])
            .await
            .unwrap();
        assert_eq!(
            rows[0].get(0),
            Some(&waddle_storage::SqlValue::Text("sent".to_string()))
        );

        // Simulate delivery receipt for first message
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.message.delivered",
                EventPayload::MessageDelivered {
                    id: msg1.id.clone(),
                    to: "bob@example.com".to_string(),
                },
            ))
            .await;

        let rows: Vec<waddle_storage::Row> = db
            .query("SELECT status FROM offline_queue ORDER BY id ASC", &[])
            .await
            .unwrap();
        assert_eq!(
            rows[0].get(0),
            Some(&waddle_storage::SqlValue::Text("confirmed".to_string()))
        );

        // MAM reconciliation for second message
        let mam_msg = ChatMessage {
            id: "archive-id-99".to_string(),
            from: "alice@example.com".to_string(),
            to: "carol@example.com".to_string(),
            body: "second".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Chat,
            thread: None,
        };

        // First mark second as sent
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &msg2.id,
                        "alice@example.com",
                        "carol@example.com",
                        "second",
                    ),
                },
            ))
            .await;

        // Then reconcile via MAM result
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.mam.result.received",
                EventPayload::MamResultReceived {
                    query_id: "q1".to_string(),
                    messages: vec![mam_msg],
                    complete: true,
                },
            ))
            .await;

        let rows: Vec<waddle_storage::Row> = db
            .query("SELECT status FROM offline_queue ORDER BY id ASC", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].get(0),
            Some(&waddle_storage::SqlValue::Text("confirmed".to_string()))
        );
        assert_eq!(
            rows[1].get(0),
            Some(&waddle_storage::SqlValue::Text("confirmed".to_string()))
        );
    }

    #[tokio::test]
    async fn offline_queue_non_message_commands_auto_confirmed() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));
        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));

        // Add a contact while offline - roster emits the event, messaging
        // intercepts the ui.roster.add event and enqueues it
        roster
            .add_contact("dave@example.com", Some("Dave"), &[])
            .await
            .unwrap();

        // The roster_add event was published on the bus but messaging is offline
        // so let's manually handle it as an offline command event
        let add_event = Event::new(
            Channel::new("ui.roster.add").unwrap(),
            EventSource::System("roster".into()),
            EventPayload::RosterAddRequested {
                jid: "dave@example.com".to_string(),
                name: Some("Dave".to_string()),
                groups: vec![],
            },
        );
        messaging.handle_event(&add_event).await;

        // Verify it was enqueued
        let rows: Vec<waddle_storage::Row> = db
            .query("SELECT stanza_type, status FROM offline_queue", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get(0),
            Some(&waddle_storage::SqlValue::Text("iq".to_string()))
        );
        assert_eq!(
            rows[0].get(1),
            Some(&waddle_storage::SqlValue::Text("pending".to_string()))
        );

        // Come online - drain; non-message commands go straight to confirmed
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;

        let rows: Vec<waddle_storage::Row> = db
            .query("SELECT status FROM offline_queue", &[])
            .await
            .unwrap();
        assert_eq!(
            rows[0].get(0),
            Some(&waddle_storage::SqlValue::Text("confirmed".to_string()))
        );
    }

    // ── Cross-manager: presence tracks contacts from roster events ──

    #[tokio::test]
    async fn presence_tracks_contacts_after_roster_and_connection() {
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let presence = Arc::new(PresenceManager::new(bus.clone()));

        // Connection
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        presence.handle_event(&connected).await;

        // Roster received
        let roster = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived { items: vec![] },
        );
        presence.handle_event(&roster).await;

        // Contact comes online
        let bob_available = make_xmpp_event(
            "xmpp.presence.changed",
            EventPayload::PresenceChanged {
                jid: "bob@example.com/desktop".to_string(),
                show: PresenceShow::Available,
                status: Some("online".to_string()),
                priority: 5,
            },
        );
        presence.handle_event(&bob_available).await;

        let info = presence.get_presence("bob@example.com");
        assert!(matches!(info.show, PresenceShow::Available));
        assert_eq!(info.status, Some("online".to_string()));

        // Bob on mobile with higher priority
        let bob_mobile = make_xmpp_event(
            "xmpp.presence.changed",
            EventPayload::PresenceChanged {
                jid: "bob@example.com/mobile".to_string(),
                show: PresenceShow::Away,
                status: Some("on phone".to_string()),
                priority: 10,
            },
        );
        presence.handle_event(&bob_mobile).await;

        let info = presence.get_presence("bob@example.com");
        assert!(matches!(info.show, PresenceShow::Away));
        assert_eq!(info.priority, 10);

        // Connection lost clears all
        let lost = make_event(
            "system.connection.lost",
            EventPayload::ConnectionLost {
                reason: "timeout".to_string(),
                will_retry: true,
            },
        );
        presence.handle_event(&lost).await;

        let info = presence.get_presence("bob@example.com");
        assert!(matches!(info.show, PresenceShow::Unavailable));
    }

    // ── Full lifecycle: connection → roster → presence → MAM ────────

    #[tokio::test]
    async fn full_startup_lifecycle_connection_to_mam_sync() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = TempDir::new().unwrap();
                let db = setup_db(&dir).await;
                let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

                let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));
                let presence = Arc::new(PresenceManager::new(bus.clone()));
                let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));
                let mam = Arc::new(MamManager::new(db.clone(), bus.clone()));

                let mut ui_sub = bus.subscribe("ui.**").unwrap();
                let mut sys_sub = bus.subscribe("system.**").unwrap();

                // 1. ConnectionEstablished
                let connected = make_event(
                    "system.connection.established",
                    EventPayload::ConnectionEstablished {
                        jid: "alice@example.com".to_string(),
                    },
                );
                roster.handle_event(&connected).await;
                presence.handle_event(&connected).await;
                messaging.handle_event(&connected).await;
                mam.handle_event(&connected).await;

                // Drain RosterFetchRequested
                let fetch = timeout(TIMEOUT, ui_sub.recv())
                    .await
                    .expect("timed out")
                    .unwrap();
                assert!(matches!(fetch.payload, EventPayload::RosterFetchRequested));

                // Drain ComingOnline from messaging
                let coming = timeout(TIMEOUT, sys_sub.recv())
                    .await
                    .expect("timed out")
                    .unwrap();
                assert!(matches!(coming.payload, EventPayload::ComingOnline));

                // 2. RosterReceived
                let roster_event = make_xmpp_event(
                    "xmpp.roster.received",
                    EventPayload::RosterReceived {
                        items: vec![RosterItem {
                            jid: "bob@example.com".to_string(),
                            name: Some("Bob".to_string()),
                            subscription: Subscription::Both,
                            groups: vec![],
                        }],
                    },
                );
                roster.handle_event(&roster_event).await;
                presence.handle_event(&roster_event).await;

                // Verify roster stored
                let stored = roster.get_roster().await.unwrap();
                assert_eq!(stored.len(), 1);

                // 3. Presence sends initial Available
                let pres_event = timeout(TIMEOUT, ui_sub.recv())
                    .await
                    .expect("timed out")
                    .unwrap();
                assert!(matches!(
                    pres_event.payload,
                    EventPayload::PresenceSetRequested {
                        show: PresenceShow::Available,
                        ..
                    }
                ));

                // 4. OwnPresenceChanged triggers MAM sync
                let own_presence = make_xmpp_event(
                    "xmpp.presence.own_changed",
                    EventPayload::OwnPresenceChanged {
                        show: PresenceShow::Available,
                        status: None,
                    },
                );
                presence.handle_event(&own_presence).await;

                let mam_clone = mam.clone();
                let mam_handle = tokio::task::spawn_local(async move {
                    mam_clone.handle_event(&own_presence).await;
                });

                // 5. MAM queries for catch-up
                let query_event = timeout(TIMEOUT, ui_sub.recv())
                    .await
                    .expect("timed out waiting for MAM query")
                    .unwrap();

                let query_id = match &query_event.payload {
                    EventPayload::MamQueryRequested { query_id, .. } => query_id.clone(),
                    other => panic!("expected MamQueryRequested, got {other:?}"),
                };

                // Respond with empty archive
                bus.publish(Event::new(
                    Channel::new("xmpp.mam.fin.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamFinReceived {
                        iq_id: query_id,
                        complete: true,
                        last_id: None,
                    },
                ))
                .unwrap();

                timeout(Duration::from_secs(5), mam_handle)
                    .await
                    .expect("MAM handle timed out")
                    .expect("MAM handle should not panic");

                // 6. SyncStarted and SyncCompleted events
                let started = timeout(TIMEOUT, sys_sub.recv())
                    .await
                    .expect("timed out waiting for SyncStarted")
                    .unwrap();
                assert!(matches!(started.payload, EventPayload::SyncStarted));

                let completed = timeout(TIMEOUT, sys_sub.recv())
                    .await
                    .expect("timed out waiting for SyncCompleted")
                    .unwrap();
                assert!(matches!(
                    completed.payload,
                    EventPayload::SyncCompleted { messages_synced: 0 }
                ));
                assert_eq!(started.correlation_id, completed.correlation_id);
            })
            .await;
    }

    // ── 7. Reconnection Flow ─────────────────────────────────────
    // Disconnect → enqueue messages → reconnect → verify drain and
    // state recovery across all managers

    #[tokio::test]
    async fn reconnection_drains_queue_and_restores_state() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));
        let presence = Arc::new(PresenceManager::new(bus.clone()));
        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));

        // Initial connection
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;
        presence.handle_event(&connected).await;
        roster.handle_event(&connected).await;

        // Seed roster so presence becomes Available
        let roster_event = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived {
                items: vec![RosterItem {
                    jid: "bob@example.com".to_string(),
                    name: Some("Bob".to_string()),
                    subscription: Subscription::Both,
                    groups: vec![],
                }],
            },
        );
        roster.handle_event(&roster_event).await;
        presence.handle_event(&roster_event).await;

        assert!(matches!(
            presence.own_presence().show,
            PresenceShow::Available
        ));

        // Bob comes online
        let bob_online = make_xmpp_event(
            "xmpp.presence.changed",
            EventPayload::PresenceChanged {
                jid: "bob@example.com/laptop".to_string(),
                show: PresenceShow::Available,
                status: None,
                priority: 0,
            },
        );
        presence.handle_event(&bob_online).await;
        assert!(matches!(
            presence.get_presence("bob@example.com").show,
            PresenceShow::Available
        ));

        // Lose connection
        let lost = make_event(
            "system.connection.lost",
            EventPayload::ConnectionLost {
                reason: "network error".to_string(),
                will_retry: true,
            },
        );
        messaging.handle_event(&lost).await;
        presence.handle_event(&lost).await;

        // Presence should be cleared
        assert!(matches!(
            presence.own_presence().show,
            PresenceShow::Unavailable
        ));
        assert!(matches!(
            presence.get_presence("bob@example.com").show,
            PresenceShow::Unavailable
        ));

        // Send messages while offline (enqueued)
        messaging
            .send_message("bob@example.com", "missed you")
            .await
            .unwrap();

        // Reconnect
        let mut ui_sub = bus.subscribe("ui.**").unwrap();
        let reconnected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&reconnected).await;
        presence.handle_event(&reconnected).await;
        roster.handle_event(&reconnected).await;

        // Both RosterFetchRequested and drained MessageSendRequested are emitted
        // (order depends on which manager processes first)
        let mut saw_roster_fetch = false;
        let mut saw_drained_msg = false;
        for _ in 0..2 {
            let event = timeout(TIMEOUT, ui_sub.recv())
                .await
                .expect("timed out waiting for reconnect events")
                .unwrap();
            match &event.payload {
                EventPayload::RosterFetchRequested => saw_roster_fetch = true,
                EventPayload::MessageSendRequested { body, .. } if body == "missed you" => {
                    saw_drained_msg = true
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(saw_roster_fetch, "expected RosterFetchRequested");
        assert!(saw_drained_msg, "expected drained message");

        // Roster persisted from before disconnect
        let stored = roster.get_roster().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].jid, "bob@example.com");
    }

    // ── 8. Subscription Request/Approval Flow ────────────────────
    // Full subscription lifecycle: request → approve → roster update

    #[tokio::test]
    async fn subscription_request_approve_flow() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));

        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        // Seed initial roster
        let initial = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived {
                items: vec![RosterItem {
                    jid: "bob@example.com".to_string(),
                    name: Some("Bob".to_string()),
                    subscription: Subscription::None,
                    groups: vec![],
                }],
            },
        );
        roster.handle_event(&initial).await;

        // Inbound subscription request
        let sub_request = make_xmpp_event(
            "xmpp.subscription.request",
            EventPayload::SubscriptionRequest {
                from: "carol@example.com".to_string(),
            },
        );
        roster.handle_event(&sub_request).await;

        // Approve the subscription
        roster
            .approve_subscription("carol@example.com")
            .await
            .unwrap();

        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::SubscriptionRespondRequested {
                ref jid,
                accept: true,
            } if jid == "carol@example.com"
        ));

        // Request our own subscription to carol
        roster
            .request_subscription("carol@example.com")
            .await
            .unwrap();

        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::SubscriptionSendRequested {
                ref jid,
                subscribe: true,
            } if jid == "carol@example.com"
        ));

        // Server pushes updated roster with mutual subscription
        let push = make_xmpp_event(
            "xmpp.roster.updated",
            EventPayload::RosterUpdated {
                item: RosterItem {
                    jid: "carol@example.com".to_string(),
                    name: Some("Carol".to_string()),
                    subscription: Subscription::Both,
                    groups: vec!["Friends".to_string()],
                },
            },
        );
        roster.handle_event(&push).await;

        let stored = roster.get_roster().await.unwrap();
        let carol = stored.iter().find(|r| r.jid == "carol@example.com");
        assert!(carol.is_some());
        let carol = carol.unwrap();
        assert!(matches!(carol.subscription, Subscription::Both));
        assert_eq!(carol.groups, vec!["Friends"]);
    }

    #[tokio::test]
    async fn subscription_deny_and_unsubscribe_flow() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));

        let mut ui_sub = bus.subscribe("ui.**").unwrap();

        // Deny an inbound request
        roster
            .deny_subscription("spammer@example.com")
            .await
            .unwrap();

        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::SubscriptionRespondRequested {
                ref jid,
                accept: false,
            } if jid == "spammer@example.com"
        ));

        // Seed a contact then unsubscribe
        let initial = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived {
                items: vec![RosterItem {
                    jid: "dave@example.com".to_string(),
                    name: Some("Dave".to_string()),
                    subscription: Subscription::Both,
                    groups: vec![],
                }],
            },
        );
        roster.handle_event(&initial).await;

        roster.unsubscribe("dave@example.com").await.unwrap();

        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::SubscriptionSendRequested {
                ref jid,
                subscribe: false,
            } if jid == "dave@example.com"
        ));
    }

    // ── 9. MUC + Messaging + Presence Cross-Manager ──────────────
    // Verifies MUC state, presence, and 1:1 messaging coexist

    #[tokio::test]
    async fn muc_and_direct_messaging_coexist() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));
        let muc = Arc::new(MucManager::new(db.clone(), bus.clone()));
        let presence = Arc::new(PresenceManager::new(bus.clone()));

        // Bring everything online
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;
        presence.handle_event(&connected).await;

        // Join a MUC room
        muc.join_room("dev@conference.example.com", "Alice")
            .await
            .unwrap();
        let joined = make_xmpp_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "dev@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        muc.handle_event(&joined).await;

        // Send a direct message
        let sent = messaging
            .send_message("bob@example.com", "Direct hello")
            .await
            .unwrap();
        assert_eq!(sent.to, "bob@example.com");

        // Send a MUC message
        muc.send_message("dev@conference.example.com", "Room hello")
            .await
            .unwrap();

        // Receive a MUC message
        let muc_msg = ChatMessage {
            id: "muc-1".to_string(),
            from: "dev@conference.example.com/Bob".to_string(),
            to: "dev@conference.example.com".to_string(),
            body: "Hey from room".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Groupchat,
            thread: None,
        };
        let muc_recv = make_xmpp_event(
            "xmpp.muc.message.received",
            EventPayload::MucMessageReceived {
                room: "dev@conference.example.com".to_string(),
                message: muc_msg,
            },
        );
        muc.handle_event(&muc_recv).await;

        // Receive a direct message
        let direct_msg =
            make_chat_message("dm-1", "bob@example.com", "alice@example.com", "Hey direct");
        let direct_recv = make_xmpp_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: direct_msg,
            },
        );
        messaging.handle_event(&direct_recv).await;

        // Verify both message stores are independent
        let direct_messages = messaging
            .get_messages("bob@example.com", 50, None)
            .await
            .unwrap();
        assert_eq!(direct_messages.len(), 2); // sent + received
        assert!(direct_messages.iter().any(|m| m.body == "Direct hello"));
        assert!(direct_messages.iter().any(|m| m.body == "Hey direct"));

        let room_messages = muc
            .get_room_messages("dev@conference.example.com", 50, None)
            .await
            .unwrap();
        assert_eq!(room_messages.len(), 1);
        assert_eq!(room_messages[0].body, "Hey from room");

        // Verify MUC state unaffected by direct messaging
        let rooms = muc.get_joined_rooms().await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert!(rooms[0].joined);
    }

    // ── 10. MAM Paginated Sync with Deduplication ────────────────
    // Multi-page MAM sync: page1 incomplete → page2 complete, with
    // deduplication of already-stored messages

    #[tokio::test]
    async fn mam_paginated_sync_with_dedup() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = TempDir::new().unwrap();
                let db = setup_db(&dir).await;
                let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

                let mam = Arc::new(MamManager::new(db.clone(), bus.clone()));
                let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

                let mut ui_sub = bus.subscribe("ui.**").unwrap();

                // Pre-store a message that MAM will also return (dedup test)
                let existing =
                    make_chat_message("msg-2", "bob@example.com", "alice@example.com", "Dup msg");
                let recv_event = make_xmpp_event(
                    "xmpp.message.received",
                    EventPayload::MessageReceived { message: existing },
                );
                messaging.handle_event(&recv_event).await;

                // Verify the pre-stored message is there
                let before_count: Vec<Row> = db
                    .query("SELECT COUNT(*) FROM messages", &[])
                    .await
                    .unwrap();
                assert_eq!(before_count[0].get(0), Some(&SqlValue::Integer(1)));

                // Connection + presence to trigger MAM
                let connected = make_event(
                    "system.connection.established",
                    EventPayload::ConnectionEstablished {
                        jid: "alice@example.com".to_string(),
                    },
                );
                mam.handle_event(&connected).await;

                let own_presence = make_xmpp_event(
                    "xmpp.presence.own_changed",
                    EventPayload::OwnPresenceChanged {
                        show: PresenceShow::Available,
                        status: None,
                    },
                );

                let mam_clone = mam.clone();
                let handle = tokio::task::spawn_local(async move {
                    mam_clone.handle_event(&own_presence).await;
                });

                // Page 1: incomplete, returns 2 messages (one is a dup)
                let q1_event = timeout(TIMEOUT, ui_sub.recv())
                    .await
                    .expect("timed out waiting for MAM query 1")
                    .unwrap();
                let q1_id = match &q1_event.payload {
                    EventPayload::MamQueryRequested { query_id, .. } => query_id.clone(),
                    other => panic!("expected MamQueryRequested, got {other:?}"),
                };

                bus.publish(Event::new(
                    Channel::new("xmpp.mam.result.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamResultReceived {
                        query_id: q1_id.clone(),
                        messages: vec![
                            make_chat_message(
                                "msg-1",
                                "carol@example.com",
                                "alice@example.com",
                                "First archived",
                            ),
                            make_chat_message(
                                "msg-2",
                                "bob@example.com",
                                "alice@example.com",
                                "Dup msg",
                            ),
                        ],
                        complete: false,
                    },
                ))
                .unwrap();

                bus.publish(Event::new(
                    Channel::new("xmpp.mam.fin.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamFinReceived {
                        iq_id: q1_id,
                        complete: false,
                        last_id: Some("msg-2".to_string()),
                    },
                ))
                .unwrap();

                // Page 2: MAM requests next page
                let q2_event = timeout(TIMEOUT, ui_sub.recv())
                    .await
                    .expect("timed out waiting for MAM query 2")
                    .unwrap();
                let q2_id = match &q2_event.payload {
                    EventPayload::MamQueryRequested { query_id, .. } => query_id.clone(),
                    other => panic!("expected MamQueryRequested page 2, got {other:?}"),
                };

                bus.publish(Event::new(
                    Channel::new("xmpp.mam.result.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamResultReceived {
                        query_id: q2_id.clone(),
                        messages: vec![make_chat_message(
                            "msg-3",
                            "dave@example.com",
                            "alice@example.com",
                            "Third archived",
                        )],
                        complete: false,
                    },
                ))
                .unwrap();

                bus.publish(Event::new(
                    Channel::new("xmpp.mam.fin.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamFinReceived {
                        iq_id: q2_id,
                        complete: true,
                        last_id: Some("msg-3".to_string()),
                    },
                ))
                .unwrap();

                timeout(Duration::from_secs(5), handle)
                    .await
                    .expect("MAM sync timed out")
                    .expect("MAM sync panicked");

                // Verify: 3 unique messages stored (msg-1, msg-2 deduped, msg-3)
                let rows: Vec<Row> = db
                    .query("SELECT COUNT(*) FROM messages", &[])
                    .await
                    .unwrap();
                let count = match rows[0].get(0) {
                    Some(SqlValue::Integer(n)) => *n,
                    other => panic!("unexpected count value: {other:?}"),
                };
                // msg-2 was pre-stored; MAM stores msg-1 and msg-3; msg-2 deduped
                assert!(count >= 2, "expected at least 2 messages, got {count}");

                // Verify sync state points to last page's last ID
                let state: Vec<Row> = db
                    .query(
                        "SELECT last_stanza_id FROM mam_sync_state WHERE jid = '__global__'",
                        &[],
                    )
                    .await
                    .unwrap();
                assert_eq!(state[0].get(0), Some(&SqlValue::Text("msg-3".to_string())));
            })
            .await;
    }

    // ── 11. Offline Queue with Failed Sends ──────────────────────
    // Messages that fail to send should remain in the queue with
    // appropriate status, not get auto-confirmed

    #[tokio::test]
    async fn offline_queue_tracks_multiple_reconnect_cycles() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

        // Cycle 1: Send while offline
        let msg1 = messaging
            .send_message("bob@example.com", "cycle-1-msg")
            .await
            .unwrap();

        // Connect → drain
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;

        // Confirm first message via delivery
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &msg1.id,
                        "alice@example.com",
                        "bob@example.com",
                        "cycle-1-msg",
                    ),
                },
            ))
            .await;
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.message.delivered",
                EventPayload::MessageDelivered {
                    id: msg1.id.clone(),
                    to: "bob@example.com".to_string(),
                },
            ))
            .await;

        // Disconnect again
        let lost = make_event(
            "system.connection.lost",
            EventPayload::ConnectionLost {
                reason: "timeout".to_string(),
                will_retry: true,
            },
        );
        messaging.handle_event(&lost).await;

        // Cycle 2: Send while offline again
        let msg2 = messaging
            .send_message("carol@example.com", "cycle-2-msg")
            .await
            .unwrap();

        // Reconnect again
        let mut ui_sub = bus.subscribe("ui.message.send").unwrap();
        messaging.handle_event(&connected).await;

        // Second queue item should be drained
        let event = timeout(TIMEOUT, ui_sub.recv())
            .await
            .expect("timed out waiting for cycle-2 drain")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::MessageSendRequested { ref body, .. } if body == "cycle-2-msg"
        ));

        // Verify queue state: first confirmed, second sent (drained but not yet confirmed)
        let rows: Vec<Row> = db
            .query("SELECT status FROM offline_queue ORDER BY id ASC", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].get(0),
            Some(&SqlValue::Text("confirmed".to_string()))
        );
        // Second is pending or sent depending on drain behavior
        let second_status = rows[1].get(0);
        assert!(
            second_status == Some(&SqlValue::Text("pending".to_string()))
                || second_status == Some(&SqlValue::Text("sent".to_string())),
            "expected pending or sent, got {second_status:?}"
        );

        // Confirm second via delivery
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &msg2.id,
                        "alice@example.com",
                        "carol@example.com",
                        "cycle-2-msg",
                    ),
                },
            ))
            .await;
        messaging
            .handle_event(&make_xmpp_event(
                "xmpp.message.delivered",
                EventPayload::MessageDelivered {
                    id: msg2.id.clone(),
                    to: "carol@example.com".to_string(),
                },
            ))
            .await;

        let rows: Vec<Row> = db
            .query("SELECT status FROM offline_queue ORDER BY id ASC", &[])
            .await
            .unwrap();
        assert_eq!(
            rows[1].get(0),
            Some(&SqlValue::Text("confirmed".to_string()))
        );
    }

    // ── 12. Roster Replace Semantics ─────────────────────────────
    // A fresh RosterReceived replaces the entire roster, not merges

    #[tokio::test]
    async fn roster_received_replaces_entire_roster() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));

        // First roster: bob and carol
        let first = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived {
                items: vec![
                    RosterItem {
                        jid: "bob@example.com".to_string(),
                        name: Some("Bob".to_string()),
                        subscription: Subscription::Both,
                        groups: vec![],
                    },
                    RosterItem {
                        jid: "carol@example.com".to_string(),
                        name: Some("Carol".to_string()),
                        subscription: Subscription::To,
                        groups: vec![],
                    },
                ],
            },
        );
        roster.handle_event(&first).await;
        assert_eq!(roster.get_roster().await.unwrap().len(), 2);

        // Second roster: only dave (bob and carol should be gone)
        let second = make_xmpp_event(
            "xmpp.roster.received",
            EventPayload::RosterReceived {
                items: vec![RosterItem {
                    jid: "dave@example.com".to_string(),
                    name: Some("Dave".to_string()),
                    subscription: Subscription::Both,
                    groups: vec!["Work".to_string()],
                }],
            },
        );
        roster.handle_event(&second).await;

        let stored = roster.get_roster().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].jid, "dave@example.com");
    }

    // ── 13. MAM History Fetch (per-conversation) ─────────────────
    // Verify on-demand history fetch for a specific JID

    #[tokio::test]
    async fn mam_fetch_history_for_specific_jid() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = TempDir::new().unwrap();
                let db = setup_db(&dir).await;
                let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

                let mam = Arc::new(MamManager::new(db.clone(), bus.clone()));

                let mut ui_sub = bus.subscribe("ui.**").unwrap();

                // Connection required for MAM
                let connected = make_event(
                    "system.connection.established",
                    EventPayload::ConnectionEstablished {
                        jid: "alice@example.com".to_string(),
                    },
                );
                mam.handle_event(&connected).await;

                let mam_clone = mam.clone();
                let handle = tokio::task::spawn_local(async move {
                    mam_clone
                        .fetch_history("bob@example.com", None, 10)
                        .await
                        .unwrap()
                });

                let query_event = timeout(TIMEOUT, ui_sub.recv())
                    .await
                    .expect("timed out waiting for history query")
                    .unwrap();

                let query_id = match &query_event.payload {
                    EventPayload::MamQueryRequested {
                        query_id,
                        with_jid,
                        max,
                        ..
                    } => {
                        assert_eq!(with_jid.as_deref(), Some("bob@example.com"));
                        assert_eq!(*max, 10);
                        query_id.clone()
                    }
                    other => panic!("expected MamQueryRequested, got {other:?}"),
                };

                // Respond with results
                bus.publish(Event::new(
                    Channel::new("xmpp.mam.result.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamResultReceived {
                        query_id: query_id.clone(),
                        messages: vec![
                            make_chat_message(
                                "hist-1",
                                "bob@example.com",
                                "alice@example.com",
                                "Old message 1",
                            ),
                            make_chat_message(
                                "hist-2",
                                "alice@example.com",
                                "bob@example.com",
                                "Old message 2",
                            ),
                        ],
                        complete: false,
                    },
                ))
                .unwrap();

                bus.publish(Event::new(
                    Channel::new("xmpp.mam.fin.received").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MamFinReceived {
                        iq_id: query_id,
                        complete: true,
                        last_id: Some("hist-2".to_string()),
                    },
                ))
                .unwrap();

                let result = timeout(Duration::from_secs(5), handle)
                    .await
                    .expect("fetch timed out")
                    .expect("fetch panicked");

                assert_eq!(result.len(), 2);
            })
            .await;
    }

    // ── 14. MUC Leave Clears Room State ──────────────────────────
    // Verify that leaving a room properly cleans occupants and
    // joining a different room is independent

    #[tokio::test]
    async fn muc_multiple_rooms_independent_state() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let muc = Arc::new(MucManager::new(db.clone(), bus.clone()));

        // Join two rooms
        muc.join_room("room1@conference.example.com", "Alice")
            .await
            .unwrap();
        muc.join_room("room2@conference.example.com", "Alice")
            .await
            .unwrap();

        let joined1 = make_xmpp_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "room1@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        let joined2 = make_xmpp_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "room2@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        muc.handle_event(&joined1).await;
        muc.handle_event(&joined2).await;

        // Add occupant to room1
        let occupant = make_xmpp_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room1@conference.example.com".to_string(),
                occupant: MucOccupant {
                    nick: "Bob".to_string(),
                    jid: Some("bob@example.com".to_string()),
                    affiliation: MucAffiliation::Member,
                    role: MucRole::Participant,
                },
            },
        );
        muc.handle_event(&occupant).await;

        assert_eq!(muc.get_occupants("room1@conference.example.com").len(), 1);
        assert_eq!(muc.get_occupants("room2@conference.example.com").len(), 0);

        // Leave room1 — should not affect room2
        let left = make_xmpp_event(
            "xmpp.muc.left",
            EventPayload::MucLeft {
                room: "room1@conference.example.com".to_string(),
            },
        );
        muc.handle_event(&left).await;

        let rooms = muc.get_joined_rooms().await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room_jid, "room2@conference.example.com");
        assert!(rooms[0].joined);

        // Room1 occupants should be cleared
        assert!(muc.get_occupants("room1@conference.example.com").is_empty());
    }

    // ── 15. Connection Reconnecting Event ────────────────────────
    // Verify ConnectionReconnecting is handled without panics

    #[tokio::test]
    async fn connection_reconnecting_handled_gracefully() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));
        let presence = Arc::new(PresenceManager::new(bus.clone()));

        // Establish
        let connected = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connected).await;
        presence.handle_event(&connected).await;

        // Lose connection
        let lost = make_event(
            "system.connection.lost",
            EventPayload::ConnectionLost {
                reason: "timeout".to_string(),
                will_retry: true,
            },
        );
        messaging.handle_event(&lost).await;
        presence.handle_event(&lost).await;

        // Reconnecting attempts
        for attempt in 1..=3 {
            let reconnecting = make_event(
                "system.connection.reconnecting",
                EventPayload::ConnectionReconnecting { attempt },
            );
            messaging.handle_event(&reconnecting).await;
            presence.handle_event(&reconnecting).await;
        }

        // Messages should still queue offline
        let msg = messaging
            .send_message("bob@example.com", "during reconnect")
            .await
            .unwrap();
        assert!(!msg.id.is_empty());

        // Finally reconnect
        messaging.handle_event(&connected).await;
        presence.handle_event(&connected).await;

        // The drain happened during handle_event above, so check the queue
        let rows: Vec<Row> = db
            .query(
                "SELECT status FROM offline_queue ORDER BY id DESC LIMIT 1",
                &[],
            )
            .await
            .unwrap();
        assert!(!rows.is_empty());
    }

    // ── 16. Error Propagation ────────────────────────────────────
    // Verify ErrorOccurred events are handled without panics

    #[tokio::test]
    async fn error_events_handled_without_panic() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));
        let roster = Arc::new(RosterManager::new(db.clone(), bus.clone()));
        let presence = Arc::new(PresenceManager::new(bus.clone()));
        let mam = Arc::new(MamManager::new(db.clone(), bus.clone()));

        let error = make_event(
            "system.error.occurred",
            EventPayload::ErrorOccurred {
                component: "xmpp".to_string(),
                message: "TLS handshake failed".to_string(),
                recoverable: true,
            },
        );

        // All managers should handle error events without panicking
        messaging.handle_event(&error).await;
        roster.handle_event(&error).await;
        presence.handle_event(&error).await;
        mam.handle_event(&error).await;
    }
}
