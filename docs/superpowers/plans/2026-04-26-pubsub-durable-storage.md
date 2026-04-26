# Durable SQL-backed PubSub/PEP Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make XEP-0060 (PubSub) and XEP-0163 (PEP) state durable across server restarts, close existing perf hotspots, and add the missing subscription/affiliation/purge/configure surfaces. Closes [waddle#207](https://github.com/waddle-social/waddle/issues/207).

**Architecture:** Extend `PubSubStorage` trait with subscriptions/affiliations/purge primitives. Rewrite `pubsub_items` schema to use `published_at_ms INTEGER` + monotonic `seq` (per-driver autoincrement). Keep `DatabasePubSubStorage` as the production path; preserve `InMemoryPubSubStorage` only for tests/explicit dev mode (env-var gated). Layer XMPP semantics (access-model enforcement, owner derivation) in a new `pubsub_authz` module composing data primitives. Migrations are drop-and-recreate gated by a `pubsub_schema_version` row — CLAUDE.md greenlights breaking changes since there is no production data.

**Tech Stack:** Rust 2021, sqlx (sqlite + postgres), `xmpp_parsers`, `minidom`, `jid`, `chrono`, `uuid`, `dashmap` (existing), `tokio`, `async_trait`. Tests: `tokio::test`, file-backed sqlite for restart, and the existing `tests/ws_common` harness for L3 wire-conformance.

---

## File Structure

**New files:**
- `server/crates/waddle-server/src/time.rs` — `now_ms()` helper.
- `server/crates/waddle-xmpp-core/src/pubsub/subscription.rs` — `SubId`, `SubscriptionState`, `Subscription` typed payloads.
- `server/crates/waddle-xmpp-core/src/pubsub/affiliation.rs` — `Affiliation` enum, `Display`/`FromStr`.
- `server/crates/waddle-server/src/pubsub_authz.rs` — `can_subscribe`, `can_publish`, `derive_owner_affiliation`, `effective_affiliation`.
- `server/crates/waddle-server/tests/xep0060_pubsub_ws.rs` — L3 XEP-0060 wire conformance.
- `server/crates/waddle-server/tests/xep0163_pep_ws.rs` — L3 XEP-0163 wire conformance.

**Modified files:**
- `server/crates/waddle-xmpp/src/pubsub/storage.rs` — new trait methods + `InMemoryPubSubStorage` impl.
- `server/crates/waddle-xmpp/src/pubsub/mod.rs` — re-exports.
- `server/crates/waddle-xmpp-core/src/pubsub/mod.rs` — module wiring.
- `server/crates/waddle-xmpp-core/src/pubsub/stanzas.rs` — extend parser/builders for Purge, ConfigureSet, Affiliations get/set, subscribe-result `subid`.
- `server/crates/waddle-server/src/pubsub.rs` — schema rewrite, new methods, drop-and-recreate migration, env-var gated in-memory fallback.
- `server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs` — wire new handlers; migrate Subscribe/Unsubscribe to storage; access-model enforcement.
- `server/crates/waddle-server/src/server/routes/websocket/mod.rs` — drop `pubsub_subscriptions: DashSet` field.
- `server/crates/waddle-server/src/server/mod.rs` — drop the `DashSet::new()` initializer; update `build_pubsub_storage` signature.
- `server/crates/waddle-server/src/lib.rs` — declare new `time` and `pubsub_authz` modules.
- `infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml` — add `WADDLE_XMPP_PUBSUB_DATABASE_URL` env var pointing at the existing postgres cluster.

---

## Conventions

**Commit style:** `feat(server): ...` / `fix(server): ...` per CLAUDE.md. Single scope. Each task ends with one commit.

**Test style:** TDD. For every new public API, write the failing test first, run it to confirm failure, implement, run to confirm pass, then commit. For pure refactors that delete code (e.g., DashSet removal), the existing test suite is the safety net.

**Type discipline:** No new `String` payload fields on events/messages/traits; all protocol data uses `BareJid`/`Jid`/`Affiliation`/`SubscriptionState`/`SubId` etc. per CLAUDE.md typed-payloads rule.

**XML discipline:** Use `xmpp_parsers` and `minidom::Element` builders only. Never `format!`/`println!` for XML.

---

## Task 1: Time helper

**Files:**
- Create: `server/crates/waddle-server/src/time.rs`
- Modify: `server/crates/waddle-server/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to `server/crates/waddle-server/src/time.rs`:

```rust
//! Wall-clock time helpers shared across the server crate.

/// Current Unix time in milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_is_monotonic_within_call() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a, "expected b >= a, got a={a} b={b}");
    }

    #[test]
    fn now_ms_is_in_the_present() {
        let value = now_ms();
        // Anything before 2020-01-01 or after 2100-01-01 is wrong.
        assert!(value > 1_577_836_800_000, "before 2020: {value}");
        assert!(value < 4_102_444_800_000, "after 2100: {value}");
    }
}
```

Add the module declaration to `server/crates/waddle-server/src/lib.rs` (look for the existing `pub mod` list):

```rust
pub mod time;
```

- [ ] **Step 2: Run tests to verify they pass**

```bash
cd server && cargo test --package waddle-server --lib time::tests
```

Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-server/src/time.rs server/crates/waddle-server/src/lib.rs
git commit -m "feat(server): add now_ms time helper for pubsub storage"
```

---

## Task 2: Typed `SubId`, `SubscriptionState`, `Subscription`

**Files:**
- Create: `server/crates/waddle-xmpp-core/src/pubsub/subscription.rs`
- Modify: `server/crates/waddle-xmpp-core/src/pubsub/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `server/crates/waddle-xmpp-core/src/pubsub/subscription.rs`:

```rust
//! XEP-0060 subscription typed payloads.

use std::{fmt, str::FromStr};

use jid::Jid;
use serde::{Deserialize, Serialize};

/// Opaque subscription identifier (XEP-0060 §6.1.6).
///
/// Generated by the service. Treated as opaque on the wire; internally a UUID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubId(String);

impl SubId {
    /// Generate a fresh subid backed by a v4 UUID.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Wrap an existing identifier (used by storage when reading rows back).
    pub fn from_raw(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SubId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// XEP-0060 §4.2 subscription states.
///
/// `None` is the absence of a subscription and is never persisted; it is
/// exposed only so handlers can return it from `<subscription/>` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionState {
    None,
    Pending,
    Unconfigured,
    Subscribed,
}

impl fmt::Display for SubscriptionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SubscriptionState::None => "none",
            SubscriptionState::Pending => "pending",
            SubscriptionState::Unconfigured => "unconfigured",
            SubscriptionState::Subscribed => "subscribed",
        };
        f.write_str(s)
    }
}

impl FromStr for SubscriptionState {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(SubscriptionState::None),
            "pending" => Ok(SubscriptionState::Pending),
            "unconfigured" => Ok(SubscriptionState::Unconfigured),
            "subscribed" => Ok(SubscriptionState::Subscribed),
            _ => Err(()),
        }
    }
}

/// A persisted subscription (XEP-0060 §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub subid: SubId,
    pub subscriber: Jid,
    pub state: SubscriptionState,
    pub created_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subid_generates_unique() {
        let a = SubId::generate();
        let b = SubId::generate();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 36); // UUID v4 length
    }

    #[test]
    fn subscription_state_round_trips() {
        for state in [
            SubscriptionState::None,
            SubscriptionState::Pending,
            SubscriptionState::Unconfigured,
            SubscriptionState::Subscribed,
        ] {
            let s = state.to_string();
            assert_eq!(s.parse::<SubscriptionState>().unwrap(), state);
        }
    }

    #[test]
    fn subscription_state_unknown_is_err() {
        assert!("garbage".parse::<SubscriptionState>().is_err());
    }
}
```

Wire it in `server/crates/waddle-xmpp-core/src/pubsub/mod.rs` — after the existing `pub mod` lines, add:

```rust
pub mod subscription;

pub use subscription::{SubId, Subscription, SubscriptionState};
```

- [ ] **Step 2: Run tests**

```bash
cd server && cargo test --package waddle-xmpp-core --lib pubsub::subscription
```

Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-xmpp-core/src/pubsub/subscription.rs server/crates/waddle-xmpp-core/src/pubsub/mod.rs
git commit -m "feat(server): add typed SubId/Subscription/SubscriptionState payloads"
```

---

## Task 3: Typed `Affiliation`

**Files:**
- Create: `server/crates/waddle-xmpp-core/src/pubsub/affiliation.rs`
- Modify: `server/crates/waddle-xmpp-core/src/pubsub/mod.rs`

- [ ] **Step 1: Write the failing test + impl**

Create `server/crates/waddle-xmpp-core/src/pubsub/affiliation.rs`:

```rust
//! XEP-0060 §4.1 affiliations.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Affiliation {
    #[default]
    None,
    Owner,
    Publisher,
    PublishOnly,
    Member,
    Outcast,
}

impl Affiliation {
    /// Whether this affiliation is allowed to publish to a node by default.
    /// Note: still subject to `PublishModel` overrides at the node level.
    pub fn can_publish_default(self) -> bool {
        matches!(
            self,
            Affiliation::Owner | Affiliation::Publisher | Affiliation::PublishOnly
        )
    }

    /// Whether this affiliation bars all interactions with the node.
    pub fn is_outcast(self) -> bool {
        matches!(self, Affiliation::Outcast)
    }

    /// Whether the row should be persisted. We do not persist `None` rows.
    pub fn is_persisted(self) -> bool {
        !matches!(self, Affiliation::None)
    }
}

impl fmt::Display for Affiliation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Affiliation::None => "none",
            Affiliation::Owner => "owner",
            Affiliation::Publisher => "publisher",
            Affiliation::PublishOnly => "publish-only",
            Affiliation::Member => "member",
            Affiliation::Outcast => "outcast",
        };
        f.write_str(s)
    }
}

impl FromStr for Affiliation {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Affiliation::None),
            "owner" => Ok(Affiliation::Owner),
            "publisher" => Ok(Affiliation::Publisher),
            "publish-only" => Ok(Affiliation::PublishOnly),
            "member" => Ok(Affiliation::Member),
            "outcast" => Ok(Affiliation::Outcast),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affiliation_round_trips() {
        for a in [
            Affiliation::None,
            Affiliation::Owner,
            Affiliation::Publisher,
            Affiliation::PublishOnly,
            Affiliation::Member,
            Affiliation::Outcast,
        ] {
            assert_eq!(a.to_string().parse::<Affiliation>().unwrap(), a);
        }
    }

    #[test]
    fn outcast_is_outcast() {
        assert!(Affiliation::Outcast.is_outcast());
        assert!(!Affiliation::Owner.is_outcast());
    }

    #[test]
    fn none_is_not_persisted() {
        assert!(!Affiliation::None.is_persisted());
        assert!(Affiliation::Owner.is_persisted());
    }

    #[test]
    fn publish_default_capabilities() {
        assert!(Affiliation::Owner.can_publish_default());
        assert!(Affiliation::Publisher.can_publish_default());
        assert!(Affiliation::PublishOnly.can_publish_default());
        assert!(!Affiliation::Member.can_publish_default());
        assert!(!Affiliation::None.can_publish_default());
        assert!(!Affiliation::Outcast.can_publish_default());
    }
}
```

In `server/crates/waddle-xmpp-core/src/pubsub/mod.rs` add:

```rust
pub mod affiliation;
pub use affiliation::Affiliation;
```

- [ ] **Step 2: Run tests**

