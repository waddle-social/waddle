//! The session's verbs for typing, read markers and unsent-queue checks.
//!
//! Like `session.rs` this only adapts the `WaddleClient` facade; the rules
//! live in `typing.rs`, `read_sync.rs` and `delivery.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use waddle_xmpp_client_ffi::WaddleMessageAudience;

use crate::read_sync::Cursor;
use crate::runtime;
use crate::session::{Kind, Session};
use crate::typing;

fn audience(kind: Kind) -> WaddleMessageAudience {
    match kind {
        Kind::Room => WaddleMessageAudience::Muc,
        Kind::Direct => WaddleMessageAudience::Direct,
    }
}

impl Session {
    /// XEP-0085: tell a conversation what the user is doing. Fire and
    /// forget; a state that is lost is replaced by the next one.
    pub fn send_chat_state(&self, jid: &str, kind: Kind, state: typing::State) {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        runtime::spawn(async move {
            client
                .send_chat_state(peer, state.wire(), audience(kind))
                .await
        });
    }

    /// XEP-0333: mark `id` displayed.
    pub async fn send_displayed(&self, jid: &str, kind: Kind, id: String) -> bool {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        runtime::spawn(async move { client.send_displayed(peer, id, audience(kind)).await })
            .await
            .unwrap_or(false)
    }

    /// XEP-0490: publish our cursor for a chat.
    pub async fn publish_cursor(&self, cursor: Cursor) -> bool {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move {
            client
                .publish_mds_displayed(cursor.conversation, cursor.stanza_id, cursor.by)
                .await
        })
        .await
        .unwrap_or(false)
    }

    /// XEP-0490 §3: whether the server takes publish options, which a
    /// cursor needs.
    pub async fn supports_cursor_publish(&self) -> bool {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.supports_mds_publish_options().await })
            .await
            .unwrap_or(false)
    }

    /// The account's cursors as stored on the server.
    pub async fn cursors(&self) -> Vec<Cursor> {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { client.fetch_mds_displayed().await }).await {
            Ok(Ok(entries)) => entries.into_iter().map(Cursor::from).collect(),
            Ok(Err(error)) => {
                tracing::warn!(?error, "could not fetch read cursors");
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }

    /// XEP-0490: be told when another device moves a cursor.
    pub fn subscribe_cursors(&self) {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.subscribe_mds_displayed().await });
    }

    /// The origin ids on the newest archive page of a conversation. A
    /// message of ours that is already there reached the server, so
    /// sending it again could show twice.
    pub async fn recent_origin_ids(&self, jid: &str, kind: Kind) -> Option<HashSet<String>> {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        let page = runtime::spawn(async move {
            match kind {
                Kind::Room => client.fetch_room_history(peer, 50, None).await,
                Kind::Direct => client.fetch_dm_history(peer, 50, None).await,
            }
        })
        .await
        .ok()?;
        Some(
            page.messages
                .into_iter()
                .filter_map(|message| message.origin_id)
                .collect(),
        )
    }
}
