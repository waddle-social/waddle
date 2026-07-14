use std::sync::{Arc, Mutex as StdMutex};

use crate::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleEncryptedFile, WaddleEventListener,
    WaddleSendMessageOutcome, WaddleSendOptions, WaddleSharedFile,
};

#[derive(Clone, Default)]
struct RecordingListener {
    errors: Arc<StdMutex<Vec<String>>>,
}

impl RecordingListener {
    fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }
}

impl WaddleEventListener for RecordingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        if let WaddleClientEvent::Error { description } = event {
            self.errors.lock().unwrap().push(description);
        }
    }
}

fn test_client(listener: RecordingListener) -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(listener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
    })
}

fn invalid_send_options() -> WaddleSendOptions {
    WaddleSendOptions {
        shared_files: vec![WaddleSharedFile {
            url: "https://cdn.waddle.test/file.enc".to_string(),
            name: None,
            media_type: None,
            size: None,
            width: None,
            height: None,
            disposition: "attachment".to_string(),
            encrypted: Some(WaddleEncryptedFile {
                cipher: "urn:waddle:not-a-cipher".to_string(),
                key_b64: "key".to_string(),
                iv_b64: "iv".to_string(),
                hashes: Vec::new(),
                sources: vec!["https://cdn.waddle.test/file.enc".to_string()],
            }),
        }],
        ..WaddleSendOptions::default()
    }
}

#[tokio::test]
async fn send_chat_message_reports_not_connected_as_typed_outcome() {
    let listener = Arc::new(RecordingListener::default());
    let client = test_client((*listener).clone());

    let outcome = client
        .send_chat_message("bob@waddle.test".to_string(), "hello".to_string(), None)
        .await;

    assert_eq!(outcome, WaddleSendMessageOutcome::NotConnected);
    assert_eq!(listener.errors(), vec!["Not connected"]);
}

#[tokio::test]
async fn send_groupchat_message_reports_invalid_options_as_typed_outcome() {
    let listener = Arc::new(RecordingListener::default());
    let client = test_client((*listener).clone());

    let outcome = client
        .send_groupchat_message(
            "room@muc.waddle.test".to_string(),
            "hello".to_string(),
            Some(invalid_send_options()),
        )
        .await;

    assert_eq!(outcome, WaddleSendMessageOutcome::InvalidOptions);
    assert!(
        listener
            .errors()
            .first()
            .is_some_and(|error| error.contains("unknown cipher")),
        "expected invalid cipher diagnostic"
    );
}

#[tokio::test]
async fn send_chat_message_reports_invalid_options_as_typed_outcome() {
    let listener = Arc::new(RecordingListener::default());
    let client = test_client((*listener).clone());

    let outcome = client
        .send_chat_message(
            "bob@waddle.test".to_string(),
            "hello".to_string(),
            Some(invalid_send_options()),
        )
        .await;

    assert_eq!(outcome, WaddleSendMessageOutcome::InvalidOptions);
    assert!(
        listener
            .errors()
            .first()
            .is_some_and(|error| error.contains("unknown cipher")),
        "expected invalid cipher diagnostic"
    );
}

#[tokio::test]
async fn send_chat_message_reports_invalid_recipient_as_typed_outcome() {
    let listener = Arc::new(RecordingListener::default());
    let client = test_client((*listener).clone());

    let outcome = client
        .send_chat_message("not a jid".to_string(), "hello".to_string(), None)
        .await;

    assert_eq!(outcome, WaddleSendMessageOutcome::InvalidRecipient);
    assert!(
        listener
            .errors()
            .first()
            .is_some_and(|error| error.contains("invalid peer JID")),
        "expected invalid peer JID diagnostic"
    );
}

#[tokio::test]
async fn send_groupchat_message_reports_invalid_recipient_as_typed_outcome() {
    let listener = Arc::new(RecordingListener::default());
    let client = test_client((*listener).clone());

    let outcome = client
        .send_groupchat_message("not a room".to_string(), "hello".to_string(), None)
        .await;

    assert_eq!(outcome, WaddleSendMessageOutcome::InvalidRecipient);
    assert!(
        listener
            .errors()
            .first()
            .is_some_and(|error| error.contains("invalid room JID")),
        "expected invalid room JID diagnostic"
    );
}
