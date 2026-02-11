use chrono::{DateTime, Utc};
#[cfg(feature = "native")]
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};
#[cfg(feature = "native")]
use tokio::sync::broadcast;
use uuid::Uuid;

/// Hierarchical channel name validation and parsing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Channel(String);

impl Channel {
    /// Create a new channel, validating its format.
    pub fn new(name: impl Into<String>) -> std::result::Result<Self, crate::error::EventBusError> {
        let name = name.into();
        if Self::is_valid(&name) {
            Ok(Self(name))
        } else {
            Err(crate::error::EventBusError::InvalidChannel(name))
        }
    }

    /// Check if a channel name is valid.
    pub fn is_valid(name: &str) -> bool {
        if name.is_empty() || name.starts_with('.') || name.ends_with('.') || name.contains("..") {
            return false;
        }

        // Must be lowercase and only contain a-z, 0-9, and dots
        if name
            .chars()
            .any(|c| !matches!(c, 'a'..='z' | '0'..='9' | '.'))
        {
            return false;
        }

        let parts: Vec<&str> = name.split('.').collect();
        if parts.is_empty() {
            return false;
        }

        // Check domain
        match parts[0] {
            "system" | "xmpp" | "ui" | "plugin" => {}
            _ => return false,
        }

        true
    }

    /// Get the domain of the channel.
    pub fn domain(&self) -> &str {
        self.0.split('.').next().unwrap_or("")
    }

    /// Get the full channel name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Channel> for String {
    fn from(channel: Channel) -> Self {
        channel.0
    }
}

/// The standard event envelope wrapping all events in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Hierarchical channel name (e.g., "xmpp.message.received")
    pub channel: Channel,

    /// When the event was created (UTC)
    pub timestamp: DateTime<Utc>,

    /// Unique identifier for this event
    pub id: Uuid,

    /// Optional correlation ID linking related events (e.g., request-response)
    pub correlation_id: Option<Uuid>,

    /// Source component that emitted this event
    pub source: EventSource,

    /// The typed event payload
    pub payload: EventPayload,
}

impl Event {
    /// Create a new event with a given channel and payload.
    pub fn new(channel: Channel, source: EventSource, payload: EventPayload) -> Self {
        Self {
            channel,
            timestamp: Utc::now(),
            id: Uuid::new_v4(),
            correlation_id: None,
            source,
            payload,
        }
    }

    /// Create a new event with a correlation ID.
    pub fn with_correlation(
        channel: Channel,
        source: EventSource,
        payload: EventPayload,
        correlation_id: Uuid,
    ) -> Self {
        Self {
            channel,
            timestamp: Utc::now(),
            id: Uuid::new_v4(),
            correlation_id: Some(correlation_id),
            source,
            payload,
        }
    }
}

