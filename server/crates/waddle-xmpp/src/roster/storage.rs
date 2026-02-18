//! Roster storage abstraction.
//!
//! This module provides a trait for roster persistence, allowing different
//! storage backends (database, in-memory, etc.) to be used interchangeably.

use std::future::Future;

use jid::BareJid;

use super::{RosterItem, RosterSetResult};
use crate::XmppError;

use super::{AskType, Subscription};

/// Trait for roster storage operations.
///
/// Implementors of this trait provide the backing store for user rosters.
/// This could be backed by a database, in-memory store, or other persistence layer.
pub trait RosterStorage: Send + Sync + 'static {
    /// Get all roster items for a user.
    ///
    /// Returns the complete roster for the given user JID.
    fn get_roster(
        &self,
        user_jid: &BareJid,
    ) -> impl Future<Output = Result<Vec<RosterItem>, XmppError>> + Send;

    /// Get a single roster item by JID.
    ///
    /// Returns the roster item if it exists, None otherwise.
    fn get_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> impl Future<Output = Result<Option<RosterItem>, XmppError>> + Send;

    /// Add or update a roster item.
    ///
    /// If the item already exists, it will be updated.
    /// If it doesn't exist, it will be created.
    ///
    /// Returns the result indicating whether the item was added or updated.
    fn set_roster_item(
        &self,
        user_jid: &BareJid,
        item: &RosterItem,
    ) -> impl Future<Output = Result<RosterSetResult, XmppError>> + Send;

    /// Remove a roster item.
    ///
    /// Returns Ok(true) if the item was removed, Ok(false) if it didn't exist.
    fn remove_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send;

    /// Get the current roster version for a user.
    ///
    /// Roster versioning (XEP-0237) allows clients to efficiently sync
    /// their roster by only receiving changes since a known version.
    ///
    /// Returns None if roster versioning is not supported or no version exists.
    fn get_roster_version(
        &self,
        user_jid: &BareJid,
    ) -> impl Future<Output = Result<Option<String>, XmppError>> + Send;

    /// Check if a roster item exists.
    fn has_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send;

    /// Update the subscription state for a roster item.
    ///
    /// Creates the roster item if it doesn't exist.
    /// Returns the updated roster item.
    fn update_subscription(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
        subscription: Subscription,
        ask: Option<AskType>,
    ) -> impl Future<Output = Result<RosterItem, XmppError>> + Send;

    /// Get all roster items where the user should send presence updates.
    ///
    /// Returns items with subscription=from or subscription=both.
    /// These are contacts who are subscribed to the user's presence.
    fn get_presence_subscribers(
        &self,
        user_jid: &BareJid,
    ) -> impl Future<Output = Result<Vec<BareJid>, XmppError>> + Send;

    /// Get all roster items where the user receives presence updates.
    ///
    /// Returns items with subscription=to or subscription=both.
    /// These are contacts whose presence the user is subscribed to.
    fn get_presence_subscriptions(
        &self,
        user_jid: &BareJid,
    ) -> impl Future<Output = Result<Vec<BareJid>, XmppError>> + Send;
}

/// In-memory roster storage for testing.
///
/// This implementation stores rosters in memory and is primarily
/// intended for testing purposes. For production use, a persistent
/// storage backend should be used.
#[cfg(test)]
pub mod test_storage {
    use super::*;
    use std::collections::HashMap;
    use std::sync::RwLock;

    /// In-memory roster storage for testing.
    pub struct InMemoryRosterStorage {
        rosters: RwLock<HashMap<BareJid, Vec<RosterItem>>>,
    }

    impl InMemoryRosterStorage {
        /// Create a new empty in-memory roster storage.
        pub fn new() -> Self {
            Self {
                rosters: RwLock::new(HashMap::new()),
            }
        }
    }

    impl Default for InMemoryRosterStorage {
        fn default() -> Self {
            Self::new()
        }
    }

