//! FFI test suite for the group-DM verb exports: input validation
//! and offline failure behavior. The request/response wire shapes are
//! pinned by the shared core suite
//! (`waddle_xmpp_client::group_dm::tests`).

use std::sync::Arc;

use crate::{WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener};

struct NullListener;

impl WaddleEventListener for NullListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

fn offline_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(NullListener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}

#[tokio::test]
async fn group_dm_verbs_require_connection() {
    let client = offline_client();
    assert_eq!(
        client
            .create_group_dm(
                "Alice, Bob".to_string(),
                vec![
                    "alice@waddle.test".to_string(),
                    "bob@waddle.test".to_string(),
                ],
            )
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
    assert_eq!(
        client
            .rename_group_dm(
                "gdm-abc@muc.waddle.test".to_string(),
                Some("Weekend crew".to_string()),
            )
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
    assert_eq!(
        client
            .leave_group_dm("gdm-abc@muc.waddle.test".to_string())
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
    assert_eq!(
        client
            .invite_to_group_dm(
                "gdm-abc@muc.waddle.test".to_string(),
                "charlie@waddle.test".to_string(),
                false,
            )
            .await
            .unwrap_err(),
        WaddleError::NotConnected
    );
}

#[tokio::test]
async fn group_dm_verbs_reject_malformed_jids_before_sending() {
    let client = offline_client();
    assert_eq!(
        client
            .create_group_dm(
                "trio".to_string(),
                vec!["alice@waddle.test".to_string(), "not a jid".to_string()],
            )
            .await
            .unwrap_err(),
        WaddleError::InvalidJid
    );
    assert_eq!(
        client
            .rename_group_dm("not a jid".to_string(), None)
            .await
            .unwrap_err(),
        WaddleError::InvalidJid
    );
    assert_eq!(
        client
            .leave_group_dm("not a jid".to_string())
            .await
            .unwrap_err(),
        WaddleError::InvalidJid
    );
    assert_eq!(
        client
            .invite_to_group_dm(
                "gdm-abc@muc.waddle.test".to_string(),
                "not a jid".to_string(),
                true,
            )
            .await
            .unwrap_err(),
        WaddleError::InvalidJid
    );
}
