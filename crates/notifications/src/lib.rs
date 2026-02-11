use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[cfg(feature = "native")]
use notify_rust::Notification;
use tracing::error;
#[cfg(feature = "native")]
use tracing::{debug, warn};
#[cfg(feature = "native")]
use waddle_core::config::Config;
#[cfg(feature = "native")]
use waddle_core::error::EventBusError;
#[cfg(feature = "native")]
use waddle_core::event::{Channel, EventBus, EventSource};
use waddle_core::event::{ChatMessage, Event, EventPayload};

const AGGREGATION_WINDOW: Duration = Duration::from_secs(2);
const AGGREGATION_THRESHOLD: usize = 3;
#[cfg(feature = "native")]
const NOTIFICATION_SOURCE: &str = "notifications";

#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    #[error("notification dispatch failed: {0}")]
    DispatchFailed(String),

    #[error("notification permission denied")]
    PermissionDenied,

    #[cfg(feature = "native")]
    #[error("event bus error: {0}")]
    EventBus(#[from] EventBusError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationRequest {
    title: String,
    body: String,
    event_id: Option<String>,
    conversation_jid: Option<String>,
}

impl NotificationRequest {
    fn summary(total: usize) -> Self {
        Self {
            title: "Waddle".to_string(),
            body: format!("{total} new messages"),
            event_id: None,
            conversation_jid: None,
        }
    }
}

#[derive(Debug, Default)]
struct AggregationState {
    recent_notifications: VecDeque<Instant>,
}

impl AggregationState {
    fn record_and_count(&mut self, now: Instant) -> usize {
        self.recent_notifications.push_back(now);
        while let Some(oldest) = self.recent_notifications.front() {
            if now.duration_since(*oldest) > AGGREGATION_WINDOW {
                self.recent_notifications.pop_front();
            } else {
                break;
            }
        }
        self.recent_notifications.len()
    }
}

trait NotificationDispatcher: Send + Sync {
    fn dispatch(&self, request: NotificationRequest) -> Result<(), NotificationError>;
}

#[cfg(feature = "native")]
#[derive(Debug, Default)]
struct NativeNotificationDispatcher;

#[cfg(feature = "native")]
impl NotificationDispatcher for NativeNotificationDispatcher {
    fn dispatch(&self, request: NotificationRequest) -> Result<(), NotificationError> {
        let mut notification = Notification::new();
        notification.summary(&request.title).body(&request.body);

        #[cfg(all(unix, not(target_os = "macos")))]
        if request.event_id.is_some() {
            notification.action("default", "Open");
        }

        notification
            .show()
            .map_err(|error| NotificationError::DispatchFailed(error.to_string()))?;
        Ok(())
    }
}

pub struct NotificationManager {
    notifications_enabled: AtomicBool,
    focused_conversation: RwLock<Option<String>>,
    muted_conversations: RwLock<HashSet<String>>,
    highlight_keywords: RwLock<HashSet<String>>,
    room_nicks: RwLock<HashMap<String, String>>,
    account_localpart: RwLock<Option<String>>,
    aggregation: Mutex<AggregationState>,
    dispatcher: Arc<dyn NotificationDispatcher>,
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl NotificationManager {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>, notifications_enabled: bool) -> Self {
        Self::with_dispatcher(
            event_bus,
            notifications_enabled,
            Arc::new(NativeNotificationDispatcher),
        )
    }

    #[cfg(feature = "native")]
    pub async fn run(
        event_bus: Arc<dyn EventBus>,
        config: &Config,
    ) -> Result<(), NotificationError> {
        let manager = Arc::new(Self::new(event_bus, config.ui.notifications));
        manager.serve().await
    }

