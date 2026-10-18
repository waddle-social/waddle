//! One signed-in account's XMPP session.
//!
//! The protocol work is the same `WaddleClient` facade the Android and
//! Apple apps call through UniFFI; Linux links it as Rust. Its events are
//! forwarded to the GTK main loop over a channel. Conversations are kept in
//! `waddle-client-session` stores (ADR-0019), so this module only adapts
//! records; it keeps no client state of its own.
//!
//! Reconnection here is a stopgap with the ADR-0019 backoff curve. The
//! shared `ConnectionController` replaces it.

use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use waddle_client_session::{HistoryMessage, LiveMessage};
use waddle_xmpp_client_ffi::{
    WaddleArchivedMessage, WaddleChannel, WaddleClient, WaddleClientEvent, WaddleConfig,
    WaddleEventListener, WaddleMessage, WaddleSendMessageOutcome, WaddleSendOptions,
    WaddleTopology,
};

use crate::auth::store::StoredAccount;
use crate::runtime;

/// A room in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub jid: String,
    pub name: String,
    pub autojoin: bool,
}

/// What the UI hears from the session.
pub enum SessionEvent {
    Connected,
    Disconnected,
    AuthenticationFailed,
    Message(Box<WaddleMessage>),
    DeliveryAcked(String),
    DeliveryFailed(String),
}

struct Forwarder(async_channel::Sender<SessionEvent>);

impl WaddleEventListener for Forwarder {
    fn on_event(&self, event: WaddleClientEvent) {
        let forwarded = match event {
            WaddleClientEvent::Connected => SessionEvent::Connected,
            WaddleClientEvent::Disconnected => SessionEvent::Disconnected,
            WaddleClientEvent::AuthenticationFailed { .. } => SessionEvent::AuthenticationFailed,
            WaddleClientEvent::Message { message } => SessionEvent::Message(Box::new(message)),
            WaddleClientEvent::DeliveryAcked { stanza_id } => {
                SessionEvent::DeliveryAcked(stanza_id)
            }
            WaddleClientEvent::DeliveryFailed { stanza_id } => {
                SessionEvent::DeliveryFailed(stanza_id)
            }
            WaddleClientEvent::Error { description } => {
                tracing::warn!(%description, "xmpp client error");
                return;
            }
            // History arrives through fetched pages only (ADR-0019), and the
            // rest has no Linux surface yet.
            _ => return,
        };
        // The receiver lives as long as the window; a send after it closes
        // has nobody to tell.
        let _ = self.0.send_blocking(forwarded);
    }
}

#[derive(Clone)]
pub struct Session {
    client: Arc<WaddleClient>,
    pub nick: String,
}

impl Session {
    /// Start a session and return it with its event stream.
    pub fn start(account: &StoredAccount) -> (Self, async_channel::Receiver<SessionEvent>) {
        let (sender, receiver) = async_channel::unbounded();
        let config = WaddleConfig {
            server_url: account.session.websocket_url.to_string(),
            jid: account.session.jid.to_string(),
            access_token: account.session.session_id.expose().to_string(),
            resource: account.resource.clone(),
            // This client never resumes, so it must not ask the server to
            // park the session (that suppresses push; see #1987).
            resumable: false,
        };
        let client = WaddleClient::new(config, Box::new(Forwarder(sender)));
        let session = Self {
            client,
            nick: account.session.username.clone(),
        };
        session.connect();
        (session, receiver)
    }

    pub fn connect(&self) {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.connect().await });
    }

    pub async fn disconnect(&self) {
        let client = Arc::clone(&self.client);
        let _ = runtime::spawn(async move { client.disconnect().await }).await;
    }

    pub async fn rooms(&self) -> Vec<Room> {
        let client = Arc::clone(&self.client);
        let topology = runtime::spawn(async move { client.discover_topology().await }).await;
        topology.map(rooms_from).unwrap_or_default()
    }

    pub fn join(&self, room: &Room) {
        let client = Arc::clone(&self.client);
        let (jid, nick) = (room.jid.clone(), self.nick.clone());
        runtime::spawn(async move { client.join_room(jid, nick).await });
    }

    /// The newest page before `before` (or the newest page), oldest first.
    pub async fn history(
        &self,
        room: &str,
        before: Option<String>,
    ) -> (Vec<HistoryMessage>, Option<String>, bool) {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        let page =
            runtime::spawn(async move { client.fetch_room_history(room, 50, before).await }).await;
        let Ok(page) = page else {
            return (Vec::new(), None, false);
        };
        let messages = page
            .messages
            .into_iter()
            .map(|message| history_from(message, &self.nick))
            .collect();
        (messages, page.first_id, page.is_complete)
    }

    /// Send to a room under `origin_id`. True when the stanza was written.
    pub async fn send(&self, room: &str, body: String, origin_id: String) -> bool {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        let options = WaddleSendOptions {
            stanza_id: Some(origin_id),
            ..WaddleSendOptions::default()
        };
        let outcome = runtime::spawn(async move {
            client
                .send_groupchat_message(room, body, Some(options))
                .await
        })
        .await;
        matches!(outcome, Ok(WaddleSendMessageOutcome::Sent { .. }))
    }
}