```bash
cd server && cargo test --package waddle-xmpp-core --lib pubsub::affiliation
```

Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-xmpp-core/src/pubsub/affiliation.rs server/crates/waddle-xmpp-core/src/pubsub/mod.rs
git commit -m "feat(server): add typed Affiliation enum"
```

---

## Task 4: Extend `PubSubStorage` trait

**Files:**
- Modify: `server/crates/waddle-xmpp/src/pubsub/storage.rs`
- Modify: `server/crates/waddle-xmpp/src/pubsub/mod.rs`

The trait gets four new method groups: subscriptions, affiliations, purge, find-deliverable-subscribers (the one composite query). All other access checks live in `pubsub_authz` (Task 9), not here.

- [ ] **Step 1: Add new trait methods (no impl yet — `cargo build` will break and that is the failing test)**

In `server/crates/waddle-xmpp/src/pubsub/storage.rs`, replace the `use` block at the top with:

```rust
use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp_core::pubsub::{Affiliation, SubId, Subscription, SubscriptionState};

use super::node::NodeConfig;
use super::stanzas::PubSubItem;
use crate::XmppError;
```

Append new methods to the `pub trait PubSubStorage` block (before the closing `}`):

```rust
    /// Purge all items from a node without deleting the node (XEP-0060 §8.5).
    /// Returns the number of items removed.
    async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError>;

    // ----- subscriptions -----

    /// Create a new subscription. Always inserts a new row (multi-sub-per-jid
    /// allowed by XEP-0060 §6.1). Returns the generated subid + state.
    async fn subscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
    ) -> Result<Subscription, XmppError>;

    /// Remove a subscription. If `subid` is `Some`, target that exact row;
    /// if `None`, the caller must have already established that there is
    /// exactly one subscription for `subscriber` (see XEP-0060 §6.2.3.2).
    /// Returns true if a row was deleted.
    async fn unsubscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
        subid: Option<&SubId>,
    ) -> Result<bool, XmppError>;

    /// List all subscriptions for a node.
    async fn list_node_subscriptions(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError>;

    /// List all subscriptions held by a specific subscriber across all nodes
    /// owned by `owner`. Used to answer `<subscriptions/>` requests.
    async fn list_subscriber_subscriptions(
        &self,
        owner: &BareJid,
        subscriber: &Jid,
    ) -> Result<Vec<(String, Subscription)>, XmppError>;

    /// Look up a single subscription by `(owner, node, subid)`.
    async fn get_subscription(
        &self,
        owner: &BareJid,
        node_name: &str,
        subid: &SubId,
    ) -> Result<Option<Subscription>, XmppError>;

    /// Hot-path query for publish fan-out. Returns subscribers with state
    /// `Subscribed` whose entity is *not* `Outcast`. The exact return
    /// semantics: each row is a `(subscriber_jid, subid, state)` tuple,
    /// already filtered.
    async fn list_deliverable_subscribers(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError>;

    // ----- affiliations -----

    /// Set or remove an affiliation. Setting `Affiliation::None` deletes
    /// the row. Returns the previous affiliation (`Affiliation::None` if
    /// no row existed).
    async fn set_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
        affiliation: Affiliation,
    ) -> Result<Affiliation, XmppError>;

    /// Read the explicit affiliation row for `(owner, node, entity)`.
    /// Returns `Affiliation::None` if no row exists. Owner-derivation for
    /// PEP nodes happens in `pubsub_authz`, *not* here.
    async fn get_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
    ) -> Result<Affiliation, XmppError>;

    /// List all explicit affiliation rows for a node.
    async fn list_node_affiliations(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<(BareJid, Affiliation)>, XmppError>;

    /// List all explicit affiliation rows held by a single entity across
    /// all nodes owned by `owner`.
    async fn list_entity_affiliations(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<Vec<(String, Affiliation)>, XmppError>;
```

Update the re-export in `server/crates/waddle-xmpp/src/pubsub/mod.rs`:

```rust
pub use storage::{InMemoryPubSubStorage, PubSubNode, PubSubStorage, PublishResult, StoredItem};

// Re-export typed payloads from core for convenience.
pub use waddle_xmpp_core::pubsub::{Affiliation, SubId, Subscription, SubscriptionState};
```

- [ ] **Step 2: Verify compile failure surfaces the missing impls**

```bash
cd server && cargo build --package waddle-xmpp 2>&1 | head -30
```

Expected: errors of the form `not all trait items implemented` for `InMemoryPubSubStorage`. This is the failing test — all subsequent tasks make it green again.

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-xmpp/src/pubsub/storage.rs server/crates/waddle-xmpp/src/pubsub/mod.rs
git commit -m "feat(server): extend PubSubStorage with subscriptions/affiliations/purge"
```

---

## Task 5: Implement new methods on `InMemoryPubSubStorage`

**Files:**
- Modify: `server/crates/waddle-xmpp/src/pubsub/storage.rs`

- [ ] **Step 1: Add the new fields and impls**

Replace the `pub struct InMemoryPubSubStorage` block and its `new()` impl with:

```rust
pub struct InMemoryPubSubStorage {
    /// (owner_bare_jid, node_name) -> PubSubNode
    nodes: dashmap::DashMap<(String, String), PubSubNode>,
    /// (owner_bare_jid, node_name) -> Vec<StoredItem>
    items: dashmap::DashMap<(String, String), Vec<StoredItem>>,
    /// (owner_bare_jid, node_name, subid) -> Subscription
    subscriptions: dashmap::DashMap<(String, String, String), Subscription>,
    /// (owner_bare_jid, node_name, entity_bare_jid) -> Affiliation
    affiliations: dashmap::DashMap<(String, String, String), Affiliation>,
}

impl Default for InMemoryPubSubStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryPubSubStorage {
    pub fn new() -> Self {
        Self {
            nodes: dashmap::DashMap::new(),
            items: dashmap::DashMap::new(),
            subscriptions: dashmap::DashMap::new(),
            affiliations: dashmap::DashMap::new(),
        }
    }

    fn key(owner: &BareJid, node_name: &str) -> (String, String) {
        (owner.to_string(), node_name.to_string())
    }

    fn generate_item_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
}
```

Append the trait method bodies inside the existing `impl PubSubStorage for InMemoryPubSubStorage` block (before its closing `}`):

```rust
    async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError> {
        let key = Self::key(owner, node_name);
        let removed = match self.items.get_mut(&key) {
            Some(mut items) => {
                let n = items.len() as u64;
                items.clear();
                n
            }
            None => 0,
        };
        Ok(removed)
    }

    async fn subscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
    ) -> Result<Subscription, XmppError> {
        let subid = SubId::generate();
        let sub = Subscription {
            subid: subid.clone(),
            subscriber: subscriber.clone(),
            state: SubscriptionState::Subscribed,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        let key = (
            owner.to_string(),
            node_name.to_string(),
            subid.as_str().to_string(),
        );
        self.subscriptions.insert(key, sub.clone());
        Ok(sub)
    }

    async fn unsubscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
        subid: Option<&SubId>,
    ) -> Result<bool, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        let subscriber_str = subscriber.to_string();

        if let Some(subid) = subid {
            let key = (owner_str, node_str, subid.as_str().to_string());
            return Ok(self
                .subscriptions
                .remove_if(&key, |_, sub| sub.subscriber.to_string() == subscriber_str)
                .is_some());
        }

        let mut victim = None;
        for entry in self.subscriptions.iter() {
            let (k_owner, k_node, _) = entry.key();
            if k_owner == &owner.to_string()
                && k_node == &node_name.to_string()
                && entry.value().subscriber.to_string() == subscriber_str
            {
                victim = Some(entry.key().clone());
                break;
            }
        }
        Ok(victim
            .and_then(|k| self.subscriptions.remove(&k))
            .is_some())
    }

    async fn list_node_subscriptions(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        Ok(self
            .subscriptions
            .iter()
            .filter(|e| e.key().0 == owner_str && e.key().1 == node_str)
            .map(|e| e.value().clone())
            .collect())
    }

    async fn list_subscriber_subscriptions(
        &self,
        owner: &BareJid,
        subscriber: &Jid,
    ) -> Result<Vec<(String, Subscription)>, XmppError> {
        let owner_str = owner.to_string();
        let subscriber_str = subscriber.to_string();
        Ok(self
            .subscriptions
            .iter()
            .filter(|e| {
                e.key().0 == owner_str && e.value().subscriber.to_string() == subscriber_str
            })
            .map(|e| (e.key().1.clone(), e.value().clone()))
            .collect())
    }

    async fn get_subscription(
        &self,
        owner: &BareJid,
        node_name: &str,
        subid: &SubId,
    ) -> Result<Option<Subscription>, XmppError> {
        let key = (
            owner.to_string(),
            node_name.to_string(),
            subid.as_str().to_string(),
        );
        Ok(self.subscriptions.get(&key).map(|v| v.clone()))
    }

    async fn list_deliverable_subscribers(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        let mut out = Vec::new();
        for entry in self.subscriptions.iter() {
            if entry.key().0 != owner_str || entry.key().1 != node_str {
                continue;
            }
            let sub = entry.value();
            if sub.state != SubscriptionState::Subscribed {
                continue;
            }
            // Filter outcasts.
            let entity_bare = sub.subscriber.to_bare();
            let aff_key = (owner_str.clone(), node_str.clone(), entity_bare.to_string());
            let outcast = self
                .affiliations
                .get(&aff_key)
                .map(|v| v.is_outcast())
                .unwrap_or(false);
            if outcast {
                continue;
            }
            out.push(sub.clone());
        }
        Ok(out)
    }

    async fn set_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
        affiliation: Affiliation,
    ) -> Result<Affiliation, XmppError> {
        let key = (
            owner.to_string(),
            node_name.to_string(),
            entity.to_string(),
        );
        if affiliation == Affiliation::None {
            return Ok(self.affiliations.remove(&key).map(|(_, v)| v).unwrap_or(Affiliation::None));
        }
        let prev = self.affiliations.insert(key, affiliation);
        Ok(prev.unwrap_or(Affiliation::None))
    }

    async fn get_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
    ) -> Result<Affiliation, XmppError> {
        let key = (
            owner.to_string(),
            node_name.to_string(),
            entity.to_string(),
        );
        Ok(self.affiliations.get(&key).map(|v| *v).unwrap_or(Affiliation::None))
    }

    async fn list_node_affiliations(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<(BareJid, Affiliation)>, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        let mut out = Vec::new();
        for entry in self.affiliations.iter() {
            if entry.key().0 == owner_str && entry.key().1 == node_str {
                let entity = entry
                    .key()
                    .2
                    .parse::<BareJid>()
                    .map_err(|e| XmppError::internal(e.to_string()))?;
                out.push((entity, *entry.value()));
            }
        }
        Ok(out)
    }

    async fn list_entity_affiliations(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<Vec<(String, Affiliation)>, XmppError> {
        let owner_str = owner.to_string();
        let entity_str = entity.to_string();
        Ok(self
            .affiliations
            .iter()
            .filter(|e| e.key().0 == owner_str && e.key().2 == entity_str)
            .map(|e| (e.key().1.clone(), *e.value()))
            .collect())
    }
```

- [ ] **Step 2: Verify the crate compiles**

```bash
cd server && cargo build --package waddle-xmpp
```

Expected: clean build.

- [ ] **Step 3: Add tests**

Append inside the existing `#[cfg(test)] mod tests` block in `server/crates/waddle-xmpp/src/pubsub/storage.rs`:

```rust
    #[tokio::test]
    async fn in_memory_subscribe_returns_unique_subids() {
        let storage = InMemoryPubSubStorage::new();
        let owner: BareJid = "u@x.com".parse().unwrap();
        let alice: Jid = "alice@x.com".parse().unwrap();

        let s1 = storage.subscribe(&owner, "node", &alice).await.unwrap();
        let s2 = storage.subscribe(&owner, "node", &alice).await.unwrap();
        assert_ne!(s1.subid, s2.subid);
        assert_eq!(s1.state, SubscriptionState::Subscribed);
    }

    #[tokio::test]
    async fn in_memory_unsubscribe_with_subid_targets_one_row() {
        let storage = InMemoryPubSubStorage::new();
        let owner: BareJid = "u@x.com".parse().unwrap();
        let alice: Jid = "alice@x.com".parse().unwrap();

        let s1 = storage.subscribe(&owner, "node", &alice).await.unwrap();
        let _s2 = storage.subscribe(&owner, "node", &alice).await.unwrap();

        let removed = storage.unsubscribe(&owner, "node", &alice, Some(&s1.subid)).await.unwrap();
        assert!(removed);

        let remaining = storage.list_node_subscriptions(&owner, "node").await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[tokio::test]
    async fn in_memory_set_affiliation_none_deletes_row() {
        let storage = InMemoryPubSubStorage::new();
        let owner: BareJid = "u@x.com".parse().unwrap();
        let entity: BareJid = "bob@x.com".parse().unwrap();

        let prev = storage.set_affiliation(&owner, "node", &entity, Affiliation::Outcast).await.unwrap();
        assert_eq!(prev, Affiliation::None);
        assert_eq!(storage.get_affiliation(&owner, "node", &entity).await.unwrap(), Affiliation::Outcast);

        let prev = storage.set_affiliation(&owner, "node", &entity, Affiliation::None).await.unwrap();
        assert_eq!(prev, Affiliation::Outcast);
        assert_eq!(storage.get_affiliation(&owner, "node", &entity).await.unwrap(), Affiliation::None);
    }

    #[tokio::test]
    async fn in_memory_deliverable_subscribers_excludes_outcasts() {
        let storage = InMemoryPubSubStorage::new();
        let owner: BareJid = "u@x.com".parse().unwrap();
        let alice: Jid = "alice@x.com".parse().unwrap();
        let bob: Jid = "bob@x.com".parse().unwrap();

        storage.subscribe(&owner, "node", &alice).await.unwrap();
        storage.subscribe(&owner, "node", &bob).await.unwrap();

        let bob_bare: BareJid = "bob@x.com".parse().unwrap();
        storage.set_affiliation(&owner, "node", &bob_bare, Affiliation::Outcast).await.unwrap();

        let deliverable = storage.list_deliverable_subscribers(&owner, "node").await.unwrap();
        assert_eq!(deliverable.len(), 1);
        assert_eq!(deliverable[0].subscriber.to_string(), "alice@x.com");
    }

    #[tokio::test]
    async fn in_memory_purge_clears_items_keeps_node() {
        let storage = InMemoryPubSubStorage::new();
        let owner: BareJid = "u@x.com".parse().unwrap();
        for i in 1..=3 {
            let item = PubSubItem::new(Some(format!("i{i}")), None);
            storage.publish_item(&owner, "n", &item, None, true).await.unwrap();
        }
        let removed = storage.purge_node(&owner, "n").await.unwrap();
        assert!(removed >= 1); // PEP default max_items=1, so trim already ran
        let items = storage.get_items(&owner, "n", None, &[]).await.unwrap();
        assert!(items.is_empty());
        assert!(storage.get_node(&owner, "n").await.unwrap().is_some());
    }
```

- [ ] **Step 4: Run tests**

```bash
cd server && cargo test --package waddle-xmpp --lib pubsub::storage
```

Expected: all (existing + 5 new) pass.

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-xmpp/src/pubsub/storage.rs
git commit -m "feat(server): implement subscriptions/affiliations on InMemoryPubSubStorage"
```

---

## Task 6: Rewrite `pubsub.rs` schema with versioning, epoch ms, seq

**Files:**
- Modify: `server/crates/waddle-server/src/pubsub.rs`

This is the biggest single edit — full rewrite of `initialize()` and the per-driver DDL. Behavior changes captured in this task:

1. Add `pubsub_schema_version` table.
2. On version mismatch, `DROP TABLE` everything and recreate.
3. `pubsub_items.published_at TEXT` → `published_at_ms INTEGER`.
4. Add `pubsub_items.seq` autoincrement (per-driver: sqlite `INTEGER PRIMARY KEY AUTOINCREMENT`, postgres `BIGINT GENERATED BY DEFAULT AS IDENTITY`).
5. New tables `pubsub_subscriptions` and `pubsub_affiliations`.
6. New indexes on `(owner, node, seq DESC)`, `(owner, item_id)`, `(entity_jid)`, `(subscriber_jid)`.

- [ ] **Step 1: Replace `initialize()`**

In `server/crates/waddle-server/src/pubsub.rs`, replace the existing `async fn initialize(&self) -> Result<(), XmppError>` (currently lines 33–78) with:

```rust
const PUBSUB_SCHEMA_VERSION: i64 = 1;

async fn initialize(&self) -> Result<(), XmppError> {
    self.execute(
        r#"
        CREATE TABLE IF NOT EXISTS pubsub_schema_version (
            version INTEGER NOT NULL PRIMARY KEY
        )
        "#,
        (),
    )
    .await?;

    let mut rows = self
        .query("SELECT version FROM pubsub_schema_version", ())
        .await?;
    let current: Option<i64> = match rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        Some(row) => Some(
            row.get(0)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        ),
        None => None,
    };

    if current != Some(PUBSUB_SCHEMA_VERSION) {
        // Drop-and-recreate: CLAUDE.md greenlights breaking changes.
        for table in [
            "pubsub_items",
            "pubsub_subscriptions",
            "pubsub_affiliations",
            "pubsub_nodes",
        ] {
            self.execute(&format!("DROP TABLE IF EXISTS {table}"), ())
                .await?;
        }
        self.execute("DELETE FROM pubsub_schema_version", ()).await?;
    }

    self.create_schema().await?;

    if current != Some(PUBSUB_SCHEMA_VERSION) {
        self.execute(
            "INSERT INTO pubsub_schema_version (version) VALUES (?)",
            crate::db_params![PUBSUB_SCHEMA_VERSION],
        )
        .await?;
    }
    Ok(())
}

async fn create_schema(&self) -> Result<(), XmppError> {
    self.execute(
        r#"
        CREATE TABLE IF NOT EXISTS pubsub_nodes (
            owner_jid TEXT NOT NULL,
            node_name TEXT NOT NULL,
            access_model TEXT NOT NULL,
            publish_model TEXT NOT NULL,
            max_items INTEGER NOT NULL,
            persist_items INTEGER NOT NULL,
            deliver_payloads INTEGER NOT NULL,
            notify_retract INTEGER NOT NULL,
            notify_delete INTEGER NOT NULL,
            send_last_published_item TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (owner_jid, node_name)
        )
        "#,
        (),
    )
    .await?;

    let items_ddl = match self.db.driver() {
        crate::db::DatabaseDriver::Sqlite => {
            r#"
            CREATE TABLE IF NOT EXISTS pubsub_items (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                owner_jid TEXT NOT NULL,
                node_name TEXT NOT NULL,
                item_id TEXT NOT NULL,
                payload_xml TEXT,
                publisher_jid TEXT,
                published_at_ms INTEGER NOT NULL,
                UNIQUE (owner_jid, node_name, item_id),
                FOREIGN KEY (owner_jid, node_name)
                    REFERENCES pubsub_nodes(owner_jid, node_name)
                    ON DELETE CASCADE
            )
            "#
        }
        crate::db::DatabaseDriver::Postgres => {
            r#"
            CREATE TABLE IF NOT EXISTS pubsub_items (
                seq BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
                owner_jid TEXT NOT NULL,
                node_name TEXT NOT NULL,
                item_id TEXT NOT NULL,
                payload_xml TEXT,
                publisher_jid TEXT,
                published_at_ms BIGINT NOT NULL,
                UNIQUE (owner_jid, node_name, item_id),
                FOREIGN KEY (owner_jid, node_name)
                    REFERENCES pubsub_nodes(owner_jid, node_name)
                    ON DELETE CASCADE
            )
            "#
        }
    };
    self.execute(items_ddl, ()).await?;

    self.execute(
        "CREATE INDEX IF NOT EXISTS idx_pubsub_items_node_seq ON pubsub_items (owner_jid, node_name, seq DESC)",
        (),
    )
    .await?;
    self.execute(
        "CREATE INDEX IF NOT EXISTS idx_pubsub_items_owner_item ON pubsub_items (owner_jid, item_id)",
        (),
    )
    .await?;

    self.execute(
        r#"
        CREATE TABLE IF NOT EXISTS pubsub_subscriptions (
            owner_jid TEXT NOT NULL,
            node_name TEXT NOT NULL,
            subid TEXT NOT NULL,
            subscriber_jid TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (owner_jid, node_name, subid),
            FOREIGN KEY (owner_jid, node_name)
                REFERENCES pubsub_nodes(owner_jid, node_name)
                ON DELETE CASCADE
        )
        "#,
        (),
    )
    .await?;
    self.execute(
        "CREATE INDEX IF NOT EXISTS idx_pubsub_subs_subscriber ON pubsub_subscriptions (owner_jid, subscriber_jid)",
        (),
    )
    .await?;

    self.execute(
        r#"
        CREATE TABLE IF NOT EXISTS pubsub_affiliations (
            owner_jid TEXT NOT NULL,
            node_name TEXT NOT NULL,
            entity_jid TEXT NOT NULL,
            affiliation TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY (owner_jid, node_name, entity_jid),
            FOREIGN KEY (owner_jid, node_name)
                REFERENCES pubsub_nodes(owner_jid, node_name)
                ON DELETE CASCADE
        )
        "#,
        (),
    )
    .await?;
    self.execute(
        "CREATE INDEX IF NOT EXISTS idx_pubsub_affs_entity ON pubsub_affiliations (owner_jid, entity_jid)",
        (),
    )
    .await?;

    Ok(())
}
```

- [ ] **Step 2: Update `insert_node` to use `created_at_ms`**

Replace the existing `insert_node` (currently around lines 106–134) with:

```rust
async fn insert_node(&self, node: &PubSubNode) -> Result<(), XmppError> {
    let config = &node.config;
    self.execute(
        r#"
        INSERT INTO pubsub_nodes (
            owner_jid, node_name, access_model, publish_model, max_items,
            persist_items, deliver_payloads, notify_retract, notify_delete,
            send_last_published_item, created_at_ms
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(owner_jid, node_name) DO NOTHING
        "#,
        crate::db_params![
            node.owner.to_string(),
            node.node_name.clone(),
            config.access_model.to_string(),
            config.publish_model.to_string(),
            config.max_items,
            config.persist_items,
            config.deliver_payloads,
            config.notify_retract,
            config.notify_delete,
            config.send_last_published_item.to_string(),
            node.created_at.timestamp_millis(),
        ],
    )
    .await?;
    Ok(())
}
```

- [ ] **Step 3: Update `decode_node` to read `created_at_ms`**

In `decode_node` (currently around lines 136–193), change the `created_at` decode block from RFC3339 parsing to:

```rust
let created_at_ms: i64 = row
    .get(10)
    .map_err(|error| XmppError::internal(error.to_string()))?;
let created_at = chrono::DateTime::from_timestamp_millis(created_at_ms)
    .ok_or_else(|| XmppError::internal("invalid PubSub created_at_ms".to_string()))?;
```

(Remove the `created_raw: String` lookup and the `parse_from_rfc3339` call.)

- [ ] **Step 4: Update `decode_item` to read `published_at_ms`**

In `decode_item` (currently around lines 195–224), change to:

```rust
fn decode_item(row: &crate::db::Row) -> Result<StoredItem, XmppError> {
    let id: String = row
        .get(0)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let payload_xml: Option<String> = row
        .get(1)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let publisher_raw: Option<String> = row
        .get(2)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let published_at_ms: i64 = row
        .get(3)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let publisher = publisher_raw
        .map(|raw| {
            raw.parse::<BareJid>().map_err(|error| {
                XmppError::internal(format!("invalid PubSub publisher JID: {error}"))
            })
        })
        .transpose()?;
    let published_at = chrono::DateTime::from_timestamp_millis(published_at_ms)
        .ok_or_else(|| XmppError::internal("invalid PubSub published_at_ms".to_string()))?;
    Ok(StoredItem {
        id,
        payload_xml,
        publisher,
        published_at,
    })
}
```

- [ ] **Step 5: Verify schema build is wired**

```bash
cd server && cargo build --package waddle-server
```

Expected: build error from the `decode_node` line-counting (the `.get(10)` call relies on column ordering) being cross-checked against `get_node`'s `SELECT` list. Read both and confirm the SELECT order matches the decode order.

- [ ] **Step 6: Update `get_node` SELECT to read `created_at_ms`**

Replace the existing `get_node`'s SELECT (lines 285–310) so the last selected column is `created_at_ms` (rename — keep the column order). Just s/`created_at`/`created_at_ms`/.

- [ ] **Step 7: Build clean**

```bash
cd server && cargo build --package waddle-server
```

Expected: clean build.

- [ ] **Step 8: Commit**

```bash
git add server/crates/waddle-server/src/pubsub.rs
git commit -m "feat(server): rewrite pubsub schema with versioning, epoch ms, seq, sub/aff tables"
```

---

## Task 7: SQL pushdown in `get_items` + single-DELETE `enforce_max_items`

**Files:**
- Modify: `server/crates/waddle-server/src/pubsub.rs`

- [ ] **Step 1: Rewrite `get_items`**

Replace the existing `get_items` impl with:

```rust
async fn get_items(
    &self,
    owner: &BareJid,
    node_name: &str,
    max_items: Option<u32>,
    item_ids: &[String],
) -> Result<Vec<StoredItem>, XmppError> {
    if !item_ids.is_empty() {
        // Build IN (?, ?, ...) clause inline. item_ids comes from a parsed
        // IQ payload and is bounded by the request size. Use placeholders,
        // never string-format the values.
        let placeholders = std::iter::repeat("?").take(item_ids.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            r#"
            SELECT item_id, payload_xml, publisher_jid, published_at_ms
            FROM pubsub_items
            WHERE owner_jid = ? AND node_name = ? AND item_id IN ({placeholders})
            ORDER BY seq ASC
            "#
        );
        let mut params: Vec<crate::db::Value> = Vec::with_capacity(2 + item_ids.len());
        params.push(crate::db::Value::from(owner.to_string()));
        params.push(crate::db::Value::from(node_name));
        for id in item_ids {
            params.push(crate::db::Value::from(id.clone()));
        }
        return self.run_select_items(&sql, params).await;
    }

    let limit_sql = match max_items {
        Some(n) if n > 0 => format!(
            r#"
            SELECT item_id, payload_xml, publisher_jid, published_at_ms FROM (
                SELECT item_id, payload_xml, publisher_jid, published_at_ms, seq
                FROM pubsub_items
                WHERE owner_jid = ? AND node_name = ?
                ORDER BY seq DESC
                LIMIT {n}
            ) t
            ORDER BY seq ASC
            "#
        ),
        _ => r#"
            SELECT item_id, payload_xml, publisher_jid, published_at_ms
            FROM pubsub_items
            WHERE owner_jid = ? AND node_name = ?
            ORDER BY seq ASC
            "#
        .to_string(),
    };

    self.run_select_items(
        &limit_sql,
        vec![
            crate::db::Value::from(owner.to_string()),
            crate::db::Value::from(node_name),
        ],
    )
    .await
}
```

Add the helper next to `decode_item` (note: SELECT order is `(item_id, payload_xml, publisher_jid, published_at_ms)`, matching `decode_item` exactly):

```rust
async fn run_select_items(
    &self,
    sql: &str,
    params: Vec<crate::db::Value>,
) -> Result<Vec<StoredItem>, XmppError> {
    let mut rows = self.query(sql, params).await?;
    let mut items = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        items.push(Self::decode_item(&row)?);
    }
    Ok(items)
}
```

- [ ] **Step 2: Rewrite `enforce_max_items` to single DELETE**

Replace it with:

```rust
async fn enforce_max_items(
    &self,
    owner: &BareJid,
    node_name: &str,
    max_items: u32,
) -> Result<(), XmppError> {
    if max_items == 0 || max_items == u32::MAX {
        return Ok(());
    }
    self.execute(
        r#"
        DELETE FROM pubsub_items
        WHERE owner_jid = ? AND node_name = ?
          AND seq NOT IN (
              SELECT seq FROM pubsub_items
              WHERE owner_jid = ? AND node_name = ?
              ORDER BY seq DESC
              LIMIT ?
          )
        "#,
        crate::db_params![
            owner.to_string(),
            node_name,
            owner.to_string(),
            node_name,
            max_items,
        ],
    )
    .await?;
    Ok(())
}
```

- [ ] **Step 3: Update `publish_item` to write `published_at_ms`**

In the existing `publish_item`, change the published_at line:

```rust
let published_at_ms = crate::time::now_ms();
```

And the INSERT:

```rust
self.execute(
    r#"
    INSERT INTO pubsub_items (
        owner_jid, node_name, item_id, payload_xml, publisher_jid, published_at_ms
    )
    VALUES (?, ?, ?, ?, ?, ?)
    ON CONFLICT(owner_jid, node_name, item_id) DO UPDATE SET
        payload_xml = excluded.payload_xml,
        publisher_jid = excluded.publisher_jid,
        published_at_ms = excluded.published_at_ms
    "#,
    crate::db_params![
        owner.to_string(),
        node_name,
        item_id.clone(),
        payload_xml,
        publisher_jid,
        published_at_ms,
    ],
)
.await?;
```

(`seq` is omitted from the column list — the autoincrement supplies it. On `ON CONFLICT DO UPDATE` the existing row keeps its `seq`, which is the documented behavior we want: re-publishing the same `item_id` keeps it at its original sort position. If you want re-publish to bump it to the front, use `ON CONFLICT DO UPDATE SET ... seq = nextval(...)` for postgres or a separate `UPDATE pubsub_items SET seq = ... WHERE rowid = last_insert_rowid()` dance for sqlite — but XEP-0060 §7.1.2 is silent on this and ejabberd keeps the original position, so do the same.)

- [ ] **Step 4: Run the existing pubsub tests**

```bash
cd server && cargo test --package waddle-server pubsub::tests
```

Expected: existing tests in `pubsub.rs` pass (they don't yet exercise subscriptions/affiliations).

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-server/src/pubsub.rs
git commit -m "feat(server): push max_items/item_ids into SQL; single-DELETE trim"
```

---

## Task 8: Implement subscriptions/affiliations/purge on `DatabasePubSubStorage`

**Files:**
- Modify: `server/crates/waddle-server/src/pubsub.rs`

Append the new method bodies inside `impl PubSubStorage for DatabasePubSubStorage`. Each one follows the `query` / `execute` pattern from existing methods.

- [ ] **Step 1: Add `purge_node`**

```rust
async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError> {
    let affected = self
        .execute(
            "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ?",
            crate::db_params![owner.to_string(), node_name],
        )
        .await?;
    Ok(affected)
}
```

- [ ] **Step 2: Add `subscribe`**

```rust
async fn subscribe(
    &self,
    owner: &BareJid,
    node_name: &str,
    subscriber: &Jid,
) -> Result<Subscription, XmppError> {
    let subid = SubId::generate();
    let now = crate::time::now_ms();
    self.execute(
        r#"
        INSERT INTO pubsub_subscriptions (owner_jid, node_name, subid, subscriber_jid, state, created_at_ms)
        VALUES (?, ?, ?, ?, 'subscribed', ?)
        "#,
        crate::db_params![
            owner.to_string(),
            node_name,
            subid.as_str().to_string(),
            subscriber.to_string(),
            now,
        ],
    )
    .await?;
    Ok(Subscription {
        subid,
        subscriber: subscriber.clone(),
        state: SubscriptionState::Subscribed,
        created_at_ms: now,
    })
}
```

- [ ] **Step 3: Add `unsubscribe`**

```rust
async fn unsubscribe(
    &self,
    owner: &BareJid,
    node_name: &str,
    subscriber: &Jid,
    subid: Option<&SubId>,
) -> Result<bool, XmppError> {
    let affected = match subid {
        Some(subid) => {
            self.execute(
                "DELETE FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? AND subid = ? AND subscriber_jid = ?",
                crate::db_params![
                    owner.to_string(),
                    node_name,
                    subid.as_str().to_string(),
                    subscriber.to_string(),
                ],
            ).await?
        }
        None => {
            self.execute(
                "DELETE FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? AND subscriber_jid = ?",
                crate::db_params![
                    owner.to_string(),
                    node_name,
                    subscriber.to_string(),
                ],
            ).await?
        }
    };
    Ok(affected > 0)
}
```

- [ ] **Step 4: Add the `list_*` and `get_subscription` methods**

```rust
async fn list_node_subscriptions(
    &self,
    owner: &BareJid,
    node_name: &str,
) -> Result<Vec<Subscription>, XmppError> {
    let mut rows = self
        .query(
            "SELECT subid, subscriber_jid, state, created_at_ms FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? ORDER BY created_at_ms ASC",
            crate::db_params![owner.to_string(), node_name],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| XmppError::internal(e.to_string()))? {
        out.push(decode_subscription(&row)?);
    }
    Ok(out)
}

async fn list_subscriber_subscriptions(
    &self,
    owner: &BareJid,
    subscriber: &Jid,
) -> Result<Vec<(String, Subscription)>, XmppError> {
    let mut rows = self
        .query(
            "SELECT node_name, subid, subscriber_jid, state, created_at_ms FROM pubsub_subscriptions WHERE owner_jid = ? AND subscriber_jid = ? ORDER BY node_name ASC, created_at_ms ASC",
            crate::db_params![owner.to_string(), subscriber.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| XmppError::internal(e.to_string()))? {
        let node: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
        let sub = decode_subscription_offset(&row, 1)?;
        out.push((node, sub));
    }
    Ok(out)
}

async fn get_subscription(
    &self,
    owner: &BareJid,
    node_name: &str,
    subid: &SubId,
) -> Result<Option<Subscription>, XmppError> {
    let mut rows = self
        .query(
            "SELECT subid, subscriber_jid, state, created_at_ms FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? AND subid = ?",
            crate::db_params![owner.to_string(), node_name, subid.as_str().to_string()],
        )
        .await?;
    let Some(row) = rows.next().await.map_err(|e| XmppError::internal(e.to_string()))? else {
        return Ok(None);
    };
    Ok(Some(decode_subscription(&row)?))
}

async fn list_deliverable_subscribers(
    &self,
    owner: &BareJid,
    node_name: &str,
) -> Result<Vec<Subscription>, XmppError> {
    let mut rows = self
        .query(
            r#"
            SELECT s.subid, s.subscriber_jid, s.state, s.created_at_ms
            FROM pubsub_subscriptions s
            LEFT JOIN pubsub_affiliations a
              ON a.owner_jid = s.owner_jid
             AND a.node_name = s.node_name
             AND a.entity_jid = s.subscriber_jid
            WHERE s.owner_jid = ?
              AND s.node_name = ?
              AND s.state = 'subscribed'
              AND (a.affiliation IS NULL OR a.affiliation <> 'outcast')
            "#,
            crate::db_params![owner.to_string(), node_name],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| XmppError::internal(e.to_string()))? {
        out.push(decode_subscription(&row)?);
    }
    Ok(out)
}
```

(NOTE: the LEFT JOIN keys by bare-or-full subscriber JID against bare entity JID. For full-JID subscriptions, the outcast check should match against the bare. We accept this slightly looser join shape; the alternative — `a.entity_jid = bare(s.subscriber_jid)` — requires SQL-side bare-jid extraction, which is engine-specific. Document the limitation.)

- [ ] **Step 5: Add affiliation methods**

```rust
async fn set_affiliation(
    &self,
    owner: &BareJid,
    node_name: &str,
    entity: &BareJid,
    affiliation: Affiliation,
) -> Result<Affiliation, XmppError> {
    let prev = self.get_affiliation(owner, node_name, entity).await?;
    if affiliation == Affiliation::None {
        self.execute(
            "DELETE FROM pubsub_affiliations WHERE owner_jid = ? AND node_name = ? AND entity_jid = ?",
            crate::db_params![owner.to_string(), node_name, entity.to_string()],
        ).await?;
        return Ok(prev);
    }
    self.execute(
        r#"
        INSERT INTO pubsub_affiliations (owner_jid, node_name, entity_jid, affiliation, updated_at_ms)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(owner_jid, node_name, entity_jid) DO UPDATE SET
            affiliation = excluded.affiliation,
            updated_at_ms = excluded.updated_at_ms
        "#,
        crate::db_params![
            owner.to_string(),
            node_name,
            entity.to_string(),
            affiliation.to_string(),
            crate::time::now_ms(),
        ],
    ).await?;
    Ok(prev)
}

async fn get_affiliation(
    &self,
    owner: &BareJid,
    node_name: &str,
    entity: &BareJid,
) -> Result<Affiliation, XmppError> {
    let mut rows = self
        .query(
            "SELECT affiliation FROM pubsub_affiliations WHERE owner_jid = ? AND node_name = ? AND entity_jid = ?",
            crate::db_params![owner.to_string(), node_name, entity.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await.map_err(|e| XmppError::internal(e.to_string()))? else {
        return Ok(Affiliation::None);
    };
    let raw: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
    Ok(raw.parse().unwrap_or(Affiliation::None))
}

async fn list_node_affiliations(
    &self,
    owner: &BareJid,
    node_name: &str,
) -> Result<Vec<(BareJid, Affiliation)>, XmppError> {
    let mut rows = self
        .query(
            "SELECT entity_jid, affiliation FROM pubsub_affiliations WHERE owner_jid = ? AND node_name = ? ORDER BY entity_jid ASC",
            crate::db_params![owner.to_string(), node_name],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| XmppError::internal(e.to_string()))? {
        let entity_raw: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
        let entity = entity_raw.parse::<BareJid>().map_err(|e| XmppError::internal(e.to_string()))?;
        let aff_raw: String = row.get(1).map_err(|e| XmppError::internal(e.to_string()))?;
        let aff: Affiliation = aff_raw.parse().unwrap_or(Affiliation::None);
        out.push((entity, aff));
    }
    Ok(out)
}

async fn list_entity_affiliations(
    &self,
    owner: &BareJid,
    entity: &BareJid,
) -> Result<Vec<(String, Affiliation)>, XmppError> {
    let mut rows = self
        .query(
            "SELECT node_name, affiliation FROM pubsub_affiliations WHERE owner_jid = ? AND entity_jid = ? ORDER BY node_name ASC",
            crate::db_params![owner.to_string(), entity.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| XmppError::internal(e.to_string()))? {
        let node: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
        let aff_raw: String = row.get(1).map_err(|e| XmppError::internal(e.to_string()))?;
        let aff: Affiliation = aff_raw.parse().unwrap_or(Affiliation::None);
        out.push((node, aff));
    }
    Ok(out)
}
```

- [ ] **Step 6: Add the decode helpers**

Outside the `impl PubSubStorage` block (after `decode_item`):

```rust
fn decode_subscription(row: &crate::db::Row) -> Result<Subscription, XmppError> {
    decode_subscription_offset(row, 0)
}

fn decode_subscription_offset(row: &crate::db::Row, offset: usize) -> Result<Subscription, XmppError> {
    let subid_raw: String = row.get(offset).map_err(|e| XmppError::internal(e.to_string()))?;
    let subscriber_raw: String = row.get(offset + 1).map_err(|e| XmppError::internal(e.to_string()))?;
    let state_raw: String = row.get(offset + 2).map_err(|e| XmppError::internal(e.to_string()))?;
    let created_at_ms: i64 = row.get(offset + 3).map_err(|e| XmppError::internal(e.to_string()))?;
    Ok(Subscription {
        subid: SubId::from_raw(subid_raw),
        subscriber: subscriber_raw.parse().map_err(|e: jid::Error| XmppError::internal(e.to_string()))?,
        state: state_raw.parse().unwrap_or(SubscriptionState::Subscribed),
        created_at_ms,
    })
}
```

Note: `decode_subscription` and `decode_subscription_offset` are free functions, not methods on `DatabasePubSubStorage`. Place them above the existing `pub async fn build_pubsub_storage(...)`.

- [ ] **Step 7: Add `use` declarations**

At the top of the file, add to existing `use` block:

```rust
use jid::Jid;
use waddle_xmpp::pubsub::{Affiliation, SubId, Subscription, SubscriptionState};
```

- [ ] **Step 8: Build clean**

```bash
cd server && cargo build --package waddle-server
```

Expected: clean build.

- [ ] **Step 9: Add restart-persistence test for subscriptions**

In the existing `mod tests` block in `pubsub.rs`, append:

```rust
#[tokio::test]
async fn database_subscriptions_persist_across_reopen() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let path = artifacts.join(format!("pubsub-sub-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let owner = jid("alice@example.com");
    let alice: jid::Jid = "alice@example.com".parse().unwrap();

    let saved_subid = {
        let storage = DatabasePubSubStorage::open(Some(&url)).await.unwrap();
        let (_, _) = storage.get_or_create_node(&owner, "n").await.unwrap();
        let sub = storage.subscribe(&owner, "n", &alice).await.unwrap();
        sub.subid
    };

    let reopened = DatabasePubSubStorage::open(Some(&url)).await.unwrap();
    let listed = reopened.list_node_subscriptions(&owner, "n").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].subid, saved_subid);

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

#[tokio::test]
async fn database_affiliations_persist_across_reopen() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let path = artifacts.join(format!("pubsub-aff-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let owner = jid("alice@example.com");
    let bob = jid("bob@example.com");

    {
        let storage = DatabasePubSubStorage::open(Some(&url)).await.unwrap();
        storage.get_or_create_node(&owner, "n").await.unwrap();
        let prev = storage.set_affiliation(&owner, "n", &bob, Affiliation::Outcast).await.unwrap();
        assert_eq!(prev, Affiliation::None);
    }

    let reopened = DatabasePubSubStorage::open(Some(&url)).await.unwrap();
    let aff = reopened.get_affiliation(&owner, "n", &bob).await.unwrap();
    assert_eq!(aff, Affiliation::Outcast);

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

#[tokio::test]
async fn database_purge_clears_items_keeps_node() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:")).await.unwrap();
    let owner = jid("alice@example.com");
    storage.update_node_config(&owner, "n", &NodeConfig::spaces_public()).await.ok();
    storage.get_or_create_node(&owner, "n").await.unwrap();
    storage.update_node_config(&owner, "n", &NodeConfig::spaces_public()).await.unwrap();
    for i in 1..=3 {
        let item = PubSubItem { id: Some(format!("i{i}")), payload: None };
        storage.publish_item(&owner, "n", &item, None, false).await.unwrap();
    }
    let purged = storage.purge_node(&owner, "n").await.unwrap();
    assert_eq!(purged, 3);
    assert!(storage.get_node(&owner, "n").await.unwrap().is_some());
    assert!(storage.get_items(&owner, "n", None, &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn database_deliverable_subscribers_excludes_outcast() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:")).await.unwrap();
    let owner = jid("alice@example.com");
    storage.get_or_create_node(&owner, "n").await.unwrap();
    let alice: jid::Jid = "alice@x.com".parse().unwrap();
    let bob: jid::Jid = "bob@x.com".parse().unwrap();
    storage.subscribe(&owner, "n", &alice).await.unwrap();
    storage.subscribe(&owner, "n", &bob).await.unwrap();
    let bob_bare = jid("bob@x.com");
    storage.set_affiliation(&owner, "n", &bob_bare, Affiliation::Outcast).await.unwrap();

    let listed = storage.list_deliverable_subscribers(&owner, "n").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].subscriber.to_string(), "alice@x.com");
}
```

- [ ] **Step 10: Run tests**

```bash
cd server && cargo test --package waddle-server pubsub
```

Expected: all pass (existing + 4 new).

- [ ] **Step 11: Commit**

```bash
git add server/crates/waddle-server/src/pubsub.rs
git commit -m "feat(server): implement subscriptions/affiliations/purge on DatabasePubSubStorage"
```

---

## Task 9: `pubsub_authz` module

**Files:**
- Create: `server/crates/waddle-server/src/pubsub_authz.rs`
- Modify: `server/crates/waddle-server/src/lib.rs`

This module composes data primitives from the storage trait into XMPP semantics. It does NOT add any trait methods; it works against the existing `dyn PubSubStorage`.

- [ ] **Step 1: Write the module**

Create `server/crates/waddle-server/src/pubsub_authz.rs`:

```rust
//! XEP-0060 / XEP-0163 authorization layered on top of `PubSubStorage` data
//! primitives. Storage knows nothing about XMPP semantics; this module knows
//! about access models, owner derivation, and outcast enforcement.

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::XmppError;
use waddle_xmpp_core::pubsub::{Affiliation, AccessModel, PublishModel};

/// Owner-derivation rule for PEP nodes (XEP-0163 §1).
///
/// For PEP, the node owner is the bare JID matching the target JID. For
/// non-PEP (Spaces, MUC#user) nodes, owner status is established by an
/// explicit affiliation row.
pub fn derive_pep_owner(target: &BareJid, entity: &BareJid) -> bool {
    target == entity
}

/// Resolve the effective affiliation: explicit row, falling back to derived
/// owner for PEP nodes (target_jid is the node owner / namespace owner —
/// for PEP it's the user JID hosting the node tree).
pub async fn effective_affiliation(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<Affiliation, XmppError> {
    let stored = storage.get_affiliation(target, node, entity).await?;
    if stored != Affiliation::None {
        return Ok(stored);
    }
    if is_pep && derive_pep_owner(target, entity) {
        return Ok(Affiliation::Owner);
    }
    Ok(Affiliation::None)
}

/// Whether `entity` is permitted to subscribe to a node (XEP-0060 §6.1).
///
/// `is_pep` controls owner-derivation. `roster_check` is invoked for
/// `AccessModel::Presence` and `AccessModel::Roster`; pass `None` for now
/// (those access models will deny by default until presence-driven delivery
/// ships in a separate issue).
pub async fn can_subscribe(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<bool, XmppError> {
    let aff = effective_affiliation(storage, target, node, entity, is_pep).await?;
    if aff.is_outcast() {
        return Ok(false);
    }
    let Some(node_meta) = storage.get_node(target, node).await? else {
        return Ok(false);
    };
    match node_meta.config.access_model {
        AccessModel::Open => Ok(true),
        AccessModel::Whitelist => Ok(matches!(
            aff,
            Affiliation::Owner | Affiliation::Publisher | Affiliation::Member
        )),
        // Presence/Roster require live roster+presence integration; defer.
        // Owners always pass.
        AccessModel::Presence | AccessModel::Roster => {
            Ok(matches!(aff, Affiliation::Owner) || derive_pep_owner(target, entity) && is_pep)
        }
        AccessModel::Authorize => Ok(matches!(aff, Affiliation::Owner)),
    }
}

/// Whether `entity` is permitted to publish to a node (XEP-0060 §7.1.3).
pub async fn can_publish(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<bool, XmppError> {
    let aff = effective_affiliation(storage, target, node, entity, is_pep).await?;
    if aff.is_outcast() {
        return Ok(false);
    }
    let Some(node_meta) = storage.get_node(target, node).await? else {
        return Ok(false);
    };
    if matches!(aff, Affiliation::Owner) {
        return Ok(true);
    }
    match node_meta.config.publish_model {
        PublishModel::Open => Ok(true),
        PublishModel::Publishers => Ok(aff.can_publish_default()),
        PublishModel::Subscribers => {
            // Treat any non-outcast subscription as publish-eligible.
            let has_sub = !storage
                .list_subscriber_subscriptions(target, &jid::Jid::Bare(entity.clone()))
                .await?
                .is_empty();
            Ok(has_sub || aff.can_publish_default())
        }
    }
}

/// Whether `entity` is permitted to configure or delete a node (owner only).
pub async fn can_administer(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<bool, XmppError> {
    let aff = effective_affiliation(storage, target, node, entity, is_pep).await?;
    Ok(matches!(aff, Affiliation::Owner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::pubsub::InMemoryPubSubStorage;
    use waddle_xmpp_core::pubsub::NodeConfig;

    fn jid(s: &str) -> BareJid {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn pep_owner_is_self() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        storage.get_or_create_node(&alice, "urn:xmpp:bookmarks:1").await.unwrap();

        let aff = effective_affiliation(&storage, &alice, "urn:xmpp:bookmarks:1", &alice, true).await.unwrap();
        assert_eq!(aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn explicit_owner_overrides_derived() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");
        storage.get_or_create_node(&alice, "n").await.unwrap();
        storage.set_affiliation(&alice, "n", &bob, Affiliation::Owner).await.unwrap();

        let aff = effective_affiliation(&storage, &alice, "n", &bob, true).await.unwrap();
        assert_eq!(aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn outcast_cannot_subscribe_to_open_node() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");
        storage.get_or_create_node(&alice, "n").await.unwrap();
        storage.update_node_config(&alice, "n", &NodeConfig::public()).await.unwrap();
        storage.set_affiliation(&alice, "n", &bob, Affiliation::Outcast).await.unwrap();

        assert!(!can_subscribe(&storage, &alice, "n", &bob, false).await.unwrap());
    }

    #[tokio::test]
    async fn whitelist_denies_random_subscriber() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");
        storage.get_or_create_node(&alice, "n").await.unwrap();
        storage.update_node_config(&alice, "n", &NodeConfig::whitelist()).await.unwrap();

        assert!(!can_subscribe(&storage, &alice, "n", &bob, false).await.unwrap());
        storage.set_affiliation(&alice, "n", &bob, Affiliation::Member).await.unwrap();
        assert!(can_subscribe(&storage, &alice, "n", &bob, false).await.unwrap());
    }

    #[tokio::test]
    async fn pep_owner_can_publish() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        storage.get_or_create_node(&alice, "n").await.unwrap();
        assert!(can_publish(&storage, &alice, "n", &alice, true).await.unwrap());
    }
}
```

Add to `server/crates/waddle-server/src/lib.rs`:

```rust
pub mod pubsub_authz;
```

- [ ] **Step 2: Run tests**

```bash
cd server && cargo test --package waddle-server --lib pubsub_authz
```

Expected: 5 passed.

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-server/src/pubsub_authz.rs server/crates/waddle-server/src/lib.rs
git commit -m "feat(server): add pubsub_authz module composing storage primitives"
```

---

## Task 10: Extend stanza parsers/builders for Purge / Affiliations / Configure-set / Subscribe-result with subid

**Files:**
- Modify: `server/crates/waddle-xmpp-core/src/pubsub/stanzas.rs`

The existing `PubSubRequest` enum needs three new variants and one extension. The existing `Subscribe` parser doesn't return `subid` because subscribe is a request shape — subid lives on the *response*, which we build via a new builder.

- [ ] **Step 1: Add new variants to `PubSubRequest`**

Inside the `pub enum PubSubRequest` block, add:

```rust
    /// XEP-0060 §8.5 `<purge node='...'/>` (owner-only).
    PurgeNode { node: String },

    /// XEP-0060 §6.4 `<configure node='...'><x.../>` (owner-only, set form).
    ConfigureNodeSet {
        node: String,
        config: NodeConfig,
    },

    /// XEP-0060 §8.9 `<affiliations node='...'/>` get on owner namespace.
    AffiliationsGet { node: String },

    /// XEP-0060 §8.9.4 `<affiliations node='...'><affiliation jid='...' affiliation='...'/></affiliations>` set.
    AffiliationsSet {
        node: String,
        changes: Vec<(jid::BareJid, Affiliation)>,
    },
```

Add `use` at the top of the file:

```rust
use crate::pubsub::{Affiliation, NodeConfig};
```

- [ ] **Step 2: Extend the parser**

Inside `parse_pubsub_iq`, after the existing `unsubscribe` branch, add (the `<purge>` element lives in `NS_PUBSUB_OWNER`):

```rust
    if let Some(purge) = pubsub_elem.get_child("purge", NS_PUBSUB_OWNER) {
        return Ok(PubSubRequest::PurgeNode {
            node: required_attr(purge, "node")?,
        });
    }

    if let Some(affs) = pubsub_elem.get_child("affiliations", NS_PUBSUB_OWNER) {
        let node = required_attr(affs, "node")?;
        let mut changes: Vec<(jid::BareJid, Affiliation)> = Vec::new();
        for child in affs.children().filter(|c| c.is("affiliation", NS_PUBSUB_OWNER)) {
            let entity_raw = required_attr(child, "jid")?;
            let entity: jid::BareJid = entity_raw
                .parse()
                .map_err(|e: jid::Error| CoreError::bad_request(Some(e.to_string())))?;
            let aff_raw = required_attr(child, "affiliation")?;
            let aff: Affiliation = aff_raw
                .parse()
                .map_err(|_| CoreError::bad_request(Some(format!("invalid affiliation: {aff_raw}"))))?;
            changes.push((entity, aff));
        }
        if changes.is_empty() {
            return Ok(PubSubRequest::AffiliationsGet { node });
        }
        return Ok(PubSubRequest::AffiliationsSet { node, changes });
    }
```

For the configure-set, replace the existing `configure` branch with:

```rust
    if let Some(configure) = pubsub_elem.get_child("configure", NS_PUBSUB_OWNER) {
        let node = required_attr(configure, "node")?;
        if let Some(form) = configure.get_child("x", "jabber:x:data") {
            let config = parse_configure_form(form)?;
            return Ok(PubSubRequest::ConfigureNodeSet { node, config });
        }
        return Ok(PubSubRequest::ConfigureNode { node });
    }
```

(Note the namespace change: the existing `configure` branch checks `NS_PUBSUB`; the spec puts owner-only `<configure>` under `NS_PUBSUB_OWNER`. Verify by grepping the existing code — if both paths are needed, keep both branches in `parse_pubsub_iq` and union them.)

Add the form parser (free function in the same file, near the bottom):

```rust
fn parse_configure_form(form: &minidom::Element) -> CoreResult<NodeConfig> {
    use crate::pubsub::node::{AccessModel, PublishModel, SendLastPublishedItem};

    let mut config = NodeConfig::default();
    for field in form.children().filter(|c| c.is("field", "jabber:x:data")) {
        let var = field.attr("var").unwrap_or("");
        let value = field
            .get_child("value", "jabber:x:data")
            .map(|v| v.text())
            .unwrap_or_default();
        match var {
            "pubsub#access_model" => config.access_model = value.parse().unwrap_or(AccessModel::Presence),
            "pubsub#publish_model" => config.publish_model = value.parse().unwrap_or(PublishModel::Publishers),
            "pubsub#max_items" => {
                if let Ok(n) = value.parse::<u32>() {
                    config.max_items = n;
                }
            }
            "pubsub#persist_items" => config.persist_items = matches!(value.as_str(), "1" | "true"),
            "pubsub#deliver_payloads" => config.deliver_payloads = matches!(value.as_str(), "1" | "true"),
            "pubsub#notify_retract" => config.notify_retract = matches!(value.as_str(), "1" | "true"),
            "pubsub#notify_delete" => config.notify_delete = matches!(value.as_str(), "1" | "true"),
            "pubsub#send_last_published_item" => {
                config.send_last_published_item = value.parse().unwrap_or(SendLastPublishedItem::OnSubAndPresence);
            }
            _ => {} // Unknown fields ignored per XEP-0060.
        }
    }
    Ok(config)
}
```

- [ ] **Step 3: Add subscribe-result builder**

Append to the file:

```rust
/// Build a `<subscribe/>` result IQ that carries `subscription` (XEP-0060 §6.1.6).
pub fn build_pubsub_subscribe_result(
    original_iq: &Iq,
    node: &str,
    subscriber: &jid::Jid,
    subid: &crate::pubsub::SubId,
) -> Iq {
    let subscription = Element::builder("subscription", NS_PUBSUB)
        .attr("node", node)
        .attr("jid", subscriber.to_string())
        .attr("subid", subid.to_string())
        .attr("subscription", "subscribed")
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(subscription)
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build an `<affiliations/>` result IQ for `<affiliations node='...'/>` get.
pub fn build_pubsub_affiliations_result(
    original_iq: &Iq,
    node: &str,
    rows: &[(jid::BareJid, crate::pubsub::Affiliation)],
) -> Iq {
    let mut affs = Element::builder("affiliations", NS_PUBSUB_OWNER).attr("node", node);
    for (entity, aff) in rows {
        affs = affs.append(
            Element::builder("affiliation", NS_PUBSUB_OWNER)
                .attr("jid", entity.to_string())
                .attr("affiliation", aff.to_string())
                .build(),
        );
    }
    let pubsub = Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(affs.build())
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}
```

- [ ] **Step 4: Update existing parser/dispatcher tests**

Find tests that match on `PubSubRequest` (e.g., `PubSubRequest::ConfigureNode`) — add explicit arms returning `()` or `unreachable!()` for the new variants where compilation requires exhaustiveness.

- [ ] **Step 5: Build clean**

```bash
cd server && cargo build --package waddle-xmpp-core --package waddle-xmpp --package waddle-server
```

Expected: clean build (handler match-arms not yet wired — that comes in Task 11+).

- [ ] **Step 6: Commit**

```bash
git add server/crates/waddle-xmpp-core/src/pubsub/stanzas.rs
git commit -m "feat(server): parse purge/affiliations/configure-set; add subid + affiliations builders"
```

---

## Task 11: Migrate Subscribe/Unsubscribe handlers from DashSet to storage; return subid

**Files:**
- Modify: `server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs`

- [ ] **Step 1: Replace the `Subscribe` arm**

In `handlers/iq.rs:1647`, replace the existing `PubSubRequest::Subscribe` block with:

```rust
PubSubRequest::Subscribe { node, jid } => {
    let subscription_jid = jid.to_bare();
    if subscription_jid != user_jid {
        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
        return vec![iq_to_xml(error)];
    }

    let is_pep = is_pep_request_to(&target_jid, &iq);
    match crate::pubsub_authz::can_subscribe(
        &state.deps.protocol.pubsub_storage,
        &target_jid,
        &node,
        &subscription_jid,
        is_pep,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            let error = build_pubsub_error(&iq, PubSubError::Forbidden);
            return vec![iq_to_xml(error)];
        }
        Err(e) => {
            warn!("PubSub access check failed: {e}");
            let error = build_pubsub_error(&iq, PubSubError::Forbidden);
            return vec![iq_to_xml(error)];
        }
    }

    match state
        .deps
        .protocol
        .pubsub_storage
        .subscribe(&target_jid, &node, &jid)
        .await
    {
        Ok(sub) => {
            let response = build_pubsub_subscribe_result(&iq, &node, &jid, &sub.subid);
            vec![iq_to_xml(response)]
        }
        Err(e) => {
            warn!("PubSub subscribe failed: {e}");
            let error = build_pubsub_error(&iq, PubSubError::Forbidden);
            vec![iq_to_xml(error)]
        }
    }
}
```

- [ ] **Step 2: Replace the `Unsubscribe` arm**

```rust
PubSubRequest::Unsubscribe { node, jid, subid } => {
    let subscription_jid = jid.to_bare();
    if subscription_jid != user_jid {
        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
        return vec![iq_to_xml(error)];
    }
    let typed_subid = subid.as_deref().map(SubId::from_raw);
    match state
        .deps
        .protocol
        .pubsub_storage
        .unsubscribe(&target_jid, &node, &jid, typed_subid.as_ref())
        .await
    {
        Ok(true) => {
            let response = build_pubsub_success(&iq);
            vec![iq_to_xml(response)]
        }
        Ok(false) => {
            let error = build_pubsub_error(&iq, PubSubError::NotSubscribed);
            vec![iq_to_xml(error)]
        }
        Err(e) => {
            warn!("PubSub unsubscribe failed: {e}");
            let error = build_pubsub_error(&iq, PubSubError::NotSubscribed);
            vec![iq_to_xml(error)]
        }
    }
}
```

Add `use waddle_xmpp::pubsub::SubId;` and `use waddle_xmpp_core::pubsub::stanzas::build_pubsub_subscribe_result;` at the top of the file (next to existing pubsub imports).

(`is_pep_request_to` is already a public re-export from `waddle_xmpp::pubsub` — check the existing imports in this file and reuse.)

- [ ] **Step 3: Build clean**

```bash
cd server && cargo build --package waddle-server
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs
git commit -m "feat(server): migrate pubsub subscribe/unsubscribe to durable storage"
```

---

## Task 12: Wire `<purge/>` handler

**Files:**
- Modify: `server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs`

- [ ] **Step 1: Add a new arm to the `match request` block**

```rust
PubSubRequest::PurgeNode { node } => {
    let is_pep = is_pep_request_to(&target_jid, &iq);
    match crate::pubsub_authz::can_administer(
        &state.deps.protocol.pubsub_storage,
        &target_jid,
        &node,
        &user_jid,
        is_pep,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            let error = build_pubsub_error(&iq, PubSubError::Forbidden);
            return vec![iq_to_xml(error)];
        }
        Err(e) => {
            warn!("PubSub purge authz failed: {e}");
            let error = build_pubsub_error(&iq, PubSubError::Forbidden);
            return vec![iq_to_xml(error)];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .purge_node(&target_jid, &node)
        .await
    {
        Ok(_) => vec![iq_to_xml(build_pubsub_success(&iq))],
        Err(e) => {
            warn!("PubSub purge failed: {e}");
            vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::NodeNotFound))]
        }
    }
}
```

- [ ] **Step 2: Build clean**

```bash
cd server && cargo build --package waddle-server
```

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs
git commit -m "feat(server): wire pubsub <purge/> handler"
```