    impl RosterStorage for InMemoryRosterStorage {
        async fn get_roster(&self, user_jid: &BareJid) -> Result<Vec<RosterItem>, XmppError> {
            let rosters = self.rosters.read().unwrap();
            Ok(rosters.get(user_jid).cloned().unwrap_or_default())
        }

        async fn get_roster_item(
            &self,
            user_jid: &BareJid,
            contact_jid: &BareJid,
        ) -> Result<Option<RosterItem>, XmppError> {
            let rosters = self.rosters.read().unwrap();
            Ok(rosters
                .get(user_jid)
                .and_then(|items| items.iter().find(|i| &i.jid == contact_jid).cloned()))
        }

        async fn set_roster_item(
            &self,
            user_jid: &BareJid,
            item: &RosterItem,
        ) -> Result<RosterSetResult, XmppError> {
            let mut rosters = self.rosters.write().unwrap();
            let roster = rosters.entry(user_jid.clone()).or_insert_with(Vec::new);

            // Check if item exists
            if let Some(existing) = roster.iter_mut().find(|i| i.jid == item.jid) {
                *existing = item.clone();
                Ok(RosterSetResult::Updated(item.clone()))
            } else {
                roster.push(item.clone());
                Ok(RosterSetResult::Added(item.clone()))
            }
        }

        async fn remove_roster_item(
            &self,
            user_jid: &BareJid,
            contact_jid: &BareJid,
        ) -> Result<bool, XmppError> {
            let mut rosters = self.rosters.write().unwrap();
            if let Some(roster) = rosters.get_mut(user_jid) {
                let initial_len = roster.len();
                roster.retain(|i| &i.jid != contact_jid);
                Ok(roster.len() < initial_len)
            } else {
                Ok(false)
            }
        }

        async fn get_roster_version(
            &self,
            _user_jid: &BareJid,
        ) -> Result<Option<String>, XmppError> {
            // In-memory storage doesn't support versioning
            Ok(None)
        }

        async fn has_roster_item(
            &self,
            user_jid: &BareJid,
            contact_jid: &BareJid,
        ) -> Result<bool, XmppError> {
            let rosters = self.rosters.read().unwrap();
            Ok(rosters
                .get(user_jid)
                .map(|items| items.iter().any(|i| &i.jid == contact_jid))
                .unwrap_or(false))
        }

        async fn update_subscription(
            &self,
            user_jid: &BareJid,
            contact_jid: &BareJid,
            subscription: Subscription,
            ask: Option<AskType>,
        ) -> Result<RosterItem, XmppError> {
            let mut rosters = self.rosters.write().unwrap();
            let roster = rosters.entry(user_jid.clone()).or_insert_with(Vec::new);

            // Find or create the roster item
            if let Some(existing) = roster.iter_mut().find(|i| &i.jid == contact_jid) {
                existing.subscription = subscription;
                existing.ask = ask;
                Ok(existing.clone())
            } else {
                let mut item = RosterItem::new(contact_jid.clone());
                item.subscription = subscription;
                item.ask = ask;
                roster.push(item.clone());
                Ok(item)
            }
        }