/// Reconnect delay for the `attempt`th retry: uniform in
/// [1 s, min(60 s, 2ⁿ s)] (ADR-0019), from the caller's randomness.
pub fn reconnect_delay(attempt: u32, unit_random: f64) -> Duration {
    let ceiling = 2f64
        .powi(i32::try_from(attempt.min(6)).unwrap_or(6))
        .min(60.0);
    let seconds = 1.0 + (ceiling - 1.0).max(0.0) * unit_random.clamp(0.0, 1.0);
    Duration::from_secs_f64(seconds)
}

/// Rooms for the sidebar: channels, not group DMs, in catalogue order.
pub fn rooms_from(topology: WaddleTopology) -> Vec<Room> {
    let mut channels: Vec<WaddleChannel> = topology
        .channels
        .into_iter()
        .filter(|channel| !channel.is_group_dm)
        .collect();
    channels.sort_by(|a, b| {
        (a.space_id.as_str(), a.position, a.name.as_str()).cmp(&(
            b.space_id.as_str(),
            b.position,
            b.name.as_str(),
        ))
    });
    channels
        .into_iter()
        .map(|channel| Room {
            name: channel.bookmark_name.unwrap_or(channel.name),
            jid: channel.room_jid,
            autojoin: channel.autojoin,
        })
        .collect()
}

/// The room a groupchat message belongs to, and the core's view of it.
pub fn live_from(message: WaddleMessage, own_nick: &str) -> Option<(String, LiveMessage)> {
    if message.message_type != "groupchat" {
        return None;
    }
    let from = message.from?;
    let (room, nick) = split_occupant(&from)?;
    Some((
        room.to_string(),
        LiveMessage {
            stanza_id: message.stanza_id,
            origin_id: message.origin_id,
            own: nick == own_nick,
            sender: nick.to_string(),
            body: message.body,
            timestamp_ms: timestamp_ms(message.timestamp.as_deref()),
            replaces_id: message.replaces_id,
            retracts_id: message.retracts_id,
        },
    ))
}

pub fn history_from(message: WaddleArchivedMessage, own_nick: &str) -> HistoryMessage {
    let nick = message
        .from
        .as_deref()
        .and_then(split_occupant)
        .map(|(_, nick)| nick.to_string())
        .unwrap_or_default();
    HistoryMessage {
        stanza_id: message.stanza_id.or(Some(message.mam_id)),
        origin_id: message.origin_id,
        own: nick == own_nick,
        sender: nick,
        body: message.body,
        timestamp_ms: timestamp_ms(message.timestamp.as_deref()),
        replaces_id: message.replaces_id,
        retracted: message.is_retracted,
    }
}

/// `room@muc.example/nick` → (`room@muc.example`, `nick`).
fn split_occupant(full: &str) -> Option<(&str, &str)> {
    let (room, nick) = full.split_once('/')?;
    (!room.is_empty() && !nick.is_empty()).then_some((room, nick))
}

fn timestamp_ms(raw: Option<&str>) -> i64 {
    raw.and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|at| at.timestamp_millis())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str, position: i32, group_dm: bool) -> WaddleChannel {
        WaddleChannel {
            id: name.into(),
            room_jid: format!("{name}@muc.waddle.social"),
            name: name.into(),
            description: None,
            channel_type: "text".into(),
            position,
            space_id: "default".into(),
            autojoin: true,
            bookmark_name: None,
            is_group_dm: group_dm,
        }
    }

    #[test]
    fn rooms_are_channels_in_catalogue_order() {
        let rooms = rooms_from(WaddleTopology {
            spaces: Vec::new(),
            channels: vec![
                channel("random", 2, false),
                channel("dm-ann-bob", 0, true),
                channel("general", 1, false),
            ],
        });
        let names: Vec<&str> = rooms.iter().map(|room| room.name.as_str()).collect();
        assert_eq!(names, ["general", "random"]);
        assert_eq!(rooms[0].jid, "general@muc.waddle.social");
    }

    #[test]
    fn occupant_jids_split_into_room_and_nick() {
        assert_eq!(
            split_occupant("general@muc.waddle.social/david"),
            Some(("general@muc.waddle.social", "david"))
        );
        assert_eq!(split_occupant("general@muc.waddle.social"), None);
        assert_eq!(split_occupant("general@muc.waddle.social/"), None);
    }

    #[test]
    fn timestamps_parse_rfc3339_to_utc_millis() {
        assert_eq!(timestamp_ms(Some("1970-01-01T00:00:01+00:00")), 1000);
        assert_eq!(timestamp_ms(Some("1970-01-01T01:00:01+01:00")), 1000);
    }

    #[test]
    fn reconnect_backoff_follows_the_adr_curve() {
        assert_eq!(reconnect_delay(0, 0.9), Duration::from_secs(1));
        assert_eq!(reconnect_delay(3, 0.0), Duration::from_secs(1));
        assert_eq!(reconnect_delay(3, 1.0), Duration::from_secs(8));
        assert_eq!(reconnect_delay(40, 1.0), Duration::from_secs(60));
    }
}