---

## Task 13: Wire `<configure/>` get + set handlers

**Files:**
- Modify: `server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs`

- [ ] **Step 1: Replace the existing `ConfigureNode` arm and add `ConfigureNodeSet`**

The existing handler treats `ConfigureNode` as a stub; replace it. The configure-get response carries a `<x type='form'>` data form describing current config (XEP-0060 §6.4 example 158). For brevity in this PR, return a minimal form populated with current values:

```rust
PubSubRequest::ConfigureNode { node } => {
    let is_pep = is_pep_request_to(&target_jid, &iq);
    if !crate::pubsub_authz::can_administer(
        &state.deps.protocol.pubsub_storage,
        &target_jid,
        &node,
        &user_jid,
        is_pep,
    )
    .await
    .unwrap_or(false)
    {
        return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
    }
    let Some(node_meta) = state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&target_jid, &node)
        .await
        .ok()
        .flatten()
    else {
        return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::NodeNotFound))];
    };
    let response = waddle_xmpp_core::pubsub::stanzas::build_pubsub_configure_form_result(
        &iq,
        &node,
        &node_meta.config,
    );
    vec![iq_to_xml(response)]
}

PubSubRequest::ConfigureNodeSet { node, config } => {
    let is_pep = is_pep_request_to(&target_jid, &iq);
    if !crate::pubsub_authz::can_administer(
        &state.deps.protocol.pubsub_storage,
        &target_jid,
        &node,
        &user_jid,
        is_pep,
    )
    .await
    .unwrap_or(false)
    {
        return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(&target_jid, &node, &config)
        .await
    {
        Ok(_) => vec![iq_to_xml(build_pubsub_success(&iq))],
        Err(_) => vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::NodeNotFound))],
    }
}
```

