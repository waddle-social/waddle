#[cfg(all(test, feature = "native"))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use waddle_core::event::{
        BroadcastEventBus, Channel, ChatMessage, Event, EventBus, EventPayload, EventSource,
        MessageType, MessageEmbed,
    };
    use serde_json::json;
    use waddle_messaging::MessageManager;
    use waddle_storage::Database;

    const TIMEOUT: Duration = Duration::from_millis(500);

    async fn setup_db(dir: &TempDir) -> Arc<impl Database + use<>> {
        let db_path = dir.path().join("test.db");
        let db = waddle_storage::open_database(&db_path)
            .await
            .expect("failed to open database");
        Arc::new(db)
    }

    fn make_chat_message(id: &str, from: &str, to: &str, body: &str) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            body: body.to_string(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Chat,
            thread: None,
            embeds: vec![],
        }
    }

    // ── GitHub URL Enrichment Integration Test ──────────────────────
    //
    // This test verifies that:
    // 1. A message with a GitHub URL is detected.
    // 2. The enrichment process (which runs via the XmppStream/Enricher
    //    before hitting the MessageManager) correctly adds embeds.
    //
    // Note: Since the enrichment logic resides in `waddle-xmpp-xep-github`
    // and modifies the message *before* it hits the bus as MessageReceived,
    // we can't easily test the full network -> enrich -> bus pipeline here
    // without mocking the XmppStream.
    //
    // However, we CAN verify that if a message arrives with GitHub embeds
    // (simulating successful enrichment), the MessageManager persists them correctly.

    #[tokio::test]
    async fn message_with_github_embeds_is_persisted() {
        let dir = TempDir::new().unwrap();
        let db = setup_db(&dir).await;
        let bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());

        let messaging = Arc::new(MessageManager::new(db.clone(), bus.clone()));

        // Simulate a message that has been enriched with a GitHub repo
        // This simulates the result of the `waddle-xmpp-xep-github` pipeline
        let mut msg = make_chat_message(
            "gh-1",
            "bob@example.com",
            "alice@example.com",
            "Check out https://github.com/rust-lang/rust",
        );
        
        // We'll manually construct the JSON embed that represents the GitHub data
        // In a real flow, this comes from the parser/enricher. Here we just need to ensure
        // it round-trips through the database.
        
        let embed = MessageEmbed {
            namespace: "urn:xmpp:waddle:github:0".to_string(),
            data: json!({
                "owner": "rust-lang",
                "repo": "rust",
                "url": "https://github.com/rust-lang/rust",
                "description": "Rust Programming Language"
            }),
        };
            
        msg.embeds.push(embed);


        // Inject the connection event FIRST
        let connect_event = Event::new(
            Channel::new("system.connection.established").unwrap(),
            EventSource::System("test".into()),
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        messaging.handle_event(&connect_event).await;
        
        // Inject the message
        let event = Event::new(
            Channel::new("xmpp.message.received").unwrap(),
            EventSource::Xmpp,
            EventPayload::MessageReceived { message: msg.clone() },
        );
        messaging.handle_event(&event).await;

        // Verify persistence
        let stored = messaging
            .get_messages("bob@example.com", 50, None)
            .await
            .unwrap();
        
        assert_eq!(stored.len(), 1);
        let stored_msg = &stored[0];
        assert_eq!(stored_msg.body, "Check out https://github.com/rust-lang/rust");
        assert_eq!(stored_msg.embeds.len(), 1);
        
        let stored_embed = &stored_msg.embeds[0];
        assert_eq!(stored_embed.namespace, "urn:xmpp:waddle:github:0");
        
        let data = &stored_embed.data;
        assert_eq!(data["owner"], "rust-lang");
        assert_eq!(data["repo"], "rust");
    }
}