/// Identifies the source of an event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "camelCase")]
pub enum EventSource {
    /// Core system component
    System(String),
    /// XMPP subsystem
    Xmpp,
    /// User interface
    Ui(UiTarget),
    /// Plugin with its ID
    Plugin(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiTarget {
    Tui,
    Gui,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum EventPayload {
    // ── System events ──────────────────────────────────────────────
    StartupComplete,
    ShutdownRequested {
        reason: String,
    },
    ConnectionEstablished {
        jid: String,
    },
    ConnectionLost {
        reason: String,
        will_retry: bool,
    },
    ConnectionReconnecting {
        attempt: u32,
    },
    GoingOffline,
    ComingOnline,
    SyncStarted,
    SyncCompleted {
        messages_synced: u64,
    },
    ConfigReloaded,
    ErrorOccurred {
        component: String,
        message: String,
        recoverable: bool,
    },

    // ── XMPP Roster events ────────────────────────────────────────
    RosterReceived {
        items: Vec<RosterItem>,
    },
    RosterUpdated {
        item: RosterItem,
    },
    RosterRemoved {
        jid: String,
    },
    SubscriptionRequest {
        from: String,
    },
    SubscriptionApproved {
        jid: String,
    },
    SubscriptionRevoked {
        jid: String,
    },

    // ── XMPP Presence events ──────────────────────────────────────
    PresenceChanged {
        jid: String,
        show: PresenceShow,
        status: Option<String>,
    },
    OwnPresenceChanged {
        show: PresenceShow,
        status: Option<String>,
    },

    // ── XMPP Message events ──────────────────────────────────────
    MessageReceived {
        message: ChatMessage,
    },
    MessageSent {
        message: ChatMessage,
    },
    MessageDelivered {
        id: String,
        to: String,
    },
    ChatStateReceived {
        from: String,
        state: ChatState,
    },
    MucMessageReceived {
        room: String,
        message: ChatMessage,
    },
    MucJoined {
        room: String,
        nick: String,
    },
    MucLeft {
        room: String,
    },
    MucSubjectChanged {
        room: String,
        subject: String,
    },
    MucOccupantChanged {
        room: String,
        occupant: MucOccupant,
    },

    // ── XMPP MAM events ──────────────────────────────────────────
    MamResultReceived {
        query_id: String,
        messages: Vec<ChatMessage>,
        complete: bool,
    },

    // ── XMPP Debug events ────────────────────────────────────────
    RawStanzaReceived {
        stanza: String,
    },
    RawStanzaSent {
        stanza: String,
    },

    // ── UI events ────────────────────────────────────────────────
    ConversationOpened {
        jid: String,
    },
    ConversationClosed {
        jid: String,
    },
    ScrollRequested {
        jid: String,
        direction: ScrollDirection,
    },
    ComposeStarted {
        jid: String,
    },
    SearchRequested {
        query: String,
    },
    ThemeChanged {
        theme_id: String,
    },
    NotificationClicked {
        event_id: String,
    },

    // ── UI command events (consumed by XMPP outbound router) ────
    MessageSendRequested {
        to: String,
        body: String,
        message_type: MessageType,
    },
    PresenceSetRequested {
        show: PresenceShow,
        status: Option<String>,
    },
    RosterAddRequested {
        jid: String,
        name: Option<String>,
        groups: Vec<String>,
    },
    RosterRemoveRequested {
        jid: String,
    },
    SubscriptionRespondRequested {
        jid: String,
        accept: bool,
    },
    SubscriptionSendRequested {
        jid: String,
        subscribe: bool,
    },
    MucJoinRequested {
        room: String,
        nick: String,
    },
    MucLeaveRequested {
        room: String,
    },
    RosterUpdateRequested {
        jid: String,
        name: Option<String>,
        groups: Vec<String>,
    },
    RosterFetchRequested,
    MucSendRequested {
        room: String,
        body: String,
    },
    ChatStateSendRequested {
        to: String,
        state: ChatState,
    },

    // ── Plugin events ────────────────────────────────────────────
    PluginLoaded {
        plugin_id: String,
        version: String,
    },
    PluginUnloaded {
        plugin_id: String,
    },
    PluginError {
        plugin_id: String,
        error: String,
    },
    PluginCustomEvent {
        plugin_id: String,
        event_type: String,
        data: serde_json::Value,
    },
    PluginInstallStarted {
        plugin_id: String,
    },
    PluginInstallCompleted {
        plugin_id: String,
    },
}

/// A single entry in the XMPP roster.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterItem {
    /// The contact's bare JID (e.g., "alice@example.com")
    pub jid: String,

    /// Display name set by the user, if any
    pub name: Option<String>,

    /// Roster subscription state
    pub subscription: Subscription,

    /// User-defined groups this contact belongs to
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Subscription {
    None,
    To,
    From,
    Both,
    Remove,
}

impl Subscription {
    pub fn as_str(&self) -> &'static str {
        match self {
            Subscription::None => "none",
            Subscription::To => "to",
            Subscription::From => "from",
            Subscription::Both => "both",
            Subscription::Remove => "remove",
        }
    }
}

impl std::str::FromStr for Subscription {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "to" => Subscription::To,
            "from" => Subscription::From,
            "both" => Subscription::Both,
            "remove" => Subscription::Remove,
            _ => Subscription::None,
        })
    }
}

/// A chat message (1:1 or MUC).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    /// Server-assigned or client-generated unique message ID
    pub id: String,

    /// Bare JID of the sender
    pub from: String,

    /// Bare JID of the recipient (or room JID for MUC)
    pub to: String,

    /// Plain-text message body
    pub body: String,

    /// When the message was sent (UTC)
    pub timestamp: DateTime<Utc>,

    /// Message type
    pub message_type: MessageType,

    /// Thread ID for conversation threading, if present
    pub thread: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageType {
    Chat,
    Groupchat,
    Normal,
    Headline,
    Error,
}