- [ ] **Step 2: Add the form-result builder**

Append to `server/crates/waddle-xmpp-core/src/pubsub/stanzas.rs`:

```rust
/// Build the result for a `<configure/>` get carrying current node config
/// as a `<x type='form'/>` data form (XEP-0060 §6.4).
pub fn build_pubsub_configure_form_result(
    original_iq: &Iq,
    node: &str,
    config: &NodeConfig,
) -> Iq {
    fn field(var: &str, value: &str) -> Element {
        Element::builder("field", "jabber:x:data")
            .attr("var", var)
            .append(
                Element::builder("value", "jabber:x:data")
                    .append(value)
                    .build(),
            )
            .build()
    }
    let form = Element::builder("x", "jabber:x:data")
        .attr("type", "form")
        .append(field("FORM_TYPE", "http://jabber.org/protocol/pubsub#node_config"))
        .append(field("pubsub#access_model", &config.access_model.to_string()))
        .append(field("pubsub#publish_model", &config.publish_model.to_string()))
        .append(field("pubsub#max_items", &config.max_items.to_string()))
        .append(field("pubsub#persist_items", if config.persist_items { "1" } else { "0" }))
        .append(field("pubsub#deliver_payloads", if config.deliver_payloads { "1" } else { "0" }))
        .append(field("pubsub#notify_retract", if config.notify_retract { "1" } else { "0" }))
        .append(field("pubsub#notify_delete", if config.notify_delete { "1" } else { "0" }))
        .append(field(
            "pubsub#send_last_published_item",
            &config.send_last_published_item.to_string(),
        ))
        .build();
    let configure = Element::builder("configure", NS_PUBSUB_OWNER)
        .attr("node", node)
        .append(form)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(configure)
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}
```

