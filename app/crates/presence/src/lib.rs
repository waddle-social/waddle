use std::collections::HashMap;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use tracing::{debug, error, warn};

use waddle_core::event::{Event, EventPayload, PresenceShow};

#[cfg(feature = "native")]
use std::sync::Arc;
#[cfg(feature = "native")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "native")]
use waddle_core::event::{Channel, EventBus, EventSource};

#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    #[error("failed to send presence: {0}")]
    SendFailed(String),

    #[error("invalid priority value: {0} (must be -128..127)")]
    InvalidPriority(i16),

    #[error("event bus error: {0}")]
    EventBus(String),
}

#[derive(Debug, Clone)]
pub struct PresenceInfo {
    pub jid: String,
    pub show: PresenceShow,
    pub status: Option<String>,
    pub priority: i8,
    pub last_updated: DateTime<Utc>,
}

impl PresenceInfo {
    fn unavailable(jid: &str) -> Self {
        Self {
            jid: jid.to_string(),
            show: PresenceShow::Unavailable,
            status: None,
            priority: 0,
            last_updated: Utc::now(),
        }
    }
}

/// Per-resource presence map for a single bare JID.
type ResourceMap = HashMap<String, PresenceInfo>;

pub struct PresenceManager {
    own_presence: RwLock<PresenceInfo>,
    /// Bare JID -> (resource -> PresenceInfo)
    contacts: RwLock<HashMap<String, ResourceMap>>,
    #[cfg(feature = "native")]
    awaiting_initial_presence: AtomicBool,
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl PresenceManager {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self {
            own_presence: RwLock::new(PresenceInfo {
                jid: String::new(),
                show: PresenceShow::Unavailable,
                status: None,
                priority: 0,
                last_updated: Utc::now(),
            }),
            contacts: RwLock::new(HashMap::new()),
            awaiting_initial_presence: AtomicBool::new(false),
            event_bus,
        }
    }

    pub fn own_presence(&self) -> PresenceInfo {
        self.own_presence.read().unwrap().clone()
    }

    /// Get the current presence of a JID. Returns the highest-priority
    /// resource's presence, or Unavailable if no presence is known.
    pub fn get_presence(&self, jid: &str) -> PresenceInfo {
        let bare = bare_jid(jid);
        let contacts = self.contacts.read().unwrap();
        match contacts.get(&bare) {
            Some(resources) => best_presence(&bare, resources),
            None => PresenceInfo::unavailable(&bare),
        }
    }

    #[cfg(feature = "native")]
    pub fn set_own_presence(
        &self,
        show: PresenceShow,
        status: Option<&str>,
        priority: Option<i8>,
    ) -> Result<(), PresenceError> {
        {
            let mut own = self.own_presence.write().unwrap();
            own.show = show.clone();
            own.status = status.map(String::from);
            if let Some(p) = priority {
                own.priority = p;
            }
            own.last_updated = Utc::now();
        }

        let _ = self.event_bus.publish(Event::new(
            Channel::new("ui.presence.set").unwrap(),
            EventSource::System("presence".into()),
            EventPayload::PresenceSetRequested {
                show,
                status: status.map(String::from),
            },
        ));

        Ok(())
    }

