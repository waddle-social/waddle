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
/// `(spaces_jid, node)`. Idempotent on re-run; any existing affiliation
/// (including `Outcast`) is overridden — entries seeded by this module
/// represent the configured server-owner JID set, which is non-negotiable
/// authority on Spaces nodes by design (see issue #241).
///
/// Per-entity write failures are logged with `warn!` and do not abort the
/// batch.
pub async fn seed_owners_on_node(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    node: &str,
    entities: &[BareJid],
) {
    for entity in entities {
        seed_owner_one(storage, spaces_jid, node, entity).await;
    }
}

/// Write `Affiliation::Owner` for `entity` against every node owned by
/// `spaces_jid`. Used at startup to mirror server-owner permissions across
/// all existing Spaces nodes.
///
/// Like [`seed_owners_on_node`], existing `Outcast` rows are overridden —
/// server owners are non-negotiable.
///
/// Per-node write failures are logged with `warn!` and do not abort the
/// batch. A failure to enumerate nodes propagates as `Err` because the
/// caller cannot proceed without the node list.
pub async fn seed_owner_on_all_nodes(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    entity: &BareJid,
) -> Result<(), XmppError> {
    let nodes = storage.list_nodes(spaces_jid).await?;
    for node in &nodes {
        seed_owner_one(storage, spaces_jid, node, entity).await;
    }
    Ok(())
}

async fn seed_owner_one(
    storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    node: &str,
    entity: &BareJid,
) {
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

        seed_owners_on_node(&storage, &spaces, "general", &owners).await;

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

        seed_owners_on_node(&storage, &spaces, "general", &owners).await;
        seed_owners_on_node(&storage, &spaces, "general", &owners).await;

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

        seed_owners_on_node(&storage, &spaces, "general", &[jid("alice")]).await;

        let aff = storage
            .get_affiliation(&spaces, "general", &jid("alice"))
            .await
            .expect("get aff");
        assert_eq!(aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn seed_owners_on_node_overrides_existing_outcast() {
        // Server owners are non-negotiable: if a node owner manually demoted a
        // server owner to Outcast via <affiliations/>, the next reseed must
        // restore Owner so admin ops keep working.
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        storage
            .get_or_create_node(&spaces, "general")
            .await
            .expect("create node");
        storage
            .set_affiliation(&spaces, "general", &jid("admin"), Affiliation::Outcast)
            .await
            .expect("set outcast");

        seed_owners_on_node(&storage, &spaces, "general", &[jid("admin")]).await;

        let aff = storage
            .get_affiliation(&spaces, "general", &jid("admin"))
            .await
            .expect("get aff");
        assert_eq!(aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn seed_owner_on_all_nodes_overrides_existing_outcast() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        for node in ["general", "engineering"] {
            storage
                .get_or_create_node(&spaces, node)
                .await
                .expect("create node");
        }
        storage
            .set_affiliation(&spaces, "engineering", &jid("admin"), Affiliation::Outcast)
            .await
            .expect("set outcast");

        seed_owner_on_all_nodes(&storage, &spaces, &jid("admin"))
            .await
            .expect("seed all");

        for node in ["general", "engineering"] {
            let aff = storage
                .get_affiliation(&spaces, node, &jid("admin"))
                .await
                .expect("get aff");
            assert_eq!(aff, Affiliation::Owner, "admin must be Owner on {node}");
        }
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
    async fn seed_owner_on_all_nodes_back_fills_pre_existing_node() {
        // Models the regression #241 cares about: a Spaces node already exists
        // (e.g. created before WADDLE_SERVER_OWNER_LOCALPARTS gained a new
        // localpart) and the new owner needs an affiliation row back-filled.
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let spaces = spaces_jid();
        storage
            .get_or_create_node(&spaces, "preexisting")
            .await
            .expect("create node");

        seed_owner_on_all_nodes(&storage, &spaces, &jid("late-admin"))
            .await
            .expect("back-fill seed");

        let aff = storage
            .get_affiliation(&spaces, "preexisting", &jid("late-admin"))
            .await
            .expect("get aff");
        assert_eq!(aff, Affiliation::Owner);
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

        seed_owners_on_node(&storage, &spaces, "general", &[]).await;

        let rows = storage
            .list_node_affiliations(&spaces, "general")
            .await
            .expect("list");
        assert!(rows.is_empty());
    }
}