- [ ] **Step 3: Build clean**

```bash
cd server && cargo build --package waddle-server
```

- [ ] **Step 4: Commit**

```bash
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs server/crates/waddle-xmpp-core/src/pubsub/stanzas.rs
git commit -m "feat(server): wire pubsub <configure/> get/set handlers"
```

---

## Task 14: Wire `<affiliations/>` get + set handlers

**Files:**
- Modify: `server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs`

- [ ] **Step 1: Add new arms**

```rust
PubSubRequest::AffiliationsGet { node } => {
    let is_pep = is_pep_request_to(&target_jid, &iq);
    if !crate::pubsub_authz::can_administer(
        &state.deps.protocol.pubsub_storage,
        &target_jid,
        &node,
        &user_jid,
        is_pep,
    )
    .await
    .unwrap_or(false)
    {
        return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
    }
    let rows = state
        .deps
        .protocol
        .pubsub_storage
        .list_node_affiliations(&target_jid, &node)
        .await
        .unwrap_or_default();
    let response = waddle_xmpp_core::pubsub::stanzas::build_pubsub_affiliations_result(&iq, &node, &rows);
    vec![iq_to_xml(response)]
}

PubSubRequest::AffiliationsSet { node, changes } => {
    let is_pep = is_pep_request_to(&target_jid, &iq);
    if !crate::pubsub_authz::can_administer(
        &state.deps.protocol.pubsub_storage,
        &target_jid,
        &node,
        &user_jid,
        is_pep,
    )
    .await
    .unwrap_or(false)
    {
        return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
    }
    for (entity, aff) in &changes {
        if let Err(e) = state
            .deps
            .protocol
            .pubsub_storage
            .set_affiliation(&target_jid, &node, entity, *aff)
            .await
        {
            warn!("set_affiliation failed: {e}");
            return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
        }
    }
    vec![iq_to_xml(build_pubsub_success(&iq))]
}
```