    #[cfg(feature = "native")]
    pub async fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::ConnectionEstablished { jid } => {
                debug!(jid = %jid, "connection established, waiting for roster before initial presence");
                {
                    let mut own = self.own_presence.write().unwrap();
                    own.jid = jid.clone();
                    own.show = PresenceShow::Unavailable;
                    own.status = None;
                    own.priority = 0;
                    own.last_updated = Utc::now();
                }
                self.contacts.write().unwrap().clear();
                self.awaiting_initial_presence
                    .store(true, Ordering::Relaxed);
            }
            EventPayload::RosterReceived { .. } => {
                if !self
                    .awaiting_initial_presence
                    .swap(false, Ordering::Relaxed)
                {
                    return;
                }

                debug!("roster received, sending initial presence");
                {
                    let mut own = self.own_presence.write().unwrap();
                    own.show = PresenceShow::Available;
                    own.status = None;
                    own.priority = 0;
                    own.last_updated = Utc::now();
                }
                self.send_initial_presence();
            }
            EventPayload::ConnectionLost { .. } => {
                debug!("connection lost, sending unavailable and clearing presence map");
                self.awaiting_initial_presence
                    .store(false, Ordering::Relaxed);
                self.send_unavailable_presence();
                self.contacts.write().unwrap().clear();
                {
                    let mut own = self.own_presence.write().unwrap();
                    own.show = PresenceShow::Unavailable;
                    own.status = None;
                    own.last_updated = Utc::now();
                }
            }
            EventPayload::PresenceChanged {
                jid,
                show,
                status,
                priority,
            } => {
                debug!(jid = %jid, ?show, priority, "contact presence changed");
                let bare = bare_jid(jid);
                let resource = resource_part(jid);
                let info = PresenceInfo {
                    jid: bare.clone(),
                    show: show.clone(),
                    status: status.clone(),
                    priority: *priority,
                    last_updated: Utc::now(),
                };
                let mut contacts = self.contacts.write().unwrap();
                let resources = contacts.entry(bare).or_default();
                if matches!(show, PresenceShow::Unavailable) {
                    resources.remove(&resource);
                } else {
                    resources.insert(resource, info);
                }
            }
            EventPayload::OwnPresenceChanged { show, status } => {
                debug!(?show, "own presence changed");
                let mut own = self.own_presence.write().unwrap();
                own.show = show.clone();
                own.status = status.clone();
                own.last_updated = Utc::now();
            }
            _ => {}
        }
    }

    #[cfg(feature = "native")]
    fn send_initial_presence(&self) {
        let _ = self.event_bus.publish(Event::new(
            Channel::new("ui.presence.set").unwrap(),
            EventSource::System("presence".into()),
            EventPayload::PresenceSetRequested {
                show: PresenceShow::Available,
                status: None,
            },
        ));
    }

    #[cfg(feature = "native")]
    fn send_unavailable_presence(&self) {
        let _ = self.event_bus.publish(Event::new(
            Channel::new("ui.presence.set").unwrap(),
            EventSource::System("presence".into()),
            EventPayload::PresenceSetRequested {
                show: PresenceShow::Unavailable,
                status: None,
            },
        ));
    }

    #[cfg(feature = "native")]
    pub async fn run(self: Arc<Self>) -> Result<(), PresenceError> {
        let mut sub = self
            .event_bus
            .subscribe("{system,xmpp}.**")
            .map_err(|e| PresenceError::EventBus(e.to_string()))?;

        loop {
            match sub.recv().await {
                Ok(event) => {
                    self.handle_event(&event).await;
                }
                Err(waddle_core::error::EventBusError::ChannelClosed) => {
                    debug!("event bus closed, presence manager stopping");
                    return Ok(());
                }
                Err(waddle_core::error::EventBusError::Lagged(count)) => {
                    warn!(count, "presence manager lagged, some events dropped");
                }
                Err(e) => {
                    error!(error = %e, "presence manager subscription error");
                    return Err(PresenceError::EventBus(e.to_string()));
                }
            }
        }
    }
}

/// Select the highest-priority resource's presence. Ties broken by most
/// recent update. Returns Unavailable if the resource map is empty.
fn best_presence(bare: &str, resources: &ResourceMap) -> PresenceInfo {
    resources
        .values()
        .max_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(a.last_updated.cmp(&b.last_updated))
        })
        .cloned()
        .unwrap_or_else(|| PresenceInfo::unavailable(bare))
}

fn bare_jid(jid: &str) -> String {
    match jid.find('/') {
        Some(pos) => jid[..pos].to_string(),
        None => jid.to_string(),
    }
}

