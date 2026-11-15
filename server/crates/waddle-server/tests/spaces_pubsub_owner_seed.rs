//! The owner seed of the Spaces and community PubSub services
//! (`spaces_pubsub_seed`): the configured server owners are written as
//! `Owner` on every node, idempotently, without lifting a ban and only for
//! accounts of the served account domain (ADR-0020 D8).
//!
//! The cases run against the in-memory PubSub storage with a deployment
//! that holds no canonical host-grant store, so no node is enrolled. They
//! need neither PostgreSQL nor SpiceDB; the refusal of an enrolled Space
//! node is covered by `xep0045_host_grant_fence`.

use std::sync::Arc;

use jid::BareJid;
use waddle_server::permissions::host_grant::{HostGrantGate, IssuancePolicy};
use waddle_server::spaces_pubsub_seed::{
    account_domain_seed_issuance, seed_owner_on_all_nodes, seed_owners_on_node, OwnerSeedOutcome,
    OwnerSeedPolicy,
};
use waddle_xmpp::pubsub::{Affiliation, InMemoryPubSubStorage, PubSubStorage};

const ACCOUNT_DOMAIN: &str = "example.com";

struct Seed {
    storage: Arc<dyn PubSubStorage>,
    spaces: BareJid,
    issuance: IssuancePolicy,
    host_grants: HostGrantGate,
}

impl Seed {
    fn new() -> Self {
        Self {
            storage: Arc::new(InMemoryPubSubStorage::new()),
            spaces: "spaces.example.com".parse().expect("Spaces service"),
            issuance: account_domain_seed_issuance(ACCOUNT_DOMAIN)
                .expect("issuance policy of the account domain"),
            host_grants: HostGrantGate::default(),
        }
    }

    fn policy(&self) -> OwnerSeedPolicy<'_> {
        OwnerSeedPolicy {
            issuance: &self.issuance,
            host_grants: &self.host_grants,
        }
    }

    async fn node(&self, node: &str) {
        self.storage
            .get_or_create_node(&self.spaces, node)
            .await
            .expect("Space node");
    }

    async fn on_node(&self, node: &str, entities: &[BareJid]) -> Vec<OwnerSeedOutcome> {
        seed_owners_on_node(&self.storage, &self.spaces, node, entities, self.policy()).await
    }

    async fn on_all_nodes(&self, entity: &BareJid) -> Vec<OwnerSeedOutcome> {
        seed_owner_on_all_nodes(&self.storage, &self.spaces, entity, self.policy())
            .await
            .expect("seed every node")
    }

    async fn set(&self, node: &str, entity: &BareJid, affiliation: Affiliation) {
        self.storage
            .set_affiliation(&self.spaces, node, entity, affiliation)
            .await
            .expect("affiliation");
    }

    async fn affiliation(&self, node: &str, entity: &BareJid) -> Affiliation {
        self.storage
            .affiliation(&self.spaces, node, entity)
            .await
            .expect("affiliation read")
    }

    async fn rows(&self, node: &str) -> usize {
        self.storage
            .list_node_affiliations(&self.spaces, node)
            .await
            .expect("affiliation rows")
            .len()
    }
}

fn account(localpart: &str) -> BareJid {
    format!("{localpart}@{ACCOUNT_DOMAIN}")
        .parse()
        .expect("account JID")
}

#[tokio::test]
async fn seeding_a_node_writes_an_owner_row_for_each_account() {
    let seed = Seed::new();
    seed.node("general").await;
    let owners = [account("alice"), account("bob")];

    let outcomes = seed.on_node("general", &owners).await;

    assert_eq!(
        outcomes,
        vec![OwnerSeedOutcome::Seeded, OwnerSeedOutcome::Seeded]
    );
    for owner in &owners {
        assert_eq!(seed.affiliation("general", owner).await, Affiliation::Owner);
    }
}

#[tokio::test]
async fn seeding_twice_leaves_one_owner_row() {
    let seed = Seed::new();
    seed.node("general").await;
    let owners = [account("alice")];

    seed.on_node("general", &owners).await;
    let outcomes = seed.on_node("general", &owners).await;

    assert_eq!(outcomes, vec![OwnerSeedOutcome::Seeded]);
    assert_eq!(
        seed.affiliation("general", &owners[0]).await,
        Affiliation::Owner
    );
    assert_eq!(seed.rows("general").await, 1);
}