- [ ] **Step 2: Build clean and commit**

```bash
cd server && cargo build --package waddle-server
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs
git commit -m "feat(server): wire pubsub <affiliations/> get/set handlers"
```

---

## Task 15: Add access-model + outcast enforcement to Publish handler

**Files:**
- Modify: `server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs`

- [ ] **Step 1: Find the existing `Publish` arm and prepend an authz check**

Locate `PubSubRequest::Publish { node, item }` in `handlers/iq.rs`. Before the call to `pubsub_storage.publish_item(...)`, add:

```rust
let is_pep = is_pep_request_to(&target_jid, &iq);
if !crate::pubsub_authz::can_publish(
    &state.deps.protocol.pubsub_storage,
    &target_jid,
    &node,
    &user_jid,
    is_pep,
)
.await
.unwrap_or(false)
{
    return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
}
```

- [ ] **Step 2: Build clean and commit**

```bash
cd server && cargo build --package waddle-server
git add server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs
git commit -m "fix(server): enforce pubsub access-model on publish"
```

---

## Task 16: Remove the `pubsub_subscriptions` DashSet

**Files:**
- Modify: `server/crates/waddle-server/src/server/routes/websocket/mod.rs`
- Modify: `server/crates/waddle-server/src/server/mod.rs`

- [ ] **Step 1: Drop the field**

