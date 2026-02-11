use std::sync::Arc;

use tracing::{debug, error, warn};

use waddle_core::event::{Channel, Event, EventPayload, EventSource, RosterItem, Subscription};
use waddle_storage::{Database, FromRow, Row, SqlValue, StorageError};

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

#[derive(Debug, thiserror::Error)]
pub enum RosterError {
    #[error("roster fetch failed: {0}")]
    FetchFailed(String),

    #[error("roster set failed for {jid}: {reason}")]
    SetFailed { jid: String, reason: String },

    #[error("contact not found: {0}")]
    ContactNotFound(String),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("event bus error: {0}")]
    EventBus(String),
}

struct StoredRosterItem {
    jid: String,
    name: Option<String>,
    subscription: String,
    groups: Option<String>,
}

impl FromRow for StoredRosterItem {
    fn from_row(row: &Row) -> Result<Self, StorageError> {
        let jid = match row.get(0) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => return Err(StorageError::QueryFailed("missing jid column".to_string())),
        };
        let name = match row.get(1) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            Some(SqlValue::Null) | None => None,
            _ => None,
        };
        let subscription = match row.get(2) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing subscription column".to_string(),
                ));
            }
        };
        let groups = match row.get(3) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            Some(SqlValue::Null) | None => None,
            _ => None,
        };
        Ok(StoredRosterItem {
            jid,
            name,
            subscription,
            groups,
        })
    }
}

impl StoredRosterItem {
    fn into_roster_item(self) -> RosterItem {
        let groups: Vec<String> = self
            .groups
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        RosterItem {
            jid: self.jid,
            name: self.name,
            subscription: self.subscription.parse::<Subscription>().unwrap(),
            groups,
        }
    }
}

