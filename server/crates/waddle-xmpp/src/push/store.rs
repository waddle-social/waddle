//! In-memory push subscription storage.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use dashmap::DashMap;

use super::{PushError, PushSubscription};

/// Trait for storing and retrieving push notification subscriptions.
pub trait PushSubscriptionStore: Send + Sync + 'static {
    /// Register a new push subscription.
    fn register(
        &self,
        sub: PushSubscription,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>>;
    /// Remove a push subscription.
    fn remove(
        &self,
        user_jid: &str,
        service_jid: &str,
        node: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>>;
    /// Get all subscriptions for a user.
    fn get_for_user(
        &self,
        user_jid: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PushSubscription>, PushError>> + Send + '_>>;
}

/// In-memory push subscription store backed by [`DashMap`].
#[derive(Debug, Clone)]
pub struct InMemoryPushStore {
    subs: Arc<DashMap<String, Vec<PushSubscription>>>,
}

impl Default for InMemoryPushStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryPushStore {
    /// Create a new empty in-memory push store.
    pub fn new() -> Self {
        Self {
            subs: Arc::new(DashMap::new()),
        }
    }
}

impl PushSubscriptionStore for InMemoryPushStore {
    fn register(
        &self,
        sub: PushSubscription,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
        let service_jid = sub.service_jid.clone();
        let node = sub.node.clone();
        let user_jid = sub.user_jid.clone();

        {
            let mut entry = self.subs.entry(user_jid).or_default();
            entry.retain(|s| !(s.service_jid == service_jid && s.node == node));
            entry.push(sub);
        }

        Box::pin(std::future::ready(Ok(())))
    }

    fn remove(
        &self,
        user_jid: &str,
        service_jid: &str,
        node: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + '_>> {
        let user_jid = user_jid.to_owned();
        let service_jid = service_jid.to_owned();
        let node = node.map(|s| s.to_owned());
        Box::pin(async move {
            if let Some(mut entry) = self.subs.get_mut(&user_jid) {
                entry.retain(|s| !(s.service_jid == service_jid && s.node == node));
            }
            Ok(())
        })
    }

    fn get_for_user(
        &self,
        user_jid: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PushSubscription>, PushError>> + Send + '_>> {
        let user_jid = user_jid.to_owned();
        Box::pin(async move {
            Ok(self
                .subs
                .get(&user_jid)
                .map(|e| e.clone())
                .unwrap_or_default())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sub(user: &str, service: &str, node: Option<&str>) -> PushSubscription {
        PushSubscription {
            user_jid: user.into(),
            service_jid: service.into(),
            node: node.map(|s| s.into()),
            endpoint: Some("https://push.example.com/ep".into()),
            p256dh: None,
            auth_key: None,
        }
    }

    #[tokio::test]
    async fn test_register_and_get() {
        let store = InMemoryPushStore::new();
        store
            .register(make_sub("alice@ex", "push.ex", Some("n1")))
            .await
            .expect("ok");
        let subs = store.get_for_user("alice@ex").await.expect("ok");
        assert_eq!(subs.len(), 1);
    }

    #[tokio::test]
    async fn test_register_replaces_same_key() {
        let store = InMemoryPushStore::new();
        store
            .register(make_sub("alice@ex", "push.ex", Some("n1")))
            .await
            .expect("ok");
        store
            .register(make_sub("alice@ex", "push.ex", Some("n1")))
            .await
            .expect("ok");
        assert_eq!(store.get_for_user("alice@ex").await.expect("ok").len(), 1);
    }

    #[tokio::test]
    async fn test_different_nodes_coexist() {
        let store = InMemoryPushStore::new();
        store
            .register(make_sub("alice@ex", "push.ex", Some("n1")))
            .await
            .expect("ok");
        store
            .register(make_sub("alice@ex", "push.ex", Some("n2")))
            .await
            .expect("ok");
        assert_eq!(store.get_for_user("alice@ex").await.expect("ok").len(), 2);
    }

    #[tokio::test]
    async fn test_remove() {
        let store = InMemoryPushStore::new();
        store
            .register(make_sub("alice@ex", "push.ex", Some("n1")))
            .await
            .expect("ok");
        store
            .remove("alice@ex", "push.ex", Some("n1"))
            .await
            .expect("ok");
        assert!(store.get_for_user("alice@ex").await.expect("ok").is_empty());
    }

    #[tokio::test]
    async fn test_remove_nonexistent_ok() {
        let store = InMemoryPushStore::new();
        assert!(store.remove("nobody@ex", "push.ex", None).await.is_ok());
    }

    #[tokio::test]
    async fn test_get_empty() {
        let store = InMemoryPushStore::new();
        assert!(store
            .get_for_user("nobody@ex")
            .await
            .expect("ok")
            .is_empty());
    }

    #[tokio::test]
    async fn test_user_isolation() {
        let store = InMemoryPushStore::new();
        store
            .register(make_sub("alice@ex", "push.ex", Some("n1")))
            .await
            .expect("ok");
        store
            .register(make_sub("bob@ex", "push.ex", Some("n1")))
            .await
            .expect("ok");
        assert_eq!(store.get_for_user("alice@ex").await.expect("ok").len(), 1);
        assert_eq!(store.get_for_user("bob@ex").await.expect("ok").len(), 1);
    }

    #[tokio::test]
    async fn test_register_same_key_concurrent_dedup() {
        let store = InMemoryPushStore::new();
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                store
                    .register(make_sub("alice@ex", "push.ex", Some("same-node")))
                    .await
                    .expect("ok");
            }));
        }

        for task in tasks {
            task.await.expect("task joined");
        }

        let subs = store.get_for_user("alice@ex").await.expect("ok");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].service_jid, "push.ex");
        assert_eq!(subs[0].node.as_deref(), Some("same-node"));
    }
}