In `server/crates/waddle-server/src/server/routes/websocket/mod.rs:119`, remove the line:

```rust
    pub pubsub_subscriptions: Arc<dashmap::DashSet<(String, String, String)>>,
```

In the same file at line 1841, remove:

```rust
                    pubsub_subscriptions: Arc::new(dashmap::DashSet::new()),
```

In `server/crates/waddle-server/src/server/mod.rs:896`, remove:

```rust
                pubsub_subscriptions: Arc::new(dashmap::DashSet::new()),
```

- [ ] **Step 2: Build clean and run all server tests**

```bash
cd server && cargo build --package waddle-server && cargo test --package waddle-server
```

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-server/src/server/routes/websocket/mod.rs server/crates/waddle-server/src/server/mod.rs
git commit -m "feat(server): drop in-memory pubsub_subscriptions DashSet"
```

---

## Task 17: Gate in-memory fallback behind `WADDLE_PUBSUB_INMEMORY=1`

**Files:**
- Modify: `server/crates/waddle-server/src/pubsub.rs`
- Modify: `server/crates/waddle-server/src/server/mod.rs`

- [ ] **Step 1: Replace `build_pubsub_storage`**

In `pubsub.rs`, replace the existing function with:

```rust
pub async fn build_pubsub_storage(
    database_url: Option<String>,
) -> Result<Arc<dyn PubSubStorage>, XmppError> {
    if let Some(url) = database_url {
        return Ok(Arc::new(DatabasePubSubStorage::open(Some(&url)).await?));
    }
    if std::env::var("WADDLE_PUBSUB_INMEMORY").is_ok_and(|v| v == "1") {
        return Ok(Arc::new(DatabasePubSubStorage::open(None).await?));
    }
    Err(XmppError::config(
        "WADDLE_XMPP_PUBSUB_DATABASE_URL is required for production durability; \
         set WADDLE_PUBSUB_INMEMORY=1 to opt into ephemeral storage for dev/test"
            .to_string(),
    ))
}
```

- [ ] **Step 2: Make `server/mod.rs` propagate the error**

In `server/mod.rs:862`, change `unwrap_or_else(|error| panic!(...))` to a propagating `?`:

```rust
let pubsub_storage = build_pubsub_storage(xmpp_config.pubsub_database_url.clone()).await?;
```

(If the surrounding function does not return `Result<..., XmppError>`, wrap appropriately. Read 30 lines of context first.)

- [ ] **Step 3: Add a test that asserts the error**

In `pubsub.rs::tests` mod, add:

```rust
#[tokio::test]
async fn build_pubsub_storage_errors_without_url_or_envvar() {
    std::env::remove_var("WADDLE_PUBSUB_INMEMORY");
    let result = build_pubsub_storage(None).await;
    assert!(result.is_err());
}
```

(Test fixtures that need in-memory storage already construct `DatabasePubSubStorage::open(Some("sqlite::memory:"))` directly; nothing else needs the env var.)

- [ ] **Step 4: Build, test, commit**

```bash
cd server && cargo test --package waddle-server pubsub::tests::build_pubsub_storage_errors_without_url_or_envvar
git add server/crates/waddle-server/src/pubsub.rs server/crates/waddle-server/src/server/mod.rs
git commit -m "fix(server): require pubsub database URL in production by default"
```

---

## Task 18: Add `WADDLE_XMPP_PUBSUB_DATABASE_URL` to helmrelease

**Files:**
- Modify: `infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml`

- [ ] **Step 1: Add the env var**

After the existing `WADDLE_XMPP_INBOX_DATABASE_URL` block, add:

```yaml
      - name: WADDLE_XMPP_PUBSUB_DATABASE_URL
        valueFrom:
          secretKeyRef:
            name: postgresql-app
            key: uri