/// XMPP presence "show" values (RFC 6121 section 4.7.2.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PresenceShow {
    /// Available (no <show/> element -- the default)
    Available,
    /// Free for chat
    Chat,
    /// Away
    Away,
    /// Extended away
    Xa,
    /// Do not disturb
    Dnd,
    /// Unavailable (offline)
    Unavailable,
}

/// XEP-0085 Chat State Notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatState {
    Active,
    Composing,
    Paused,
    Inactive,
    Gone,
}

/// An occupant in a MUC room.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MucOccupant {
    /// The occupant's room nick
    pub nick: String,

    /// The occupant's real JID, if visible
    pub jid: Option<String>,

    /// MUC affiliation
    pub affiliation: MucAffiliation,

    /// MUC role
    pub role: MucRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MucAffiliation {
    Owner,
    Admin,
    Member,
    Outcast,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MucRole {
    Moderator,
    Participant,
    Visitor,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrollDirection {
    Up,
    Down,
    Top,
    Bottom,
}

#[cfg(feature = "native")]
pub trait EventBus: Send + Sync + 'static {
    fn publish(&self, event: Event) -> std::result::Result<(), crate::error::EventBusError>;
    fn subscribe(
        &self,
        pattern: &str,
    ) -> std::result::Result<EventSubscription, crate::error::EventBusError>;
}

#[cfg(feature = "native")]
#[derive(Clone)]
pub struct BroadcastEventBus {
    system_sender: broadcast::Sender<Event>,
    xmpp_sender: broadcast::Sender<Event>,
    ui_sender: broadcast::Sender<Event>,
    plugin_sender: broadcast::Sender<Event>,
}

#[cfg(feature = "native")]
impl BroadcastEventBus {
    pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

    pub fn new(channel_capacity: usize) -> Self {
        let capacity = channel_capacity.max(1);
        let (system_sender, _) = broadcast::channel(capacity);
        let (xmpp_sender, _) = broadcast::channel(capacity);
        let (ui_sender, _) = broadcast::channel(capacity);
        let (plugin_sender, _) = broadcast::channel(capacity);

        Self {
            system_sender,
            xmpp_sender,
            ui_sender,
            plugin_sender,
        }
    }

    fn sender_for_domain(&self, domain: &str) -> Option<&broadcast::Sender<Event>> {
        match domain {
            "system" => Some(&self.system_sender),
            "xmpp" => Some(&self.xmpp_sender),
            "ui" => Some(&self.ui_sender),
            "plugin" => Some(&self.plugin_sender),
            _ => None,
        }
    }

    fn receivers_for_pattern(
        &self,
        pattern: &str,
    ) -> std::result::Result<DomainReceivers, crate::error::EventBusError> {
        let first_segment = pattern.split('.').next().unwrap_or_default();

        if first_segment.is_empty() {
            return Err(crate::error::EventBusError::InvalidPattern(
                pattern.to_string(),
            ));
        }

        if has_glob_meta(first_segment) {
            return Ok(DomainReceivers {
                system: Some(self.system_sender.subscribe()),
                xmpp: Some(self.xmpp_sender.subscribe()),
                ui: Some(self.ui_sender.subscribe()),
                plugin: Some(self.plugin_sender.subscribe()),
            });
        }

        match first_segment {
            "system" => Ok(DomainReceivers {
                system: Some(self.system_sender.subscribe()),
                xmpp: None,
                ui: None,
                plugin: None,
            }),
            "xmpp" => Ok(DomainReceivers {
                system: None,
                xmpp: Some(self.xmpp_sender.subscribe()),
                ui: None,
                plugin: None,
            }),
            "ui" => Ok(DomainReceivers {
                system: None,
                xmpp: None,
                ui: Some(self.ui_sender.subscribe()),
                plugin: None,
            }),
            "plugin" => Ok(DomainReceivers {
                system: None,
                xmpp: None,
                ui: None,
                plugin: Some(self.plugin_sender.subscribe()),
            }),
            _ => Err(crate::error::EventBusError::InvalidPattern(
                pattern.to_string(),
            )),
        }
    }
}

#[cfg(feature = "native")]
impl Default for BroadcastEventBus {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CHANNEL_CAPACITY)
    }
}