#[tokio::test]
async fn seeding_upgrades_a_member_to_owner() {
    let seed = Seed::new();
    seed.node("general").await;
    let alice = account("alice");
    seed.set("general", &alice, Affiliation::Member).await;

    let outcomes = seed.on_node("general", std::slice::from_ref(&alice)).await;

    assert_eq!(outcomes, vec![OwnerSeedOutcome::Seeded]);
    assert_eq!(
        seed.affiliation("general", &alice).await,
        Affiliation::Owner
    );
}

#[tokio::test]
async fn seeding_keeps_a_ban_on_one_node_and_across_every_node() {
    let seed = Seed::new();
    for node in ["general", "engineering"] {
        seed.node(node).await;
    }
    let admin = account("admin");
    seed.set("engineering", &admin, Affiliation::Outcast).await;

    // A reseed of the banned node keeps the ban: only an explicit
    // affiliation change lifts it.
    let outcomes = seed
        .on_node("engineering", std::slice::from_ref(&admin))
        .await;
    assert_eq!(outcomes, vec![OwnerSeedOutcome::KeptOutcast]);
    assert_eq!(
        seed.affiliation("engineering", &admin).await,
        Affiliation::Outcast
    );

    // Seeding every node seeds the other node and keeps the ban.
    let mut outcomes = seed.on_all_nodes(&admin).await;
    outcomes.sort_by_key(|outcome| format!("{outcome:?}"));
    assert_eq!(
        outcomes,
        vec![OwnerSeedOutcome::KeptOutcast, OwnerSeedOutcome::Seeded]
    );
    assert_eq!(
        seed.affiliation("general", &admin).await,
        Affiliation::Owner
    );
    assert_eq!(
        seed.affiliation("engineering", &admin).await,
        Affiliation::Outcast
    );
}

#[tokio::test]
async fn seeding_every_node_walks_each_of_them() {
    let seed = Seed::new();
    let nodes = ["general", "engineering", "design"];
    for node in nodes {
        seed.node(node).await;
    }
    let alice = account("alice");

    let outcomes = seed.on_all_nodes(&alice).await;

    assert_eq!(outcomes, vec![OwnerSeedOutcome::Seeded; nodes.len()]);
    for node in nodes {
        assert_eq!(
            seed.affiliation(node, &alice).await,
            Affiliation::Owner,
            "alice must own {node}"
        );
    }
}

#[tokio::test]
async fn seeding_every_node_back_fills_a_node_created_before_the_owner() {
    // A node that existed before its owner was configured gains the owner's
    // row at the next startup seed.
    let seed = Seed::new();
    seed.node("preexisting").await;
    let late_admin = account("late-admin");

    let outcomes = seed.on_all_nodes(&late_admin).await;

    assert_eq!(outcomes, vec![OwnerSeedOutcome::Seeded]);
    assert_eq!(
        seed.affiliation("preexisting", &late_admin).await,
        Affiliation::Owner
    );
}

#[tokio::test]
async fn seeding_every_node_of_a_service_without_nodes_does_nothing() {
    let seed = Seed::new();

    let outcomes = seed.on_all_nodes(&account("alice")).await;

    assert!(outcomes.is_empty());
}

#[tokio::test]
async fn seeding_a_node_without_accounts_writes_nothing() {
    let seed = Seed::new();
    seed.node("general").await;

    let outcomes = seed.on_node("general", &[]).await;

    assert!(outcomes.is_empty());
    assert_eq!(seed.rows("general").await, 0);
}

#[tokio::test]
async fn seeding_refuses_an_owner_outside_the_served_account_domain() {
    let seed = Seed::new();
    seed.node("general").await;
    let remote: BareJid = "eve@remote.example".parse().expect("remote account");
    let service: BareJid = "spaces.example.com".parse().expect("service address");
    let local = account("alice");

    let outcomes = seed
        .on_node("general", &[remote.clone(), service.clone(), local.clone()])
        .await;

    assert_eq!(
        outcomes,
        vec![
            OwnerSeedOutcome::RefusedSubject,
            OwnerSeedOutcome::RefusedSubject,
            OwnerSeedOutcome::Seeded,
        ]
    );
    assert_eq!(
        seed.affiliation("general", &remote).await,
        Affiliation::None
    );
    assert_eq!(
        seed.affiliation("general", &service).await,
        Affiliation::None
    );
    assert_eq!(
        seed.affiliation("general", &local).await,
        Affiliation::Owner
    );
    assert!(account_domain_seed_issuance("Example.COM.").is_none());
}