```

After `xmppInboxDatabaseUrl: "..."` in the `databaseUrl` config block (around line 113), add:

```yaml
      xmppPubsubDatabaseUrl: "postgresql://postgresql-rw:5432/waddle"
```

- [ ] **Step 2: Verify yaml parses**

```bash
yq '.' infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
```

- [ ] **Step 3: Commit**

```bash
git add infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
git commit -m "feat(server): wire WADDLE_XMPP_PUBSUB_DATABASE_URL in helmrelease"
```

---

## Task 19: L3 wire-conformance tests for XEP-0060

**Files:**
- Create: `server/crates/waddle-server/tests/xep0060_pubsub_ws.rs`

This is the dedicated XEP-0060 test suite required by CLAUDE.md's XEP custom-test-suite hard rule. Use the existing `tests/ws_common` harness pattern from `xep0313_mam_integration.rs` as a template.

- [ ] **Step 1: Read the existing harness**

```bash
cd server && head -120 crates/waddle-server/tests/xep0313_mam_integration.rs
```

Confirm: it spawns a websocket server with `ws_common::TestServer::new()`, opens authenticated streams as Alice + Bob, sends stanzas via `client.send_iq(...)`, awaits responses.

- [ ] **Step 2: Create the test file with a minimum of six wire-conformance cases**

```rust
//! XEP-0060 PubSub wire conformance.

mod ws_common;

use ws_common::{TestServer, send_iq};

#[tokio::test]
async fn create_node_then_subscribe_returns_subid() {
    let server = TestServer::new().await;
    let alice = server.connect_user("alice").await;
    // <iq><pubsub><create node='public'/></pubsub></iq>
    let create_resp = send_iq(&alice, /* node creation IQ */ todo_replace_with_helper("create-public")).await;
    assert!(create_resp.is_result());

    // Bob subscribes.
    let bob = server.connect_user("bob").await;
    let sub_resp = send_iq(&bob, /* subscribe IQ */ todo_replace_with_helper("subscribe-public")).await;
    let subid = sub_resp.payload_attr("//pubsub/subscription", "subid").unwrap();
    assert!(!subid.is_empty(), "subid must be returned per XEP-0060 §6.1.6");
}

#[tokio::test]
async fn unsubscribe_with_subid_succeeds_and_repeated_unsub_fails() { /* ... */ }

#[tokio::test]
async fn publish_to_open_node_then_retrieve_returns_oldest_first() { /* ... */ }

#[tokio::test]
async fn publish_respects_max_items_trim() { /* ... */ }

#[tokio::test]
async fn outcast_publisher_is_forbidden() { /* ... */ }

#[tokio::test]
async fn purge_clears_items_keeps_node() { /* ... */ }

#[tokio::test]
async fn whitelist_node_denies_random_subscriber() { /* ... */ }
```

(Replace `todo_replace_with_helper` with concrete IQ stanza builders. The exact helper API depends on what `ws_common` already exposes; mirror what `xep0313_mam_integration.rs` does for stanza construction. If `ws_common` lacks helpers, add them as small free functions in `ws_common::pubsub` rather than inline in each test.)

Each test must construct stanzas with `xmpp_parsers::Iq` builders, never `format!` strings, per CLAUDE.md.

- [ ] **Step 3: Run the test suite**

```bash
cd server && cargo test --package waddle-server --test xep0060_pubsub_ws
```

Expected: all pass. If `ws_common` is missing affordances (e.g., a `payload_attr` accessor), add them in this task and document the addition in the commit message.

- [ ] **Step 4: Commit**

```bash
git add server/crates/waddle-server/tests/xep0060_pubsub_ws.rs server/crates/waddle-server/tests/ws_common/
git commit -m "test(server): add XEP-0060 wire conformance suite"
```

---

## Task 20: L3 wire-conformance tests for XEP-0163 (PEP)

**Files:**
- Create: `server/crates/waddle-server/tests/xep0163_pep_ws.rs`

- [ ] **Step 1: Create the file**

```rust
//! XEP-0163 PEP wire conformance.

mod ws_common;

#[tokio::test]
async fn pep_owner_can_publish_to_self_node_without_explicit_create() {
    // PEP auto-create: any publish to urn:xmpp:bookmarks:1 by the owner JID
    // creates the node with PEP defaults (max_items=1, presence access).
    // Verify that storage now has a node row + the item, and the response
    // is a valid <publish-result/>.
    todo!()
}

#[tokio::test]
async fn pep_other_user_cannot_publish_to_alice_pep_node() {
    // Bob attempts to publish to alice@x.com/urn:xmpp:bookmarks:1 — must
    // receive <forbidden/>, regardless of PEP auto-create.
    todo!()
}

#[tokio::test]
async fn pep_max_items_is_one_by_default() {
    // Publish two items to a fresh PEP node, verify only the second remains.
    todo!()
}

#[tokio::test]
async fn pep_owner_can_purge_self_node() { todo!() }
```

Replace `todo!()` with concrete stanza-driven assertions, mirroring Task 19's structure. The point is to assert the PEP-specific semantics: owner-derivation works without explicit affiliation rows, max_items=1 default, cross-user publish is denied.

- [ ] **Step 2: Run + commit**

```bash
cd server && cargo test --package waddle-server --test xep0163_pep_ws
git add server/crates/waddle-server/tests/xep0163_pep_ws.rs
git commit -m "test(server): add XEP-0163 PEP wire conformance suite"
```

---

## Task 21: Run full workspace test + lint, fix any drift, file follow-up issues

- [ ] **Step 1: Full workspace test**

```bash
cd server && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace --all-targets --locked
```

Expected: clean. Per CLAUDE.md's clippy hard rule, no `#[allow]` attributes are permitted; fix the underlying code instead.

- [ ] **Step 2: File the four follow-up issues**

```bash
gh issue create --repo waddle-social/waddle \
  --title "Test all SQL-backed XMPP stores against postgres in CI" \
  --body "$(cat <<'EOF'
## Problem

MAM, inbox, and PubSub storage backends each support both sqlite and postgres, but CI exercises only the sqlite path. The postgres path is covered in production deployment only, leaving DDL portability bugs and type-coercion drift undetected.

## Scope

- Add a postgres service to the GitHub workflow.
- Run the storage trait conformance tests against both backends.

## Out of scope

- Migrating the existing tests to a different harness.

EOF
)"

gh issue create --repo waddle-social/waddle \
  --title "PubSub publish-time fan-out delivery" \
  --body "Implement subscriber/affiliation-aware fan-out for <publish/> on PubSub and PEP nodes, using PubSubStorage::list_deliverable_subscribers."

gh issue create --repo waddle-social/waddle \
  --title "PubSub retract/delete event notifications" \
  --body "XEP-0060 §7.2, §8.4.1: subscribers and affiliated entities should receive <retract/>/<delete/> event messages on item retraction and node deletion."

gh issue create --repo waddle-social/waddle \
  --title "PEP presence-driven delivery filtering" \
  --body "XEP-0163 §3: PEP delivery is gated by current presence + +notify CAPS. The storage layer is in place; this issue covers the runtime filter."
```

- [ ] **Step 3: Open the draft PR for this branch**

The PR was opened at the start of plan execution (see "Branch + draft PR" in the rollout). Link the four issues in the PR description so reviewers see the deliberately-deferred scope.

- [ ] **Step 4: Final commit**

```bash
git commit --allow-empty -m "chore(server): close out durable pubsub storage with follow-up issues"
git push
```

---

## Self-review checklist

- **Spec coverage:** every issue acceptance criterion is covered by at least one task — durable nodes (Task 6), items (6+7), subscriptions (4+5+8), affiliations (4+5+8), node config (6), last-published-item (6), retract/delete state (6 schema, existing handler), production-mode safety (17), tests (19+20), restart persistence (8 test rows + L2 in 19+20).
- **Type consistency:** all method signatures use `BareJid`/`Jid`/`Affiliation`/`SubId`/`Subscription`/`SubscriptionState` — never `String` for protocol data.
- **Placeholder scan:** the L3 test bodies (Tasks 19, 20) contain `todo!()` stubs because their concrete stanza construction depends on what `ws_common` exposes; the executing engineer is instructed to read that harness first and fill in. This is the only intentional placeholder. All other steps contain executable code.
- **Commit hygiene:** every task ends with a single `feat(server)`/`fix(server)`/`test(server)` commit per CLAUDE.md.
