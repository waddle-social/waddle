//! Admin V1 + V2 — community-owner-gated operations exposed via XEP-0050
//! ad-hoc commands.
//!
//! Hard rules:
//!
//! - **No REST**: admin actions flow over XMPP, specifically XEP-0050.
//! - **Owner gate**: the async helper [`is_community_owner`] is the single
//!   source of truth for "may this JID see admin surfaces?" — every
//!   `urn:waddle:admin:*` command handler MUST call it before doing
//!   anything else and refuse non-owners with `<forbidden/>`.
//! - **Custom namespace**: no XEP defines "list users with prefix
//!   search," so the V1 command lives under
//!   `urn:waddle:admin:users:list:0` per the
//!   "Waddle-namespace only when needed" rule.
//!
//! ## ACL: bootstrap + dynamic signal
//!
//! Authority for "community owner" comes from two layered signals:
//!
//! 1. **Bootstrap fallback** — the static
//!    [`crate::server::AppState::server_owner_jids`] set resolved from
//!    `WADDLE_SERVER_OWNER_LOCALPARTS` at startup. This keeps a fresh
//!    deployment usable before any dynamic signal exists and matches the
//!    JIDs that get auto-seeded as PubSub owners on every Spaces node.
//! 2. **Dynamic signal** — the durable XEP-0060 affiliation table on the
//!    Spaces (XEP-0503) service. If `jid` holds [`Affiliation::Owner`] on
//!    *any* node under `spaces_jid`, it counts as a community owner. This
//!    lets a server owner promote a peer to admin via a normal
//!    `<affiliations/>` set on a Spaces node — no restart required.
//!
//! XEP-0317 hats are intentionally NOT consulted here: per the
//! `waddle_xmpp::xep::xep0317` module docs, hats are descriptive social
//! metadata only and carry no authority.
//!
//! The check is closed-by-default: if the dynamic lookup errors out, the
//! gate denies access and logs a `warn!`. We never grant admin on a
//! storage failure.

pub mod channels;
pub mod spaces;
pub mod users_list;

use jid::BareJid;
use tracing::warn;
use waddle_xmpp::pubsub::Affiliation;

use crate::server::AppState;

/// `true` iff `jid` is the community owner of this deployment.
///
/// Returns `true` if either:
///
/// - `jid` is in the configured [`AppState::server_owner_jids`] bootstrap
///   set, OR
/// - `jid` holds an [`Affiliation::Owner`] row on any PubSub node under
///   [`AppState::spaces_jid`].
///
/// On a storage error from the dynamic lookup, returns `false` (closed by
/// default) and logs at `warn!`. We never grant admin access because of an
/// I/O failure.
///
/// This is the single chokepoint for admin authorization. New admin
/// commands MUST call this (or `caller_or_forbidden` in the per-handler
/// modules, which wraps it) before performing any work.
pub async fn is_community_owner(state: &AppState, jid: &BareJid) -> bool {
    if is_owner_in(&state.server_owner_jids, jid) {
        return true;
    }
    match dynamic_owner_signal(state, jid).await {
        Ok(is_owner) => is_owner,
        Err(error) => {
            warn!(
                jid = %jid,
                spaces_jid = %state.spaces_jid,
                error = %error,
                "dynamic community-owner lookup failed; denying admin access",
            );
            false
        }
    }
}

/// Pure helper used by tests and the public [`is_community_owner`]
/// entry point's bootstrap arm. Walks `owners` and returns `true` on the
/// first JID equal to `jid`.
pub fn is_owner_in(owners: &[BareJid], jid: &BareJid) -> bool {
    owners.iter().any(|owner| owner == jid)
}

