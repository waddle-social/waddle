use std::sync::Mutex;

use jid::Jid;
use waddle_xmpp_client::blocking::BlockingPush;
use waddle_xmpp_client::ClientEvent;

use crate::convert::dispatch_event;
use crate::test_support::offline_client;
use crate::{WaddleBlockListChange, WaddleClientEvent, WaddleError, WaddleEventListener};

#[tokio::test]
async fn targeted_blocking_verbs_reject_empty_jid_lists() {
    let client = offline_client();
    assert_eq!(
        client.block_jids(vec![]).await,
        Err(WaddleError::InvalidArgument)
    );
    assert_eq!(
        client.unblock_jids(vec![]).await,
        Err(WaddleError::InvalidArgument)
    );
}

#[tokio::test]
async fn targeted_blocking_verbs_reject_invalid_jids() {
    let client = offline_client();
    let mixed = vec!["bob@example.net/phone".to_string(), String::new()];
    assert_eq!(
        client.block_jids(mixed.clone()).await,
        Err(WaddleError::InvalidJid)
    );
    assert_eq!(
        client.unblock_jids(mixed).await,
        Err(WaddleError::InvalidJid)
    );
}

#[tokio::test]
async fn blocking_verbs_require_a_connection() {
    let client = offline_client();
    let jids = vec![
        "bob@example.net/phone".to_string(),
        "example.org/service".to_string(),
    ];
    assert_eq!(
        client.fetch_block_list().await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(
        client.block_jids(jids.clone()).await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(
        client.unblock_jids(jids).await,
        Err(WaddleError::NotConnected)
    );
    assert_eq!(client.unblock_all().await, Err(WaddleError::NotConnected));
}

#[derive(Default)]
struct BlockingListener(Mutex<Vec<WaddleBlockListChange>>);

impl WaddleEventListener for BlockingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        if let WaddleClientEvent::BlockListChanged { change } = event {
            self.0.lock().expect("test listener lock").push(change);
        }
    }
}

#[test]
fn blocking_pushes_reach_the_ffi_listener_as_typed_changes() {
    let listener = BlockingListener::default();
    let full = "bob@example.net/phone".parse::<Jid>().expect("full JID");
    let domain = "example.org/service"
        .parse::<Jid>()
        .expect("domain/resource JID");
    for change in [
        BlockingPush::Blocked(vec![full]),
        BlockingPush::Unblocked(vec![domain]),
        BlockingPush::UnblockedAll,
    ] {
        dispatch_event(
            ClientEvent::BlockListChanged(change),
            "alice@waddle.test",
            &listener,
        );
    }
    assert_eq!(
        listener.0.into_inner().expect("test listener lock"),
        vec![
            WaddleBlockListChange::Blocked {
                jids: vec!["bob@example.net/phone".to_string()],
            },
            WaddleBlockListChange::Unblocked {
                jids: vec!["example.org/service".to_string()],
            },
            WaddleBlockListChange::UnblockedAll,
        ]
    );
}
