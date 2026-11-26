//! Tenant-keyed composite keys and thread-safe caches preventing cross-tenant
//! collisions when multiple tenants reference identical localparts or remote JIDs.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{PoisonError, RwLock};

use serde::{Deserialize, Serialize};

use crate::identity::TenantId;

/// Composite key binding any local or federated key `K` to a canonical [`TenantId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TenantKey<K> {
    pub tenant_id: TenantId,
    pub key: K,
}

impl<K> TenantKey<K> {
    pub fn new(tenant_id: TenantId, key: K) -> Self {
        Self { tenant_id, key }
    }
}

/// Thread-safe map keyed structurally by `(TenantId, K)`.
///
/// Every read, write, and invalidation requires an explicit [`TenantId`], so two
/// tenants referencing the same remote `BareJid` (`bob@remote.example`) or same
/// localpart (`alice`) cannot read, overwrite, or evict each other's entries.
#[derive(Debug)]
pub struct TenantScopedCache<K, V> {
    entries: RwLock<HashMap<TenantKey<K>, V>>,
}

impl<K, V> Default for TenantScopedCache<K, V>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> TenantScopedCache<K, V>
where
    K: Eq + Hash,
{
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, tenant_id: TenantId, key: K, value: V) -> Option<V> {
        let mut guard = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        guard.insert(TenantKey::new(tenant_id, key), value)
    }

    pub fn get_cloned(&self, tenant_id: &TenantId, key: &K) -> Option<V>
    where
        K: Clone,
        V: Clone,
    {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        guard
            .get(&TenantKey::new(tenant_id.clone(), key.clone()))
            .cloned()
    }

    pub fn with_entry<R>(&self, tenant_id: &TenantId, key: &K, f: impl FnOnce(Option<&V>) -> R) -> R
    where
        K: Clone,
    {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let lookup = TenantKey::new(tenant_id.clone(), key.clone());
        f(guard.get(&lookup))
    }

    pub fn get_or_insert_with(&self, tenant_id: TenantId, key: K, init: impl FnOnce() -> V) -> V
    where
        V: Clone,
    {
        let mut guard = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        guard
            .entry(TenantKey::new(tenant_id, key))
            .or_insert_with(init)
            .clone()
    }

    pub fn remove(&self, tenant_id: &TenantId, key: &K) -> Option<V>
    where
        K: Clone,
    {
        let mut guard = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        guard.remove(&TenantKey::new(tenant_id.clone(), key.clone()))
    }

    pub fn contains_key(&self, tenant_id: &TenantId, key: &K) -> bool
    where
        K: Clone,
    {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        guard.contains_key(&TenantKey::new(tenant_id.clone(), key.clone()))
    }

    /// Evict all cached entries belonging strictly to `tenant_id` (used on tenant
    /// quiesce, lease loss, or deletion) without affecting any sibling tenant.
    pub fn clear_tenant(&self, tenant_id: &TenantId) -> usize {
        let mut guard = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        let before = guard.len();
        guard.retain(|k, _| &k.tenant_id != tenant_id);
        before.saturating_sub(guard.len())
    }

    pub fn len_for_tenant(&self, tenant_id: &TenantId) -> usize {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        guard.keys().filter(|k| &k.tenant_id == tenant_id).count()
    }

    pub fn total_len(&self) -> usize {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        guard.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolates_identical_remote_keys_across_tenants() {
        let cache: TenantScopedCache<String, u32> = TenantScopedCache::new();
        let tenant_a = TenantId::parse("tenant-a.example").expect("valid");
        let tenant_b = TenantId::parse("tenant-b.example").expect("valid");
        let remote_jid = "bob@remote.example".to_owned();

        cache.insert(tenant_a.clone(), remote_jid.clone(), 10);
        cache.insert(tenant_b.clone(), remote_jid.clone(), 20);

        assert_eq!(cache.get_cloned(&tenant_a, &remote_jid), Some(10));
        assert_eq!(cache.get_cloned(&tenant_b, &remote_jid), Some(20));

        assert_eq!(cache.clear_tenant(&tenant_a), 1);
        assert_eq!(cache.get_cloned(&tenant_a, &remote_jid), None);
        assert_eq!(cache.get_cloned(&tenant_b, &remote_jid), Some(20));
        assert_eq!(cache.len_for_tenant(&tenant_a), 0);
        assert_eq!(cache.len_for_tenant(&tenant_b), 1);
    }
}