pub struct RosterManager<D: Database> {
    db: Arc<D>,
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl<D: Database> RosterManager<D> {
    #[cfg(feature = "native")]
    pub fn new(db: Arc<D>, event_bus: Arc<dyn EventBus>) -> Self {
        Self { db, event_bus }
    }

    pub async fn get_roster(&self) -> Result<Vec<RosterItem>, RosterError> {
        let rows: Vec<StoredRosterItem> = self
            .db
            .query(
                "SELECT jid, name, subscription, groups FROM roster ORDER BY jid",
                &[],
            )
            .await?;
        Ok(rows.into_iter().map(|r| r.into_roster_item()).collect())
    }

    pub async fn add_contact(
        &self,
        jid: &str,
        name: Option<&str>,
        groups: &[String],
    ) -> Result<(), RosterError> {
        let groups_json = serde_json::to_string(groups).map_err(|e| RosterError::SetFailed {
            jid: jid.to_string(),
            reason: e.to_string(),
        })?;
        let sub = Subscription::None.as_str().to_string();
        let jid_s = jid.to_string();
        let name_s = name.map(|s| s.to_string());
        self.db
            .execute(
                "INSERT OR REPLACE INTO roster (jid, name, subscription, groups) VALUES (?1, ?2, ?3, ?4)",
                &[&jid_s, &name_s, &sub, &groups_json],
            )
            .await?;

        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.roster.add").unwrap(),
                EventSource::System("roster".into()),
                EventPayload::RosterAddRequested {
                    jid: jid.to_string(),
                    name: name.map(String::from),
                    groups: groups.to_vec(),
                },
            ));
        }

        Ok(())
    }

    pub async fn remove_contact(&self, jid: &str) -> Result<(), RosterError> {
        let jid_s = jid.to_string();
        let affected = self
            .db
            .execute("DELETE FROM roster WHERE jid = ?1", &[&jid_s])
            .await?;
        if affected == 0 {
            return Err(RosterError::ContactNotFound(jid.to_string()));
        }

        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.roster.remove").unwrap(),
                EventSource::System("roster".into()),
                EventPayload::RosterRemoveRequested {
                    jid: jid.to_string(),
                },
            ));
        }

        Ok(())
    }

    pub async fn update_contact(
        &self,
        jid: &str,
        name: Option<&str>,
        groups: &[String],
    ) -> Result<(), RosterError> {
        let jid_s = jid.to_string();

        // Verify contact exists
        let existing: Result<StoredRosterItem, _> = self
            .db
            .query_one(
                "SELECT jid, name, subscription, groups FROM roster WHERE jid = ?1",
                &[&jid_s],
            )
            .await;

        if existing.is_err() {
            return Err(RosterError::ContactNotFound(jid.to_string()));
        }

        let groups_json = serde_json::to_string(groups).map_err(|e| RosterError::SetFailed {
            jid: jid.to_string(),
            reason: e.to_string(),
        })?;
        let name_s = name.map(|s| s.to_string());
        self.db
            .execute(
                "UPDATE roster SET name = ?1, groups = ?2 WHERE jid = ?3",
                &[&name_s, &groups_json, &jid_s],
            )
            .await?;

        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.roster.update").unwrap(),
                EventSource::System("roster".into()),
                EventPayload::RosterUpdateRequested {
                    jid: jid.to_string(),
                    name: name.map(String::from),
                    groups: groups.to_vec(),
                },
            ));
        }

        Ok(())
    }

    pub async fn approve_subscription(&self, jid: &str) -> Result<(), RosterError> {
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.subscription.respond").unwrap(),
                EventSource::System("roster".into()),
                EventPayload::SubscriptionRespondRequested {
                    jid: jid.to_string(),
                    accept: true,
                },
            ));
        }
        Ok(())
    }

    pub async fn deny_subscription(&self, jid: &str) -> Result<(), RosterError> {
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.subscription.respond").unwrap(),
                EventSource::System("roster".into()),
                EventPayload::SubscriptionRespondRequested {
                    jid: jid.to_string(),
                    accept: false,
                },
            ));
        }
        Ok(())
    }

    async fn upsert_item(&self, item: &RosterItem) -> Result<(), RosterError> {
        let groups_json =
            serde_json::to_string(&item.groups).map_err(|e| RosterError::SetFailed {
                jid: item.jid.clone(),
                reason: e.to_string(),
            })?;
        let sub = item.subscription.as_str().to_string();
        self.db
            .execute(
                "INSERT OR REPLACE INTO roster (jid, name, subscription, groups) VALUES (?1, ?2, ?3, ?4)",
                &[&item.jid, &item.name, &sub, &groups_json],
            )
            .await?;
        Ok(())
    }

    async fn delete_item(&self, jid: &str) -> Result<(), RosterError> {
        let jid_s = jid.to_string();
        self.db
            .execute("DELETE FROM roster WHERE jid = ?1", &[&jid_s])
            .await?;
        Ok(())
    }

    async fn replace_all(&self, items: &[RosterItem]) -> Result<(), RosterError> {
        self.db.execute("DELETE FROM roster", &[]).await?;
        for item in items {
            self.upsert_item(item).await?;
        }
        Ok(())
    }

    #[cfg(feature = "native")]
    fn request_roster_fetch(&self) {
        let _ = self.event_bus.publish(Event::new(
            Channel::new("ui.roster.fetch").unwrap(),
            EventSource::System("roster".into()),
            EventPayload::RosterFetchRequested,
        ));
    }

    #[cfg(feature = "native")]
    pub async fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::ConnectionEstablished { .. } => {
                debug!("connection established, requesting roster fetch");
                self.request_roster_fetch();
            }
            EventPayload::RosterReceived { items } => {
                debug!(count = items.len(), "full roster received, persisting");
                if let Err(e) = self.replace_all(items).await {
                    error!(error = %e, "failed to persist roster");
                }
            }
            EventPayload::RosterUpdated { item } => {
                debug!(jid = %item.jid, "roster item updated, persisting");
                if let Err(e) = self.upsert_item(item).await {
                    error!(error = %e, jid = %item.jid, "failed to persist roster update");
                }
            }
            EventPayload::RosterRemoved { jid } => {
                debug!(jid = %jid, "roster item removed, deleting from storage");
                if let Err(e) = self.delete_item(jid).await {
                    error!(error = %e, jid = %jid, "failed to delete roster item");
                }
            }
            EventPayload::SubscriptionApproved { jid } => {
                debug!(jid = %jid, "subscription approved");
                // The server will push an updated roster item with the new subscription.
                // We don't need to update storage here; the roster push will do that.
            }
            EventPayload::SubscriptionRevoked { jid } => {
                debug!(jid = %jid, "subscription revoked");
                // Same: wait for the roster push from the server.
            }
            _ => {}
        }
    }

    #[cfg(feature = "native")]
    pub async fn run(self: Arc<Self>) -> Result<(), RosterError> {
        let mut sub = self
            .event_bus
            .subscribe("{system,xmpp}.**")
            .map_err(|e| RosterError::EventBus(e.to_string()))?;

        loop {
            match sub.recv().await {
                Ok(event) => {
                    self.handle_event(&event).await;
                }
                Err(waddle_core::error::EventBusError::ChannelClosed) => {
                    debug!("event bus closed, roster manager stopping");
                    return Ok(());
                }
                Err(waddle_core::error::EventBusError::Lagged(count)) => {
                    warn!(count, "roster manager lagged, some events dropped");
                }
                Err(e) => {
                    error!(error = %e, "roster manager subscription error");
                    return Err(RosterError::EventBus(e.to_string()));
                }
            }
        }
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use waddle_core::event::{BroadcastEventBus, EventBus};

    async fn setup() -> (
        Arc<RosterManager<impl Database>>,
        Arc<dyn EventBus>,
        TempDir,
    ) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = waddle_storage::open_database(&db_path)
            .await
            .expect("failed to open database");
        let db = Arc::new(db);
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());
        let manager = Arc::new(RosterManager::new(db, event_bus.clone()));
        (manager, event_bus, dir)
    }

    #[tokio::test]
    async fn get_roster_empty() {
        let (manager, _, _dir) = setup().await;
        let items = manager.get_roster().await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn add_and_get_contact() {
        let (manager, _, _dir) = setup().await;
        manager
            .add_contact("alice@example.com", Some("Alice"), &["Friends".to_string()])
            .await
            .unwrap();

        let items = manager.get_roster().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].jid, "alice@example.com");
        assert_eq!(items[0].name, Some("Alice".to_string()));
        assert!(matches!(items[0].subscription, Subscription::None));
        assert_eq!(items[0].groups, vec!["Friends"]);
    }

    #[tokio::test]
    async fn add_contact_no_name_no_groups() {
        let (manager, _, _dir) = setup().await;
        manager
            .add_contact("bob@example.com", None, &[])
            .await
            .unwrap();

        let items = manager.get_roster().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].jid, "bob@example.com");
        assert_eq!(items[0].name, None);
        assert!(items[0].groups.is_empty());
    }

    #[tokio::test]
    async fn remove_contact_succeeds() {
        let (manager, _, _dir) = setup().await;
        manager
            .add_contact("alice@example.com", Some("Alice"), &[])
            .await
            .unwrap();
        manager.remove_contact("alice@example.com").await.unwrap();

        let items = manager.get_roster().await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn remove_nonexistent_contact_returns_error() {
        let (manager, _, _dir) = setup().await;
        let result = manager.remove_contact("nobody@example.com").await;
        assert!(matches!(result, Err(RosterError::ContactNotFound(_))));
    }

    #[tokio::test]
    async fn update_contact_succeeds() {
        let (manager, _, _dir) = setup().await;
        manager
            .add_contact("alice@example.com", Some("Alice"), &["Friends".to_string()])
            .await
            .unwrap();
        manager
            .update_contact("alice@example.com", Some("Alice W"), &["Work".to_string()])
            .await
            .unwrap();

        let items = manager.get_roster().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, Some("Alice W".to_string()));
        assert_eq!(items[0].groups, vec!["Work"]);
    }

    #[tokio::test]
    async fn update_nonexistent_contact_returns_error() {
        let (manager, _, _dir) = setup().await;
        let result = manager
            .update_contact("nobody@example.com", Some("X"), &[])
            .await;
        assert!(matches!(result, Err(RosterError::ContactNotFound(_))));
    }

    #[tokio::test]
    async fn handle_roster_received_persists_items() {
        let (manager, _, _dir) = setup().await;
        let items = vec![
            RosterItem {
                jid: "alice@example.com".to_string(),
                name: Some("Alice".to_string()),
                subscription: Subscription::Both,
                groups: vec!["Friends".to_string()],
            },
            RosterItem {
                jid: "bob@example.com".to_string(),
                name: None,
                subscription: Subscription::To,
                groups: vec![],
            },
        ];

        let event = Event::new(
            Channel::new("xmpp.roster.received").unwrap(),
            EventSource::Xmpp,
            EventPayload::RosterReceived { items },
        );
        manager.handle_event(&event).await;

        let stored = manager.get_roster().await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].jid, "alice@example.com");
        assert!(matches!(stored[0].subscription, Subscription::Both));
        assert_eq!(stored[1].jid, "bob@example.com");
        assert!(matches!(stored[1].subscription, Subscription::To));
    }

    #[tokio::test]
    async fn handle_roster_received_replaces_existing() {
        let (manager, _, _dir) = setup().await;

        // Add initial items
        manager
            .add_contact("old@example.com", Some("Old"), &[])
            .await
            .unwrap();

        // Receive a new full roster that doesn't include the old contact
        let items = vec![RosterItem {
            jid: "new@example.com".to_string(),
            name: Some("New".to_string()),
            subscription: Subscription::None,
            groups: vec![],
        }];

        let event = Event::new(
            Channel::new("xmpp.roster.received").unwrap(),
            EventSource::Xmpp,
            EventPayload::RosterReceived { items },
        );
        manager.handle_event(&event).await;

        let stored = manager.get_roster().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].jid, "new@example.com");
    }

    #[tokio::test]
    async fn handle_roster_updated_upserts_item() {
        let (manager, _, _dir) = setup().await;

        let item = RosterItem {
            jid: "alice@example.com".to_string(),
            name: Some("Alice".to_string()),
            subscription: Subscription::Both,
            groups: vec!["Friends".to_string()],
        };

        let event = Event::new(
            Channel::new("xmpp.roster.updated").unwrap(),
            EventSource::Xmpp,
            EventPayload::RosterUpdated { item },
        );
        manager.handle_event(&event).await;

        let stored = manager.get_roster().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].jid, "alice@example.com");
        assert!(matches!(stored[0].subscription, Subscription::Both));
    }

    #[tokio::test]
    async fn handle_roster_removed_deletes_item() {
        let (manager, _, _dir) = setup().await;
        manager
            .add_contact("alice@example.com", Some("Alice"), &[])
            .await
            .unwrap();

        let event = Event::new(
            Channel::new("xmpp.roster.removed").unwrap(),
            EventSource::Xmpp,
            EventPayload::RosterRemoved {
                jid: "alice@example.com".to_string(),
            },
        );
        manager.handle_event(&event).await;

        let stored = manager.get_roster().await.unwrap();
        assert!(stored.is_empty());
    }

    #[tokio::test]
    async fn handle_connection_established_emits_fetch() {
        let (manager, event_bus, _dir) = setup().await;

        let mut sub = event_bus.subscribe("ui.**").unwrap();

        let event = Event::new(
            Channel::new("system.connection.established").unwrap(),
            EventSource::System("connection".into()),
            EventPayload::ConnectionEstablished {
                jid: "user@example.com".to_string(),
            },
        );
        manager.handle_event(&event).await;

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::RosterFetchRequested
        ));
    }

    #[tokio::test]
    async fn multiple_groups_round_trip() {
        let (manager, _, _dir) = setup().await;
        let groups = vec![
            "Friends".to_string(),
            "Work".to_string(),
            "Family".to_string(),
        ];
        manager
            .add_contact("alice@example.com", Some("Alice"), &groups)
            .await
            .unwrap();

        let items = manager.get_roster().await.unwrap();
        assert_eq!(items[0].groups, groups);
    }

    #[tokio::test]
    async fn subscription_states_round_trip() {
        let (manager, _, _dir) = setup().await;
        let states = vec![
            ("alice@example.com", Subscription::None),
            ("bob@example.com", Subscription::To),
            ("carol@example.com", Subscription::From),
            ("dave@example.com", Subscription::Both),
        ];

        for (jid, sub) in &states {
            let item = RosterItem {
                jid: jid.to_string(),
                name: None,
                subscription: sub.clone(),
                groups: vec![],
            };
            let event = Event::new(
                Channel::new("xmpp.roster.updated").unwrap(),
                EventSource::Xmpp,
                EventPayload::RosterUpdated { item },
            );
            manager.handle_event(&event).await;
        }

        let stored = manager.get_roster().await.unwrap();
        assert_eq!(stored.len(), 4);
        for (i, (jid, sub)) in states.iter().enumerate() {
            assert_eq!(stored[i].jid, *jid);
            assert_eq!(stored[i].subscription, *sub);
        }
    }
}
