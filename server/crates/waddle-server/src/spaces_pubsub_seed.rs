//! Seed `Affiliation::Owner` rows on Spaces PubSub nodes for entities that
//! administer Spaces via the Zanzibar permission model (server owners,
//! space creators).
//!
//! XEP-0060 §4.1 puts authorization in the affiliation table; this module
//! mirrors the Zanzibar permission decisions into durable affiliation rows
//! so that `pubsub_authz::can_administer` (which only consults storage) can
//! make the right call for `<configure/>`, `<purge/>`, and `<affiliations/>`
//! operations against the Spaces service.

use std::sync::Arc;

use jid::BareJid;
use tracing::warn;
use waddle_xmpp::pubsub::{Affiliation, PubSubStorage};
use waddle_xmpp::XmppError;

/// Write `Affiliation::Owner` for each entity in `entities` against
/// `(spaces_jid, node)`. Idempotent: re-running over an existing Owner row
/// is a no-op; an existing weaker affiliation is upgraded to Owner.
///
/// Per-entity write failures are logged with `warn!` and do not abort the
/// batch — the seed is a cache of a permission decision and can be repaired
/// by the next reconcile pass.
pub async fn seed_owners_on_node(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    node: &str,
    entities: &[BareJid],
) -> Result<(), XmppError> {
    for entity in entities {
        if let Err(error) = storage
            .set_affiliation(spaces_jid, node, entity, Affiliation::Owner)
            .await
        {
            warn!(
                spaces = %spaces_jid,
                node = %node,
                entity = %entity,
                error = %error,
                "failed to seed Owner affiliation on Spaces node",
            );
        }
    }
    Ok(())
}

/// Write `Affiliation::Owner` for `entity` against every node owned by
/// `spaces_jid`. Used at startup to mirror server-owner permissions across
/// all existing Spaces nodes.
///
/// Per-node write failures are logged with `warn!` and do not abort the
/// batch.
pub async fn seed_owner_on_all_nodes(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    entity: &BareJid,
) -> Result<(), XmppError> {
    let nodes = storage.list_nodes(spaces_jid).await?;
    for node in &nodes {
        if let Err(error) = storage
            .set_affiliation(spaces_jid, node, entity, Affiliation::Owner)
            .await
        {
            warn!(
                spaces = %spaces_jid,
                node = %node,
                entity = %entity,
                error = %error,
                "failed to seed Owner affiliation on Spaces node",
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::pubsub::InMemoryPubSubStorage;

    fn spaces_jid() -> BareJid {
        "spaces.localhost".parse().expect("spaces jid")
    }

    fn jid(local: &str) -> BareJid {
        format!("{local}@localhost").parse().expect("bare jid")
    }

    #[tokio::test]
    async fn seed_owners_on_node_writes_owner_rows() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        storage
            .get_or_create_node(&spaces, "general")
            .await
            .expect("create node");
        let owners = vec![jid("alice"), jid("bob")];

        seed_owners_on_node(&storage, &spaces, "general", &owners)
            .await
            .expect("seed");

        let alice_aff = storage
            .get_affiliation(&spaces, "general", &jid("alice"))
            .await
            .expect("get alice");
        let bob_aff = storage
            .get_affiliation(&spaces, "general", &jid("bob"))
            .await
            .expect("get bob");
        assert_eq!(alice_aff, Affiliation::Owner);
        assert_eq!(bob_aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn seed_owners_on_node_is_idempotent() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        storage
            .get_or_create_node(&spaces, "general")
            .await
            .expect("create node");
        let owners = vec![jid("alice")];

        seed_owners_on_node(&storage, &spaces, "general", &owners)
            .await
            .expect("seed first");
        seed_owners_on_node(&storage, &spaces, "general", &owners)
            .await
            .expect("seed second");

        let aff = storage
            .get_affiliation(&spaces, "general", &jid("alice"))
            .await
            .expect("get aff");
        assert_eq!(aff, Affiliation::Owner);
        let rows = storage
            .list_node_affiliations(&spaces, "general")
            .await
            .expect("list");
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn seed_owners_on_node_upgrades_member_to_owner() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        storage
            .get_or_create_node(&spaces, "general")
            .await
            .expect("create node");
        storage
            .set_affiliation(&spaces, "general", &jid("alice"), Affiliation::Member)
            .await
            .expect("seed member");

        seed_owners_on_node(&storage, &spaces, "general", &[jid("alice")])
            .await
            .expect("upgrade");

        let aff = storage
            .get_affiliation(&spaces, "general", &jid("alice"))
            .await
            .expect("get aff");
        assert_eq!(aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn seed_owner_on_all_nodes_walks_every_node() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        for node in ["general", "engineering", "design"] {
            storage
                .get_or_create_node(&spaces, node)
                .await
                .expect("create node");
        }

        seed_owner_on_all_nodes(&storage, &spaces, &jid("alice"))
            .await
            .expect("seed all");

        for node in ["general", "engineering", "design"] {
            let aff = storage
                .get_affiliation(&spaces, node, &jid("alice"))
                .await
                .expect("get aff");
            assert_eq!(aff, Affiliation::Owner, "alice should be Owner on {node}");
        }
    }

    #[tokio::test]
    async fn seed_owner_on_all_nodes_with_no_nodes_is_noop() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();

        seed_owner_on_all_nodes(&storage, &spaces, &jid("alice"))
            .await
            .expect("seed empty");
    }

    #[tokio::test]
    async fn seed_owners_on_node_with_empty_entities_is_noop() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        storage
            .get_or_create_node(&spaces, "general")
            .await
            .expect("create node");

        seed_owners_on_node(&storage, &spaces, "general", &[])
            .await
            .expect("seed empty");

        let rows = storage
            .list_node_affiliations(&spaces, "general")
            .await
            .expect("list");
        assert!(rows.is_empty());
    }
}