        async fn get_presence_subscribers(
            &self,
            user_jid: &BareJid,
        ) -> Result<Vec<BareJid>, XmppError> {
            let rosters = self.rosters.read().unwrap();
            Ok(rosters
                .get(user_jid)
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| {
                            matches!(i.subscription, Subscription::From | Subscription::Both)
                        })
                        .map(|i| i.jid.clone())
                        .collect()
                })
                .unwrap_or_default())
        }

        async fn get_presence_subscriptions(
            &self,
            user_jid: &BareJid,
        ) -> Result<Vec<BareJid>, XmppError> {
            let rosters = self.rosters.read().unwrap();
            Ok(rosters
                .get(user_jid)
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| matches!(i.subscription, Subscription::To | Subscription::Both))
                        .map(|i| i.jid.clone())
                        .collect()
                })
                .unwrap_or_default())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::roster::Subscription;

        #[tokio::test]
        async fn test_in_memory_storage_get_roster_empty() {
            let storage = InMemoryRosterStorage::new();
            let user: BareJid = "user@example.com".parse().unwrap();

            let roster = storage.get_roster(&user).await.unwrap();
            assert!(roster.is_empty());
        }

        #[tokio::test]
        async fn test_in_memory_storage_add_item() {
            let storage = InMemoryRosterStorage::new();
            let user: BareJid = "user@example.com".parse().unwrap();
            let contact: BareJid = "contact@example.com".parse().unwrap();

            let item = RosterItem::with_name(contact.clone(), "Alice")
                .set_subscription(Subscription::Both);

            let result = storage.set_roster_item(&user, &item).await.unwrap();
            assert!(matches!(result, RosterSetResult::Added(_)));

            let roster = storage.get_roster(&user).await.unwrap();
            assert_eq!(roster.len(), 1);
            assert_eq!(roster[0].jid, contact);
            assert_eq!(roster[0].name, Some("Alice".to_string()));
        }

        #[tokio::test]
        async fn test_in_memory_storage_update_item() {
            let storage = InMemoryRosterStorage::new();
            let user: BareJid = "user@example.com".parse().unwrap();
            let contact: BareJid = "contact@example.com".parse().unwrap();

            let item1 = RosterItem::with_name(contact.clone(), "Alice");
            storage.set_roster_item(&user, &item1).await.unwrap();

            let item2 = RosterItem::with_name(contact.clone(), "Alice Updated")
                .set_subscription(Subscription::Both);
            let result = storage.set_roster_item(&user, &item2).await.unwrap();
            assert!(matches!(result, RosterSetResult::Updated(_)));

            let roster = storage.get_roster(&user).await.unwrap();
            assert_eq!(roster.len(), 1);
            assert_eq!(roster[0].name, Some("Alice Updated".to_string()));
            assert_eq!(roster[0].subscription, Subscription::Both);
        }

        #[tokio::test]
        async fn test_in_memory_storage_remove_item() {
            let storage = InMemoryRosterStorage::new();
            let user: BareJid = "user@example.com".parse().unwrap();
            let contact: BareJid = "contact@example.com".parse().unwrap();

            let item = RosterItem::new(contact.clone());
            storage.set_roster_item(&user, &item).await.unwrap();

            assert!(storage.has_roster_item(&user, &contact).await.unwrap());

            let removed = storage.remove_roster_item(&user, &contact).await.unwrap();
            assert!(removed);

            assert!(!storage.has_roster_item(&user, &contact).await.unwrap());
        }

        #[tokio::test]
        async fn test_in_memory_storage_remove_nonexistent() {
            let storage = InMemoryRosterStorage::new();
            let user: BareJid = "user@example.com".parse().unwrap();
            let contact: BareJid = "contact@example.com".parse().unwrap();

            let removed = storage.remove_roster_item(&user, &contact).await.unwrap();
            assert!(!removed);
        }

        #[tokio::test]
        async fn test_in_memory_storage_get_single_item() {
            let storage = InMemoryRosterStorage::new();
            let user: BareJid = "user@example.com".parse().unwrap();
            let contact1: BareJid = "contact1@example.com".parse().unwrap();
            let contact2: BareJid = "contact2@example.com".parse().unwrap();

            storage
                .set_roster_item(&user, &RosterItem::with_name(contact1.clone(), "Alice"))
                .await
                .unwrap();
            storage
                .set_roster_item(&user, &RosterItem::with_name(contact2.clone(), "Bob"))
                .await
                .unwrap();

            let item = storage.get_roster_item(&user, &contact1).await.unwrap();
            assert!(item.is_some());
            assert_eq!(item.unwrap().name, Some("Alice".to_string()));

            let nonexistent: BareJid = "nobody@example.com".parse().unwrap();
            let item = storage.get_roster_item(&user, &nonexistent).await.unwrap();
            assert!(item.is_none());
        }
    }
}