    pub fn set_notifications_enabled(&self, enabled: bool) {
        self.notifications_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn set_focused_conversation(&self, jid: Option<&str>) {
        let mut focused = self.focused_conversation.write().unwrap();
        *focused = jid.map(normalize_jid);
    }

    pub fn set_conversation_muted(&self, jid: &str, muted: bool) {
        let normalized = normalize_jid(jid);
        let mut muted_conversations = self.muted_conversations.write().unwrap();
        if muted {
            muted_conversations.insert(normalized);
        } else {
            muted_conversations.remove(&normalized);
        }
    }

    pub fn is_conversation_muted(&self, jid: &str) -> bool {
        let normalized = normalize_jid(jid);
        self.muted_conversations
            .read()
            .unwrap()
            .contains(&normalized)
    }

    pub fn set_highlight_keywords(&self, keywords: &[String]) {
        let normalized = keywords
            .iter()
            .map(|keyword| keyword.trim().to_ascii_lowercase())
            .filter(|keyword| !keyword.is_empty())
            .collect::<HashSet<_>>();
        *self.highlight_keywords.write().unwrap() = normalized;
    }

    pub fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::ConnectionEstablished { jid } => {
                *self.account_localpart.write().unwrap() = extract_localpart(jid);
            }
            EventPayload::ConversationOpened { jid } => {
                self.set_focused_conversation(Some(jid));
            }
            EventPayload::ConversationClosed { jid } => {
                let normalized = normalize_jid(jid);
                let mut focused = self.focused_conversation.write().unwrap();
                if focused.as_deref() == Some(normalized.as_str()) {
                    *focused = None;
                }
            }
            EventPayload::MucJoined { room, nick } => {
                self.room_nicks
                    .write()
                    .unwrap()
                    .insert(normalize_jid(room), nick.clone());
            }
            EventPayload::MucLeft { room } => {
                self.room_nicks
                    .write()
                    .unwrap()
                    .remove(&normalize_jid(room));
            }
            EventPayload::MessageReceived { message } => {
                self.maybe_notify_message(message);
            }
            EventPayload::MucMessageReceived { room, message } => {
                self.maybe_notify_muc_message(room, message);
            }
            EventPayload::SubscriptionRequest { from } => {
                self.maybe_notify_subscription_request(from);
            }
            _ => {}
        }
    }

    #[cfg(feature = "native")]
    pub fn emit_notification_clicked(&self, event_id: &str) -> Result<(), NotificationError> {
        self.event_bus.publish(Event::new(
            Channel::new("ui.notification.clicked").unwrap(),
            EventSource::System(NOTIFICATION_SOURCE.to_string()),
            EventPayload::NotificationClicked {
                event_id: event_id.to_string(),
            },
        ))?;
        Ok(())
    }

    fn maybe_notify_message(&self, message: &ChatMessage) {
        let conversation_jid = normalize_jid(&message.from);
        if !self.should_notify_for_conversation(&conversation_jid) {
            return;
        }

        self.dispatch_with_aggregation(NotificationRequest {
            title: conversation_jid.clone(),
            body: message.body.clone(),
            event_id: Some(message.id.clone()),
            conversation_jid: Some(conversation_jid),
        });
    }

    fn maybe_notify_muc_message(&self, room: &str, message: &ChatMessage) {
        let room_jid = normalize_jid(room);
        if !self.should_notify_for_conversation(&room_jid) {
            return;
        }

        if !self.is_muc_highlight(&room_jid, message) {
            return;
        }

        self.dispatch_with_aggregation(NotificationRequest {
            title: room_jid.clone(),
            body: message.body.clone(),
            event_id: Some(message.id.clone()),
            conversation_jid: Some(room_jid),
        });
    }

    fn maybe_notify_subscription_request(&self, from: &str) {
        let from_jid = normalize_jid(from);
        if !self.notifications_enabled.load(Ordering::Relaxed) {
            return;
        }

        self.dispatch_with_aggregation(NotificationRequest {
            title: "Subscription request".to_string(),
            body: format!("{from_jid} wants to subscribe to your presence"),
            event_id: None,
            conversation_jid: Some(from_jid),
        });
    }

    fn should_notify_for_conversation(&self, conversation_jid: &str) -> bool {
        if !self.notifications_enabled.load(Ordering::Relaxed) {
            return false;
        }

        if self
            .muted_conversations
            .read()
            .unwrap()
            .contains(conversation_jid)
        {
            return false;
        }

        self.focused_conversation.read().unwrap().as_deref() != Some(conversation_jid)
    }

    fn is_muc_highlight(&self, room_jid: &str, message: &ChatMessage) -> bool {
        let body = message.body.to_ascii_lowercase();
        if body.is_empty() {
            return false;
        }

        let room_nick = self.room_nicks.read().unwrap().get(room_jid).cloned();
        if let Some(nick) = room_nick {
            let nick = nick.to_ascii_lowercase();
            if body.contains(&format!("@{nick}")) {
                return true;
            }
        }

        let account_localpart = self.account_localpart.read().unwrap().clone();
        if let Some(localpart) = account_localpart
            && body.contains(&format!("@{localpart}"))
        {
            return true;
        }

        self.highlight_keywords
            .read()
            .unwrap()
            .iter()
            .any(|keyword| body.contains(keyword))
    }

    fn dispatch_with_aggregation(&self, request: NotificationRequest) {
        let count = self
            .aggregation
            .lock()
            .unwrap()
            .record_and_count(Instant::now());

        let outgoing = if count > AGGREGATION_THRESHOLD {
            NotificationRequest::summary(count)
        } else {
            request
        };

        if let Err(error) = self.dispatcher.dispatch(outgoing) {
            error!(error = %error, "failed to dispatch notification");
        }
    }

    #[cfg(feature = "native")]
    async fn serve(self: Arc<Self>) -> Result<(), NotificationError> {
        let mut subscription = self.event_bus.subscribe("{system,xmpp,ui}.**")?;

        loop {
            match subscription.recv().await {
                Ok(event) => {
                    self.handle_event(&event);
                }
                Err(EventBusError::ChannelClosed) => {
                    debug!("event bus closed, notification manager stopping");
                    return Ok(());
                }
                Err(EventBusError::Lagged(count)) => {
                    warn!(count, "notification manager lagged, some events dropped");
                }
                Err(error) => {
                    return Err(error.into());
                }
            }
        }
    }

    #[cfg(feature = "native")]
    fn with_dispatcher(
        event_bus: Arc<dyn EventBus>,
        notifications_enabled: bool,
        dispatcher: Arc<dyn NotificationDispatcher>,
    ) -> Self {
        Self {
            notifications_enabled: AtomicBool::new(notifications_enabled),
            focused_conversation: RwLock::new(None),
            muted_conversations: RwLock::new(HashSet::new()),
            highlight_keywords: RwLock::new(HashSet::new()),
            room_nicks: RwLock::new(HashMap::new()),
            account_localpart: RwLock::new(None),
            aggregation: Mutex::new(AggregationState::default()),
            dispatcher,
            event_bus,
        }
    }
}