/// Query the dynamic community-owner signal: does `jid` hold an explicit
/// [`Affiliation::Owner`] row on any PubSub node under
/// [`AppState::spaces_jid`]?
///
/// Returns `Err` only when the underlying storage call fails. A successful
/// "no Owner row found" returns `Ok(false)`.
async fn dynamic_owner_signal(
    state: &AppState,
    jid: &BareJid,
) -> Result<bool, waddle_xmpp::XmppError> {
    let rows = state
        .pubsub_storage
        .list_entity_affiliations(&state.spaces_jid, jid)
        .await?;
    Ok(rows
        .iter()
        .any(|(_, affiliation)| matches!(affiliation, Affiliation::Owner)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use std::sync::Arc;

    fn jid(s: &str) -> BareJid {
        s.parse().expect("test jid parses")
    }

    #[test]
    fn owner_jid_returns_true() {
        let owners = vec![jid("admin@localhost")];
        assert!(is_owner_in(&owners, &jid("admin@localhost")));
    }

    #[test]
    fn non_owner_jid_returns_false() {
        let owners = vec![jid("admin@localhost")];
        assert!(!is_owner_in(&owners, &jid("alice@localhost")));
    }

    #[test]
    fn empty_owner_set_returns_false_for_anyone() {
        let owners: Vec<BareJid> = vec![];
        assert!(!is_owner_in(&owners, &jid("admin@localhost")));
    }

    #[test]
    fn multiple_owners_match_each_one() {
        let owners = vec![jid("admin@localhost"), jid("root@localhost")];
        assert!(is_owner_in(&owners, &jid("admin@localhost")));
        assert!(is_owner_in(&owners, &jid("root@localhost")));
        assert!(!is_owner_in(&owners, &jid("alice@localhost")));
    }

    async fn fresh_state() -> AppState {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        AppState::new(Arc::new(db_pool))
    }

    fn set_owners(state: &mut AppState, owners: Vec<BareJid>) {
        state.server_owner_jids = Arc::from(owners);
    }

    #[tokio::test]
    async fn bootstrap_owner_is_recognized_without_dynamic_signal() {
        let mut state = fresh_state().await;
        set_owners(&mut state, vec![jid("admin@localhost")]);

        assert!(is_community_owner(&state, &jid("admin@localhost")).await);
        assert!(!is_community_owner(&state, &jid("alice@localhost")).await);
    }

    #[tokio::test]
    async fn dynamic_pubsub_owner_grants_admin_without_restart() {
        let state = fresh_state().await;
        // No bootstrap owners; alice is unknown to the env list.
        let alice = jid("alice@localhost");
        assert!(!is_community_owner(&state, &alice).await);

        // Grant an explicit Owner affiliation on a Spaces node — this is
        // the durable signal the dynamic check consults.
        state
            .pubsub_storage
            .get_or_create_node(&state.spaces_jid, "general")
            .await
            .expect("create node");
        state
            .pubsub_storage
            .set_affiliation(&state.spaces_jid, "general", &alice, Affiliation::Owner)
            .await
            .expect("grant owner");

        assert!(is_community_owner(&state, &alice).await);
    }

    #[tokio::test]
    async fn revoking_dynamic_owner_removes_admin_access() {
        let state = fresh_state().await;
        let alice = jid("alice@localhost");
        state
            .pubsub_storage
            .get_or_create_node(&state.spaces_jid, "general")
            .await
            .expect("create node");
        state
            .pubsub_storage
            .set_affiliation(&state.spaces_jid, "general", &alice, Affiliation::Owner)
            .await
            .expect("grant owner");
        assert!(is_community_owner(&state, &alice).await);

        // Demote to Member — must drop admin access on the next call.
        state
            .pubsub_storage
            .set_affiliation(&state.spaces_jid, "general", &alice, Affiliation::Member)
            .await
            .expect("demote");
        assert!(!is_community_owner(&state, &alice).await);

        // Removing the row entirely also denies.
        state
            .pubsub_storage
            .set_affiliation(&state.spaces_jid, "general", &alice, Affiliation::None)
            .await
            .expect("remove");
        assert!(!is_community_owner(&state, &alice).await);
    }

    #[tokio::test]
    async fn non_owner_affiliation_does_not_grant_admin() {
        let state = fresh_state().await;
        let alice = jid("alice@localhost");
        state
            .pubsub_storage
            .get_or_create_node(&state.spaces_jid, "general")
            .await
            .expect("create node");
        // Publisher is the "admin" tier in the V2 vocab mapping but is
        // explicitly NOT the community-owner signal — only Owner counts.
        state
            .pubsub_storage
            .set_affiliation(&state.spaces_jid, "general", &alice, Affiliation::Publisher)
            .await
            .expect("set publisher");

        assert!(!is_community_owner(&state, &alice).await);
    }

    #[tokio::test]
    async fn bootstrap_owner_short_circuits_before_dynamic_lookup() {
        // Even if the dynamic storage would error out, the bootstrap
        // arm wins before we ever consult storage. We assert this by
        // installing a storage that fails every call and verifying the
        // env-listed JID still gets admin.
        let mut state = fresh_state().await;
        state.pubsub_storage = Arc::new(failing_storage::FailingStorage);
        set_owners(&mut state, vec![jid("admin@localhost")]);

        assert!(is_community_owner(&state, &jid("admin@localhost")).await);
    }

    #[tokio::test]
    async fn dynamic_signal_storage_error_denies_access() {
        // Closed-by-default guarantee: a storage failure on the dynamic
        // lookup must NOT grant admin.
        let mut state = fresh_state().await;
        state.pubsub_storage = Arc::new(failing_storage::FailingStorage);
        // No bootstrap owners — so the dynamic path is the only chance.

        assert!(!is_community_owner(&state, &jid("alice@localhost")).await);
    }

    /// A `PubSubStorage` that fails every call with an internal error. Used
    /// to assert the closed-by-default semantics of [`is_community_owner`]
    /// when the dynamic lookup fails. We implement the full trait surface
    /// because the storage is loaded into `AppState` via an `Arc<dyn …>`;
    /// the only method [`is_community_owner`] actually reaches is
    /// `list_entity_affiliations`, but the other methods need a body so the
    /// trait object is constructible.
    mod failing_storage {
        use async_trait::async_trait;
        use jid::{BareJid, Jid};
        use waddle_xmpp::pubsub::{
            Affiliation, PubSubItem, PubSubNode, PubSubStorage, PublishResult, StoredItem,
        };
        use waddle_xmpp::XmppError;
        use waddle_xmpp_core::pubsub::{SubId, Subscription};

        pub struct FailingStorage;

        fn boom<T>(reason: &str) -> Result<T, XmppError> {
            Err(XmppError::internal(format!(
                "failing storage forced error: {reason}"
            )))
        }

        #[async_trait]
        impl PubSubStorage for FailingStorage {
            async fn get_or_create_node(
                &self,
                _owner: &BareJid,
                _node_name: &str,
            ) -> Result<(PubSubNode, bool), XmppError> {
                boom("get_or_create_node")
            }
            async fn get_node(
                &self,
                _owner: &BareJid,
                _node_name: &str,
            ) -> Result<Option<PubSubNode>, XmppError> {
                boom("get_node")
            }
            async fn delete_node(
                &self,
                _owner: &BareJid,
                _node_name: &str,
            ) -> Result<bool, XmppError> {
                boom("delete_node")
            }
            async fn publish_item(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _item: &PubSubItem,
                _publisher: Option<&BareJid>,
                _auto_create: bool,
            ) -> Result<PublishResult, XmppError> {
                boom("publish_item")
            }
            async fn publish_item_if_missing_or_publisher(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _item: &PubSubItem,
                _publisher: &BareJid,
                _auto_create: bool,
            ) -> Result<PublishResult, XmppError> {
                boom("publish_item_if_missing_or_publisher")
            }
            async fn get_items(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _max_items: Option<u32>,
                _item_ids: &[String],
            ) -> Result<Vec<StoredItem>, XmppError> {
                boom("get_items")
            }
            async fn retract_item(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _item_id: &str,
            ) -> Result<bool, XmppError> {
                boom("retract_item")
            }
            async fn list_nodes(&self, _owner: &BareJid) -> Result<Vec<String>, XmppError> {
                boom("list_nodes")
            }
            async fn find_node_for_item(
                &self,
                _owner: &BareJid,
                _item_id: &str,
            ) -> Result<Option<PubSubNode>, XmppError> {
                boom("find_node_for_item")
            }
            async fn list_node_names_for_item(
                &self,
                _owner: &BareJid,
                _item_id: &str,
            ) -> Result<Vec<String>, XmppError> {
                boom("list_node_names_for_item")
            }
            async fn update_node_config(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _config: &waddle_xmpp::pubsub::NodeConfig,
            ) -> Result<(), XmppError> {
                boom("update_node_config")
            }
            async fn purge_node(
                &self,
                _owner: &BareJid,
                _node_name: &str,
            ) -> Result<u64, XmppError> {
                boom("purge_node")
            }
            async fn subscribe(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _subscriber: &Jid,
            ) -> Result<Subscription, XmppError> {
                boom("subscribe")
            }
            async fn unsubscribe(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _subscriber: &Jid,
                _subid: Option<&SubId>,
            ) -> Result<bool, XmppError> {
                boom("unsubscribe")
            }
            async fn list_node_subscriptions(
                &self,
                _owner: &BareJid,
                _node_name: &str,
            ) -> Result<Vec<Subscription>, XmppError> {
                boom("list_node_subscriptions")
            }
            async fn list_subscriber_subscriptions(
                &self,
                _owner: &BareJid,
                _subscriber: &Jid,
            ) -> Result<Vec<(String, Subscription)>, XmppError> {
                boom("list_subscriber_subscriptions")
            }
            async fn get_subscription(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _subid: &SubId,
            ) -> Result<Option<Subscription>, XmppError> {
                boom("get_subscription")
            }
            async fn list_deliverable_subscribers(
                &self,
                _owner: &BareJid,
                _node_name: &str,
            ) -> Result<Vec<Subscription>, XmppError> {
                boom("list_deliverable_subscribers")
            }
            async fn set_affiliation(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _entity: &BareJid,
                _affiliation: Affiliation,
            ) -> Result<Affiliation, XmppError> {
                boom("set_affiliation")
            }
            async fn get_affiliation(
                &self,
                _owner: &BareJid,
                _node_name: &str,
                _entity: &BareJid,
            ) -> Result<Affiliation, XmppError> {
                boom("get_affiliation")
            }
            async fn list_node_affiliations(
                &self,
                _owner: &BareJid,
                _node_name: &str,
            ) -> Result<Vec<(BareJid, Affiliation)>, XmppError> {
                boom("list_node_affiliations")
            }
            async fn list_entity_affiliations(
                &self,
                _owner: &BareJid,
                _entity: &BareJid,
            ) -> Result<Vec<(String, Affiliation)>, XmppError> {
                boom("list_entity_affiliations")
            }
        }
    }
}