fn resource_part(jid: &str) -> String {
    match jid.find('/') {
        Some(pos) => jid[pos + 1..].to_string(),
        None => String::new(),
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use waddle_core::event::{BroadcastEventBus, Channel, Event, EventBus, EventSource};

    fn make_manager() -> (Arc<PresenceManager>, Arc<dyn EventBus>) {
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());
        let manager = Arc::new(PresenceManager::new(event_bus.clone()));
        (manager, event_bus)
    }

    fn make_event(channel: &str, payload: EventPayload) -> Event {
        Event::new(
            Channel::new(channel).unwrap(),
            EventSource::System("test".into()),
            payload,
        )
    }

    fn presence_changed(
        jid: &str,
        show: PresenceShow,
        status: Option<&str>,
        priority: i8,
    ) -> EventPayload {
        EventPayload::PresenceChanged {
            jid: jid.to_string(),
            show,
            status: status.map(String::from),
            priority,
        }
    }

    #[tokio::test]
    async fn initial_own_presence_is_unavailable() {
        let (manager, _) = make_manager();
        let own = manager.own_presence();
        assert!(matches!(own.show, PresenceShow::Unavailable));
    }

    #[tokio::test]
    async fn unknown_contact_returns_unavailable() {
        let (manager, _) = make_manager();
        let info = manager.get_presence("unknown@example.com");
        assert!(matches!(info.show, PresenceShow::Unavailable));
        assert_eq!(info.jid, "unknown@example.com");
    }

    #[tokio::test]
    async fn connection_established_waits_for_roster_before_initial_presence() {
        let (manager, event_bus) = make_manager();
        let mut sub = event_bus.subscribe("ui.**").unwrap();

        let event = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "user@example.com".to_string(),
            },
        );
        manager.handle_event(&event).await;

        let own = manager.own_presence();
        assert!(matches!(own.show, PresenceShow::Unavailable));
        assert_eq!(own.jid, "user@example.com");

        let no_event = tokio::time::timeout(Duration::from_millis(100), sub.recv()).await;
        assert!(no_event.is_err(), "initial presence should wait for roster");

        let event = Event::new(
            Channel::new("xmpp.roster.received").unwrap(),
            EventSource::Xmpp,
            EventPayload::RosterReceived { items: Vec::new() },
        );
        manager.handle_event(&event).await;

        let own = manager.own_presence();
        assert!(matches!(own.show, PresenceShow::Available));

        let received = tokio::time::timeout(Duration::from_millis(200), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::PresenceSetRequested {
                show: PresenceShow::Available,
                status: None,
            }
        ));
    }

    #[tokio::test]
    async fn connection_lost_sends_unavailable_and_clears() {
        let (manager, event_bus) = make_manager();

        let event = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "user@example.com".to_string(),
            },
        );
        manager.handle_event(&event).await;

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Available,
                None,
                0,
            ),
        );
        manager.handle_event(&event).await;
        assert!(matches!(
            manager.get_presence("alice@example.com").show,
            PresenceShow::Available
        ));

        let mut sub = event_bus.subscribe("ui.**").unwrap();

        let event = make_event(
            "system.connection.lost",
            EventPayload::ConnectionLost {
                reason: "network error".to_string(),
                will_retry: true,
            },
        );
        manager.handle_event(&event).await;

        assert!(matches!(
            manager.own_presence().show,
            PresenceShow::Unavailable
        ));
        assert!(matches!(
            manager.get_presence("alice@example.com").show,
            PresenceShow::Unavailable
        ));

        let received = tokio::time::timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive unavailable event on disconnect");
        assert!(matches!(
            received.payload,
            EventPayload::PresenceSetRequested {
                show: PresenceShow::Unavailable,
                status: None,
            }
        ));
    }

    #[tokio::test]
    async fn presence_changed_updates_contact_map() {
        let (manager, _) = make_manager();

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Away,
                Some("brb"),
                0,
            ),
        );
        manager.handle_event(&event).await;

        let info = manager.get_presence("alice@example.com");
        assert!(matches!(info.show, PresenceShow::Away));
        assert_eq!(info.status, Some("brb".to_string()));
        assert_eq!(info.jid, "alice@example.com");
    }

    #[tokio::test]
    async fn presence_changed_resolves_bare_and_full_jid() {
        let (manager, _) = make_manager();

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed("bob@example.com/mobile", PresenceShow::Dnd, Some("busy"), 0),
        );
        manager.handle_event(&event).await;

        let info = manager.get_presence("bob@example.com");
        assert!(matches!(info.show, PresenceShow::Dnd));

        let info2 = manager.get_presence("bob@example.com/mobile");
        assert!(matches!(info2.show, PresenceShow::Dnd));
    }

    #[tokio::test]
    async fn multi_resource_returns_highest_priority() {
        let (manager, _) = make_manager();

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Available,
                None,
                5,
            ),
        );
        manager.handle_event(&event).await;

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/mobile",
                PresenceShow::Away,
                Some("on phone"),
                10,
            ),
        );
        manager.handle_event(&event).await;

        let info = manager.get_presence("alice@example.com");
        assert!(matches!(info.show, PresenceShow::Away));
        assert_eq!(info.status, Some("on phone".to_string()));
        assert_eq!(info.priority, 10);
    }

    #[tokio::test]
    async fn multi_resource_updates_one_resource() {
        let (manager, _) = make_manager();

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Available,
                None,
                10,
            ),
        );
        manager.handle_event(&event).await;

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed("alice@example.com/mobile", PresenceShow::Away, None, 5),
        );
        manager.handle_event(&event).await;

        // Desktop has higher priority
        let info = manager.get_presence("alice@example.com");
        assert!(matches!(info.show, PresenceShow::Available));
        assert_eq!(info.priority, 10);

        // Update desktop to lower priority
        let event = make_event(
            "xmpp.presence.changed",
            presence_changed("alice@example.com/desktop", PresenceShow::Dnd, None, 1),
        );
        manager.handle_event(&event).await;

        // Mobile now wins
        let info = manager.get_presence("alice@example.com");
        assert!(matches!(info.show, PresenceShow::Away));
        assert_eq!(info.priority, 5);
    }

    #[tokio::test]
    async fn unavailable_removes_resource() {
        let (manager, _) = make_manager();

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Available,
                None,
                10,
            ),
        );
        manager.handle_event(&event).await;

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed("alice@example.com/mobile", PresenceShow::Away, None, 5),
        );
        manager.handle_event(&event).await;

        // Desktop goes unavailable
        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Unavailable,
                None,
                0,
            ),
        );
        manager.handle_event(&event).await;

        // Mobile remains
        let info = manager.get_presence("alice@example.com");
        assert!(matches!(info.show, PresenceShow::Away));
        assert_eq!(info.priority, 5);

        // Mobile goes unavailable too
        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/mobile",
                PresenceShow::Unavailable,
                None,
                0,
            ),
        );
        manager.handle_event(&event).await;

        // No resources left = unavailable
        let info = manager.get_presence("alice@example.com");
        assert!(matches!(info.show, PresenceShow::Unavailable));
    }

    #[tokio::test]
    async fn own_presence_changed_updates_own_state() {
        let (manager, _) = make_manager();

        let event = Event::new(
            Channel::new("xmpp.presence.own_changed").unwrap(),
            EventSource::Xmpp,
            EventPayload::OwnPresenceChanged {
                show: PresenceShow::Dnd,
                status: Some("do not disturb".to_string()),
            },
        );
        manager.handle_event(&event).await;

        let own = manager.own_presence();
        assert!(matches!(own.show, PresenceShow::Dnd));
        assert_eq!(own.status, Some("do not disturb".to_string()));
    }

    #[tokio::test]
    async fn set_own_presence_emits_event() {
        let (manager, event_bus) = make_manager();
        let mut sub = event_bus.subscribe("ui.**").unwrap();

        manager
            .set_own_presence(PresenceShow::Away, Some("lunch"), None)
            .unwrap();

        let received = tokio::time::timeout(Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::PresenceSetRequested {
                show: PresenceShow::Away,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn set_own_presence_updates_local_state() {
        let (manager, _) = make_manager();

        manager
            .set_own_presence(PresenceShow::Xa, Some("vacation"), Some(5))
            .unwrap();

        let own = manager.own_presence();
        assert!(matches!(own.show, PresenceShow::Xa));
        assert_eq!(own.status, Some("vacation".to_string()));
        assert_eq!(own.priority, 5);
    }

    #[tokio::test]
    async fn multiple_contacts_tracked_independently() {
        let (manager, _) = make_manager();

        let contacts = vec![
            ("alice@example.com", PresenceShow::Available, None),
            ("bob@example.com", PresenceShow::Away, Some("brb")),
            ("carol@example.com", PresenceShow::Dnd, Some("busy")),
        ];

        for (jid, show, status) in &contacts {
            let event = make_event(
                "xmpp.presence.changed",
                presence_changed(jid, show.clone(), *status, 0),
            );
            manager.handle_event(&event).await;
        }

        let alice = manager.get_presence("alice@example.com");
        assert!(matches!(alice.show, PresenceShow::Available));
        assert_eq!(alice.status, None);

        let bob = manager.get_presence("bob@example.com");
        assert!(matches!(bob.show, PresenceShow::Away));
        assert_eq!(bob.status, Some("brb".to_string()));

        let carol = manager.get_presence("carol@example.com");
        assert!(matches!(carol.show, PresenceShow::Dnd));
        assert_eq!(carol.status, Some("busy".to_string()));
    }

    #[tokio::test]
    async fn presence_updates_overwrite_same_resource() {
        let (manager, _) = make_manager();

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Available,
                None,
                0,
            ),
        );
        manager.handle_event(&event).await;

        let event = make_event(
            "xmpp.presence.changed",
            presence_changed(
                "alice@example.com/desktop",
                PresenceShow::Away,
                Some("stepped out"),
                0,
            ),
        );
        manager.handle_event(&event).await;

        let info = manager.get_presence("alice@example.com");
        assert!(matches!(info.show, PresenceShow::Away));
        assert_eq!(info.status, Some("stepped out".to_string()));
    }

    #[tokio::test]
    async fn run_loop_processes_events() {
        let (manager, event_bus) = make_manager();

        let manager_clone = manager.clone();
        let handle = tokio::spawn(async move { manager_clone.run().await });

        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        event_bus
            .publish(Event::new(
                Channel::new("xmpp.presence.changed").unwrap(),
                EventSource::Xmpp,
                EventPayload::PresenceChanged {
                    jid: "test@example.com/laptop".to_string(),
                    show: PresenceShow::Chat,
                    status: Some("free to chat".to_string()),
                    priority: 0,
                },
            ))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let info = manager.get_presence("test@example.com");
        assert!(matches!(info.show, PresenceShow::Chat));
        assert_eq!(info.status, Some("free to chat".to_string()));

        handle.abort();
    }

    #[test]
    fn bare_jid_strips_resource() {
        assert_eq!(bare_jid("user@example.com/resource"), "user@example.com");
        assert_eq!(bare_jid("user@example.com"), "user@example.com");
        assert_eq!(bare_jid("user@example.com/res/extra"), "user@example.com");
    }

    #[test]
    fn resource_part_extracts_resource() {
        assert_eq!(resource_part("user@example.com/desktop"), "desktop");
        assert_eq!(resource_part("user@example.com"), "");
        assert_eq!(resource_part("user@example.com/res/extra"), "res/extra");
    }

    #[test]
    fn best_presence_picks_highest_priority() {
        let mut resources = HashMap::new();
        resources.insert(
            "desktop".to_string(),
            PresenceInfo {
                jid: "alice@example.com".to_string(),
                show: PresenceShow::Available,
                status: None,
                priority: 5,
                last_updated: Utc::now(),
            },
        );
        resources.insert(
            "mobile".to_string(),
            PresenceInfo {
                jid: "alice@example.com".to_string(),
                show: PresenceShow::Away,
                status: Some("on phone".to_string()),
                priority: 10,
                last_updated: Utc::now(),
            },
        );

        let best = best_presence("alice@example.com", &resources);
        assert!(matches!(best.show, PresenceShow::Away));
        assert_eq!(best.priority, 10);
    }

    #[test]
    fn best_presence_empty_returns_unavailable() {
        let resources = HashMap::new();
        let best = best_presence("alice@example.com", &resources);
        assert!(matches!(best.show, PresenceShow::Unavailable));
    }
}