fn normalize_jid(jid: &str) -> String {
    jid.split('/').next().unwrap_or(jid).to_string()
}

fn extract_localpart(jid: &str) -> Option<String> {
    let bare = normalize_jid(jid);
    let (localpart, _) = bare.split_once('@')?;
    if localpart.is_empty() {
        None
    } else {
        Some(localpart.to_ascii_lowercase())
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use waddle_core::event::{
        BroadcastEventBus, Channel, Event, EventBus, EventPayload, EventSource, MessageType,
    };

    #[derive(Default)]
    struct TestDispatcher {
        notifications: Mutex<Vec<NotificationRequest>>,
        fail_dispatch: AtomicBool,
    }

    impl TestDispatcher {
        fn notifications(&self) -> Vec<NotificationRequest> {
            self.notifications.lock().unwrap().clone()
        }

        fn set_fail_dispatch(&self, should_fail: bool) {
            self.fail_dispatch.store(should_fail, Ordering::Relaxed);
        }
    }

    impl NotificationDispatcher for TestDispatcher {
        fn dispatch(&self, request: NotificationRequest) -> Result<(), NotificationError> {
            if self.fail_dispatch.load(Ordering::Relaxed) {
                return Err(NotificationError::DispatchFailed(
                    "forced failure".to_string(),
                ));
            }

            self.notifications.lock().unwrap().push(request);
            Ok(())
        }
    }

    fn make_manager(notifications_enabled: bool) -> (NotificationManager, Arc<TestDispatcher>) {
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());
        let dispatcher = Arc::new(TestDispatcher::default());
        let manager = NotificationManager::with_dispatcher(
            event_bus,
            notifications_enabled,
            dispatcher.clone(),
        );
        (manager, dispatcher)
    }

    fn make_event(channel: &str, payload: EventPayload) -> Event {
        Event::new(
            Channel::new(channel).unwrap(),
            EventSource::System("test".to_string()),
            payload,
        )
    }

    fn make_message_event(from: &str, body: &str, id: &str) -> Event {
        make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: ChatMessage {
                    id: id.to_string(),
                    from: from.to_string(),
                    to: "user@example.com".to_string(),
                    body: body.to_string(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Chat,
                    thread: None,
                },
            },
        )
    }

    fn make_muc_message_event(room: &str, body: &str, id: &str) -> Event {
        make_event(
            "xmpp.muc.message.received",
            EventPayload::MucMessageReceived {
                room: room.to_string(),
                message: ChatMessage {
                    id: id.to_string(),
                    from: format!("{room}/alice"),
                    to: room.to_string(),
                    body: body.to_string(),
                    timestamp: Utc::now(),
                    message_type: MessageType::Groupchat,
                    thread: None,
                },
            },
        )
    }

    #[test]
    fn notifications_disabled_suppresses_message_notifications() {
        let (manager, dispatcher) = make_manager(false);
        manager.handle_event(&make_message_event("alice@example.com", "hello", "m1"));
        assert!(dispatcher.notifications().is_empty());
    }

    #[test]
    fn focused_conversation_suppresses_notifications() {
        let (manager, dispatcher) = make_manager(true);
        manager.handle_event(&make_event(
            "ui.conversation.opened",
            EventPayload::ConversationOpened {
                jid: "alice@example.com".to_string(),
            },
        ));
        manager.handle_event(&make_message_event(
            "alice@example.com/laptop",
            "hello",
            "m1",
        ));
        assert!(dispatcher.notifications().is_empty());
    }

    #[test]
    fn muted_conversation_suppresses_notifications() {
        let (manager, dispatcher) = make_manager(true);
        manager.set_conversation_muted("alice@example.com", true);
        manager.handle_event(&make_message_event("alice@example.com", "hello", "m1"));
        assert!(dispatcher.notifications().is_empty());
    }

    #[test]
    fn incoming_message_dispatches_notification() {
        let (manager, dispatcher) = make_manager(true);
        manager.handle_event(&make_message_event("alice@example.com", "Hello!", "m1"));

        let notifications = dispatcher.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].title, "alice@example.com");
        assert_eq!(notifications[0].body, "Hello!");
    }

    #[test]
    fn notification_burst_is_aggregated() {
        let (manager, dispatcher) = make_manager(true);

        manager.handle_event(&make_message_event("a1@example.com", "1", "m1"));
        manager.handle_event(&make_message_event("a2@example.com", "2", "m2"));
        manager.handle_event(&make_message_event("a3@example.com", "3", "m3"));
        manager.handle_event(&make_message_event("a4@example.com", "4", "m4"));

        let notifications = dispatcher.notifications();
        assert_eq!(notifications.len(), 4);
        assert_eq!(notifications[3].title, "Waddle");
        assert_eq!(notifications[3].body, "4 new messages");
    }

    #[test]
    fn subscription_request_dispatches_notification() {
        let (manager, dispatcher) = make_manager(true);
        manager.handle_event(&make_event(
            "xmpp.subscription.request",
            EventPayload::SubscriptionRequest {
                from: "newcontact@example.com".to_string(),
            },
        ));

        let notifications = dispatcher.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].title, "Subscription request");
        assert!(notifications[0].body.contains("newcontact@example.com"));
    }

    #[test]
    fn muc_notifications_require_mention() {
        let (manager, dispatcher) = make_manager(true);
        manager.handle_event(&make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "user@example.com".to_string(),
            },
        ));

        manager.handle_event(&make_muc_message_event(
            "dev@conference.example.com",
            "general room update",
            "m1",
        ));
        assert!(dispatcher.notifications().is_empty());

        manager.handle_event(&make_muc_message_event(
            "dev@conference.example.com",
            "Hey @user, check this out",
            "m2",
        ));
        assert_eq!(dispatcher.notifications().len(), 1);
        assert_eq!(
            dispatcher.notifications()[0].title,
            "dev@conference.example.com"
        );
    }

    #[test]
    fn dispatch_failures_are_non_fatal() {
        let (manager, dispatcher) = make_manager(true);
        dispatcher.set_fail_dispatch(true);

        manager.handle_event(&make_message_event("alice@example.com", "Hello!", "m1"));
        manager.handle_event(&make_message_event(
            "alice@example.com",
            "Hello again!",
            "m2",
        ));
    }
}