#[cfg(feature = "native")]
impl EventBus for BroadcastEventBus {
    fn publish(&self, event: Event) -> std::result::Result<(), crate::error::EventBusError> {
        let sender = self
            .sender_for_domain(event.channel.domain())
            .ok_or_else(|| {
                crate::error::EventBusError::InvalidChannel(event.channel.to_string())
            })?;

        let _ = sender.send(event);
        Ok(())
    }

    fn subscribe(
        &self,
        pattern: &str,
    ) -> std::result::Result<EventSubscription, crate::error::EventBusError> {
        let matcher = Glob::new(pattern)
            .map_err(|_| crate::error::EventBusError::InvalidPattern(pattern.to_string()))?
            .compile_matcher();
        let receivers = self.receivers_for_pattern(pattern)?;

        Ok(EventSubscription { matcher, receivers })
    }
}

#[cfg(feature = "native")]
struct DomainReceivers {
    system: Option<broadcast::Receiver<Event>>,
    xmpp: Option<broadcast::Receiver<Event>>,
    ui: Option<broadcast::Receiver<Event>>,
    plugin: Option<broadcast::Receiver<Event>>,
}

#[cfg(feature = "native")]
pub struct EventSubscription {
    matcher: GlobMatcher,
    receivers: DomainReceivers,
}

#[cfg(feature = "native")]
impl EventSubscription {
    pub async fn recv(&mut self) -> std::result::Result<Event, crate::error::EventBusError> {
        loop {
            let system_receiver = self.receivers.system.as_mut();
            let xmpp_receiver = self.receivers.xmpp.as_mut();
            let ui_receiver = self.receivers.ui.as_mut();
            let plugin_receiver = self.receivers.plugin.as_mut();

            let received = tokio::select! {
                result = recv_from_domain(system_receiver) => result,
                result = recv_from_domain(xmpp_receiver) => result,
                result = recv_from_domain(ui_receiver) => result,
                result = recv_from_domain(plugin_receiver) => result,
            };

            match received {
                Ok(event) if self.matcher.is_match(event.channel.as_str()) => return Ok(event),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(crate::error::EventBusError::ChannelClosed);
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    return Err(crate::error::EventBusError::Lagged(count));
                }
            }
        }
    }
}

#[cfg(feature = "native")]
async fn recv_from_domain(
    receiver: Option<&mut broadcast::Receiver<Event>>,
) -> std::result::Result<Event, broadcast::error::RecvError> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(feature = "native")]
fn has_glob_meta(segment: &str) -> bool {
    segment.contains('*')
        || segment.contains('?')
        || segment.contains('[')
        || segment.contains(']')
        || segment.contains('{')
        || segment.contains('}')
        || segment.contains('!')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_validation() {
        assert!(Channel::is_valid("system.startup.complete"));
        assert!(Channel::is_valid("xmpp.message.received"));
        assert!(Channel::is_valid("ui.theme.changed"));
        assert!(Channel::is_valid("plugin.test.event"));

        assert!(!Channel::is_valid("invalid.domain.event"));
        assert!(!Channel::is_valid("system..double.dot"));
        assert!(!Channel::is_valid(".starts.with.dot"));
        assert!(!Channel::is_valid("ends.with.dot."));
        assert!(!Channel::is_valid("UpperCase"));
        assert!(!Channel::is_valid("with-hyphen"));
        assert!(!Channel::is_valid(""));
    }

    #[test]
    fn test_channel_domain() {
        let c = Channel::new("xmpp.message.received").unwrap();
        assert_eq!(c.domain(), "xmpp");
    }

    #[test]
    fn test_channel_domain_all_domains() {
        let cases = [
            ("system.startup.complete", "system"),
            ("xmpp.message.received", "xmpp"),
            ("ui.theme.changed", "ui"),
            ("plugin.foo.loaded", "plugin"),
        ];
        for (name, expected) in cases {
            let c = Channel::new(name).unwrap();
            assert_eq!(c.domain(), expected, "domain of {name}");
        }
    }

    #[test]
    fn test_channel_new_rejects_invalid() {
        let result = Channel::new("bad.domain.event");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::EventBusError::InvalidChannel(_)
        ));
    }

    #[test]
    fn test_channel_as_str_and_display() {
        let c = Channel::new("xmpp.roster.updated").unwrap();
        assert_eq!(c.as_str(), "xmpp.roster.updated");
        assert_eq!(c.to_string(), "xmpp.roster.updated");
    }

    #[test]
    fn test_channel_into_string() {
        let c = Channel::new("ui.conversation.opened").unwrap();
        let s: String = c.into();
        assert_eq!(s, "ui.conversation.opened");
    }

    #[test]
    fn test_channel_two_segment() {
        assert!(Channel::is_valid("system.startup"));
        let c = Channel::new("system.startup").unwrap();
        assert_eq!(c.domain(), "system");
    }

    #[test]
    fn test_event_new_fields() {
        let channel = Channel::new("system.startup.complete").unwrap();
        let event = Event::new(
            channel.clone(),
            EventSource::System("test".into()),
            EventPayload::StartupComplete,
        );

        assert_eq!(event.channel, channel);
        assert!(event.correlation_id.is_none());
        assert!(!event.id.is_nil());
    }

    #[test]
    fn test_event_with_correlation() {
        let channel = Channel::new("xmpp.message.sent").unwrap();
        let corr_id = Uuid::new_v4();
        let event = Event::with_correlation(
            channel,
            EventSource::Xmpp,
            EventPayload::MessageSent {
                message: ChatMessage {
                    id: "msg1".into(),
                    from: "alice@example.com".into(),
                    to: "bob@example.com".into(),
                    body: "hello".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
            corr_id,
        );

        assert_eq!(event.correlation_id, Some(corr_id));
    }

    #[test]
    fn test_event_unique_ids() {
        let channel = Channel::new("system.startup.complete").unwrap();
        let e1 = Event::new(
            channel.clone(),
            EventSource::System("test".into()),
            EventPayload::StartupComplete,
        );
        let e2 = Event::new(
            channel,
            EventSource::System("test".into()),
            EventPayload::StartupComplete,
        );
        assert_ne!(e1.id, e2.id);
    }
}

#[cfg(all(test, feature = "native"))]
mod event_bus_tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    fn make_event(channel: &str, payload: EventPayload) -> Event {
        Event::new(
            Channel::new(channel).unwrap(),
            EventSource::System("test".into()),
            payload,
        )
    }

    // ── Routing correctness ───────────────────────────────────────

    #[tokio::test]
    async fn publish_to_system_routes_to_system_subscriber() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("system.**").unwrap();

        bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "system.startup.complete");
    }

    #[tokio::test]
    async fn publish_to_xmpp_routes_to_xmpp_subscriber() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("xmpp.**").unwrap();

        bus.publish(make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "alice@example.com".into(),
                    to: "bob@example.com".into(),
                    body: "hi".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "xmpp.message.received");
    }

    #[tokio::test]
    async fn publish_to_ui_routes_to_ui_subscriber() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("ui.**").unwrap();

        bus.publish(make_event(
            "ui.theme.changed",
            EventPayload::ThemeChanged {
                theme_id: "dark".into(),
            },
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "ui.theme.changed");
    }

    #[tokio::test]
    async fn publish_to_plugin_routes_to_plugin_subscriber() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("plugin.**").unwrap();

        bus.publish(make_event(
            "plugin.foo.loaded",
            EventPayload::PluginLoaded {
                plugin_id: "foo".into(),
                version: "1.0".into(),
            },
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "plugin.foo.loaded");
    }

    #[tokio::test]
    async fn xmpp_event_not_received_by_system_subscriber() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("system.**").unwrap();

        bus.publish(make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "test".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        ))
        .unwrap();

        let result = timeout(Duration::from_millis(50), sub.recv()).await;
        assert!(
            result.is_err(),
            "system subscriber should not receive xmpp events"
        );
    }

    #[tokio::test]
    async fn publish_succeeds_with_no_subscribers() {
        let bus = BroadcastEventBus::default();
        let result = bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn multiple_subscribers_same_domain_each_get_event() {
        let bus = BroadcastEventBus::default();
        let mut sub1 = bus.subscribe("xmpp.**").unwrap();
        let mut sub2 = bus.subscribe("xmpp.**").unwrap();

        bus.publish(make_event(
            "xmpp.roster.updated",
            EventPayload::RosterUpdated {
                item: RosterItem {
                    jid: "alice@example.com".into(),
                    name: Some("Alice".into()),
                    subscription: Subscription::Both,
                    groups: vec![],
                },
            },
        ))
        .unwrap();

        let e1 = timeout(Duration::from_millis(100), sub1.recv())
            .await
            .expect("sub1 timed out")
            .unwrap();
        let e2 = timeout(Duration::from_millis(100), sub2.recv())
            .await
            .expect("sub2 timed out")
            .unwrap();

        assert_eq!(e1.channel.as_str(), "xmpp.roster.updated");
        assert_eq!(e2.channel.as_str(), "xmpp.roster.updated");
        assert_eq!(e1.id, e2.id);
    }

    // ── Glob filtering ────────────────────────────────────────────

    #[tokio::test]
    async fn glob_star_matches_single_segment() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("xmpp.message.*").unwrap();

        bus.publish(make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "test".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "xmpp.message.received");
    }

    #[tokio::test]
    async fn glob_doublestar_matches_all_depths() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("xmpp.**").unwrap();

        bus.publish(make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "test".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        ))
        .unwrap();
        bus.publish(make_event(
            "xmpp.roster.updated",
            EventPayload::RosterUpdated {
                item: RosterItem {
                    jid: "a@b".into(),
                    name: None,
                    subscription: Subscription::None,
                    groups: vec![],
                },
            },
        ))
        .unwrap();

        let e1 = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        let e2 = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();

        assert_eq!(e1.channel.as_str(), "xmpp.message.received");
        assert_eq!(e2.channel.as_str(), "xmpp.roster.updated");
    }

    #[tokio::test]
    async fn glob_filters_non_matching_channels_within_domain() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("xmpp.roster.*").unwrap();

        // Publish a message event (should not match roster pattern)
        bus.publish(make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "test".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        ))
        .unwrap();

        // Publish a roster event (should match)
        bus.publish(make_event(
            "xmpp.roster.updated",
            EventPayload::RosterUpdated {
                item: RosterItem {
                    jid: "a@b".into(),
                    name: None,
                    subscription: Subscription::None,
                    groups: vec![],
                },
            },
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "xmpp.roster.updated");
    }

    #[tokio::test]
    async fn wildcard_first_segment_receives_all_domains() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("**.received").unwrap();

        bus.publish(make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "test".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "xmpp.message.received");
    }

    #[tokio::test]
    async fn firehose_doublestar_receives_everything() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("**").unwrap();

        bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ))
        .unwrap();
        bus.publish(make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "hi".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        ))
        .unwrap();
        bus.publish(make_event(
            "ui.theme.changed",
            EventPayload::ThemeChanged {
                theme_id: "dark".into(),
            },
        ))
        .unwrap();
        bus.publish(make_event(
            "plugin.foo.loaded",
            EventPayload::PluginLoaded {
                plugin_id: "foo".into(),
                version: "1.0".into(),
            },
        ))
        .unwrap();

        let mut channels = Vec::new();
        for _ in 0..4 {
            let event = timeout(Duration::from_millis(100), sub.recv())
                .await
                .expect("timed out")
                .unwrap();
            channels.push(event.channel.as_str().to_string());
        }

        channels.sort();
        assert_eq!(
            channels,
            vec![
                "plugin.foo.loaded",
                "system.startup.complete",
                "ui.theme.changed",
                "xmpp.message.received",
            ]
        );
    }

    // ── Subscribe error cases ─────────────────────────────────────

    #[tokio::test]
    async fn subscribe_invalid_pattern_returns_error() {
        let bus = BroadcastEventBus::default();
        let result = bus.subscribe("[invalid");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn subscribe_empty_pattern_returns_error() {
        let bus = BroadcastEventBus::default();
        let result = bus.subscribe("");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn subscribe_unknown_literal_domain_returns_error() {
        let bus = BroadcastEventBus::default();
        let result = bus.subscribe("unknown.domain.event");
        assert!(matches!(
            result,
            Err(crate::error::EventBusError::InvalidPattern(_))
        ));
    }

    // ── Per-domain ordering ───────────────────────────────────────

    #[tokio::test]
    async fn events_within_domain_preserve_publish_order() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("xmpp.**").unwrap();

        for i in 0..10 {
            bus.publish(make_event(
                "xmpp.message.received",
                EventPayload::MessageReceived {
                    message: ChatMessage {
                        id: format!("msg{i}"),
                        from: "a@b".into(),
                        to: "c@d".into(),
                        body: format!("message {i}"),
                        timestamp: Utc::now(),
                        message_type: MessageType::Chat,
                        thread: None,
                    },
                },
            ))
            .unwrap();
        }

        for i in 0..10 {
            let event = timeout(Duration::from_millis(100), sub.recv())
                .await
                .expect("timed out")
                .unwrap();
            match &event.payload {
                EventPayload::MessageReceived { message } => {
                    assert_eq!(message.id, format!("msg{i}"), "out of order at index {i}");
                }
                _ => panic!("unexpected payload"),
            }
        }
    }

    #[tokio::test]
    async fn events_across_different_domains_are_all_delivered() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("**").unwrap();

        bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ))
        .unwrap();
        bus.publish(make_event(
            "xmpp.roster.updated",
            EventPayload::RosterUpdated {
                item: RosterItem {
                    jid: "a@b".into(),
                    name: None,
                    subscription: Subscription::None,
                    groups: vec![],
                },
            },
        ))
        .unwrap();
        bus.publish(make_event(
            "ui.conversation.opened",
            EventPayload::ConversationOpened { jid: "a@b".into() },
        ))
        .unwrap();

        let mut received = Vec::new();
        for _ in 0..3 {
            let event = timeout(Duration::from_millis(100), sub.recv())
                .await
                .expect("timed out")
                .unwrap();
            received.push(event.channel.as_str().to_string());
        }

        received.sort();
        assert_eq!(
            received,
            vec![
                "system.startup.complete",
                "ui.conversation.opened",
                "xmpp.roster.updated",
            ]
        );
    }

    // ── Correlation ID tracking ───────────────────────────────────

    #[tokio::test]
    async fn correlated_events_share_correlation_id() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("xmpp.**").unwrap();

        let corr_id = Uuid::new_v4();

        bus.publish(Event::with_correlation(
            Channel::new("xmpp.message.sent").unwrap(),
            EventSource::Ui(UiTarget::Tui),
            EventPayload::MessageSent {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "alice@example.com".into(),
                    to: "bob@example.com".into(),
                    body: "hello".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
            corr_id,
        ))
        .unwrap();

        bus.publish(Event::with_correlation(
            Channel::new("xmpp.message.delivered").unwrap(),
            EventSource::Xmpp,
            EventPayload::MessageDelivered {
                id: "m1".into(),
                to: "bob@example.com".into(),
            },
            corr_id,
        ))
        .unwrap();

        let e1 = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        let e2 = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();

        assert_eq!(e1.correlation_id, Some(corr_id));
        assert_eq!(e2.correlation_id, Some(corr_id));
        assert_ne!(e1.id, e2.id);
    }

    #[tokio::test]
    async fn events_without_correlation_have_none() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("system.**").unwrap();

        bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert!(event.correlation_id.is_none());
    }

    #[tokio::test]
    async fn correlation_id_filter_across_subscriber() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("xmpp.**").unwrap();

        let target_corr = Uuid::new_v4();
        let other_corr = Uuid::new_v4();

        bus.publish(Event::with_correlation(
            Channel::new("xmpp.message.sent").unwrap(),
            EventSource::Xmpp,
            EventPayload::MessageSent {
                message: ChatMessage {
                    id: "m1".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "hello".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
            target_corr,
        ))
        .unwrap();

        bus.publish(Event::with_correlation(
            Channel::new("xmpp.message.sent").unwrap(),
            EventSource::Xmpp,
            EventPayload::MessageSent {
                message: ChatMessage {
                    id: "m2".into(),
                    from: "a@b".into(),
                    to: "c@d".into(),
                    body: "world".into(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
            other_corr,
        ))
        .unwrap();

        bus.publish(Event::with_correlation(
            Channel::new("xmpp.message.delivered").unwrap(),
            EventSource::Xmpp,
            EventPayload::MessageDelivered {
                id: "m1".into(),
                to: "c@d".into(),
            },
            target_corr,
        ))
        .unwrap();

        let mut target_events = Vec::new();
        for _ in 0..3 {
            let event = timeout(Duration::from_millis(100), sub.recv())
                .await
                .expect("timed out")
                .unwrap();
            if event.correlation_id == Some(target_corr) {
                target_events.push(event);
            }
        }

        assert_eq!(target_events.len(), 2);
        assert_eq!(target_events[0].channel.as_str(), "xmpp.message.sent");
        assert_eq!(target_events[1].channel.as_str(), "xmpp.message.delivered");
    }

    // ── Lagged subscriber recovery ────────────────────────────────

    #[tokio::test]
    async fn lagged_subscriber_returns_lagged_error() {
        let bus = BroadcastEventBus::new(2);
        let mut sub = bus.subscribe("system.**").unwrap();

        // Overflow the small buffer
        for i in 0..10 {
            bus.publish(make_event(
                "system.startup.complete",
                EventPayload::ErrorOccurred {
                    component: "test".into(),
                    message: format!("event {i}"),
                    recoverable: true,
                },
            ))
            .unwrap();
        }

        let result = sub.recv().await;
        assert!(
            matches!(result, Err(crate::error::EventBusError::Lagged(_))),
            "expected Lagged error, got {result:?}"
        );
    }

    #[tokio::test]
    async fn subscriber_recovers_after_lag() {
        let bus = BroadcastEventBus::new(2);
        let mut sub = bus.subscribe("system.**").unwrap();

        // Overflow to cause lag
        for i in 0..5 {
            bus.publish(make_event(
                "system.startup.complete",
                EventPayload::ErrorOccurred {
                    component: "test".into(),
                    message: format!("old {i}"),
                    recoverable: true,
                },
            ))
            .unwrap();
        }

        // First recv should return Lagged
        let result = sub.recv().await;
        assert!(matches!(
            result,
            Err(crate::error::EventBusError::Lagged(_))
        ));

        // Drain any remaining buffered events
        loop {
            match timeout(Duration::from_millis(10), sub.recv()).await {
                Ok(Ok(_)) => continue,
                Ok(Err(crate::error::EventBusError::Lagged(_))) => continue,
                _ => break,
            }
        }

        // Publish a new event after the lag
        bus.publish(make_event(
            "system.config.reloaded",
            EventPayload::ConfigReloaded,
        ))
        .unwrap();

        // Subscriber should recover and receive new events
        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out after lag recovery")
            .unwrap();
        assert_eq!(event.channel.as_str(), "system.config.reloaded");
    }

    // ── Channel closed ────────────────────────────────────────────

    #[tokio::test]
    async fn channel_closed_when_bus_dropped() {
        let mut sub;
        {
            let bus = BroadcastEventBus::default();
            sub = bus.subscribe("system.**").unwrap();
        }

        let result = sub.recv().await;
        assert!(matches!(
            result,
            Err(crate::error::EventBusError::ChannelClosed)
        ));
    }

    // ── BroadcastEventBus construction ────────────────────────────

    #[tokio::test]
    async fn default_construction_works() {
        let bus = BroadcastEventBus::default();
        let mut sub = bus.subscribe("system.**").unwrap();

        bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "system.startup.complete");
    }

    #[tokio::test]
    async fn zero_capacity_clamped_to_one() {
        let bus = BroadcastEventBus::new(0);
        let mut sub = bus.subscribe("system.**").unwrap();

        bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "system.startup.complete");
    }

    // ── EventBus trait usage ──────────────────────────────────────

    #[tokio::test]
    async fn trait_object_publish_and_subscribe() {
        let bus: Box<dyn EventBus> = Box::new(BroadcastEventBus::default());
        let mut sub = bus.subscribe("system.**").unwrap();

        bus.publish(make_event(
            "system.startup.complete",
            EventPayload::StartupComplete,
        ))
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(event.channel.as_str(), "system.startup.complete");
    }

    // ── has_glob_meta ─────────────────────────────────────────────

    #[test]
    fn has_glob_meta_detects_metacharacters() {
        assert!(has_glob_meta("*"));
        assert!(has_glob_meta("?"));
        assert!(has_glob_meta("[a]"));
        assert!(has_glob_meta("{a,b}"));
        assert!(has_glob_meta("!foo"));
        assert!(has_glob_meta("**"));
        assert!(!has_glob_meta("xmpp"));
        assert!(!has_glob_meta("system"));
        assert!(!has_glob_meta("plain123"));
    }
}
