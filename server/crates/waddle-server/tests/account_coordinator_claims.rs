//! The account claim as the account coordinators of a clustered node take,
//! keep and release it.
//!
//! The coordinators run over the SQLite binding store and an in-process
//! claim store, with claims taken through [`ClaimStoreClaimSource`] as on a
//! clustered node. The store's own claim fence needs PostgreSQL and is
//! covered by the `account_coordinator_cluster` binary.

use std::sync::{Arc, OnceLock};

use jid::{BareJid, FullJid};
use waddle_server::account_bindings::{AccountBindingStore, DatabaseAccountBindingStore};
use waddle_server::account_coordinator::{
    AccountBindingAuthority, AccountClaimError, AccountCoordinatorConfig, AccountCoordinatorError,
    AccountCoordinators, BindingReply, ClaimStoreClaimSource, ConfirmOutcome,
    CoordinatorBindingPublisher, LocalNodeLiveness, MirrorClaimHolds, MirrorClaimStore,
    PublishOutcome, RetireEvidence, RetireOutcome,
};
use waddle_server::db::Database;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceExecution, AccountResourceMutation, AccountResourceOperation,
    AccountResourceOperationId, AccountResourceOperationOutcome, AccountResourceTransitionError,
};
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ClaimStore, ConnectionRef, Entity, EntityType, InProcessClaimStore,
    NodeIdentity, NodeLeaseIdentity, SharedNodeIdentity,
};

fn this_node() -> NodeIdentity {
    NodeIdentity::new("node-a", "epoch-a")
}

fn other_node() -> NodeIdentity {
    NodeIdentity::new("node-b", "epoch-b")
}

fn account() -> BareJid {
    "alice@example.test".parse().expect("valid bare JID")
}

fn resource(name: &str) -> FullJid {
    format!("alice@example.test/{name}")
        .parse()
        .expect("valid full JID")
}

fn account_entity() -> Entity {
    Entity::new(EntityType::UserActor, account().to_string())
}

/// A binding on a connection this node hosts.
fn binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
        &this_node(),
    )))
}

struct Harness {
    coordinators: Arc<AccountCoordinators>,
    publisher: CoordinatorBindingPublisher,
    claim_store: Arc<dyn ClaimStore>,
}

impl Harness {
    async fn new(name: &str) -> Self {
        let db = Database::in_memory(name).await.expect("in-memory database");
        let store = DatabaseAccountBindingStore::open(db, false)
            .await
            .expect("open the binding store");
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let mirror_holds = Arc::new(MirrorClaimHolds::default());
        let coordinators = Arc::new(
            AccountCoordinators::new(
                Arc::new(store) as Arc<dyn AccountBindingStore>,
                Arc::new(ClaimStoreClaimSource::with_mirror(
                    Arc::clone(&claim_store),
                    SharedNodeIdentity::new(this_node()),
                    Arc::clone(&mirror_holds),
                )),
                Arc::new(LocalNodeLiveness::new(
                    NodeLeaseIdentity::from_node_identity(&this_node()),
                )),
                AccountCoordinatorConfig::default(),
            )
            .with_mirror_holds(mirror_holds),
        );
        Self {
            publisher: CoordinatorBindingPublisher::new(
                Arc::clone(&coordinators) as Arc<dyn AccountBindingAuthority>
            ),
            coordinators,
            claim_store,
        }
    }

    /// The node that holds the account's claim, and the claim's epoch.
    async fn claim(&self) -> Option<(NodeIdentity, ClaimEpoch)> {
        self.claim_store
            .current_claim(&account_entity())
            .await
            .expect("claim lookup")
            .map(|claim| (claim.owner, claim.claim_epoch))
    }

    /// Bind `jid` and take the binding to `Active`.
    async fn activate(&self, jid: &FullJid) -> BindingRef {
        let binding = binding();
        let published = match self
            .publisher
            .publish(jid, &binding, ResourceLifetime::fresh())
            .await
            .expect("publish answers")
        {
            PublishOutcome::Published(published) => published,
            other => panic!("expected a published binding, got {other:?}"),
        };
        match self
            .publisher
            .confirm(jid, &published)
            .await
            .expect("confirm answers")
        {
            ConfirmOutcome::Confirmed(_) => binding,
            other => panic!("expected a confirmed binding, got {other:?}"),
        }
    }

    async fn retire(&self, jid: &FullJid, binding: &BindingRef) {
        let outcome = self
            .publisher
            .retire_binding(jid, binding, RetireEvidence::recovery_owned())
            .await
            .expect("retire answers");
        assert!(
            matches!(outcome, RetireOutcome::Retired { .. }),
            "expected a retired binding, got {outcome:?}"
        );
    }

    async fn resources(&self) -> Vec<FullJid> {
        self.coordinators
            .view(&account())
            .await
            .expect("account view")
            .resources
    }

    /// The claim store as the `UserActor` mirror reaches it.
    fn mirror_store(&self) -> MirrorClaimStore {
        let coordinators = Arc::clone(&self.coordinators);
        MirrorClaimStore::new(
            Arc::clone(&self.claim_store),
            Arc::new(move || Some(Arc::clone(&coordinators))),
        )
    }
}

#[tokio::test]
async fn an_account_without_a_claim_reads_as_empty_and_the_read_takes_no_claim() {
    let harness = Harness::new("coordinator-claims-unclaimed").await;

    assert!(harness.resources().await.is_empty());
    assert_eq!(harness.claim().await, None);
    assert_eq!(harness.coordinators.loaded_accounts().await, 0);
}

#[tokio::test]
async fn the_first_bind_takes_the_claim_and_the_last_retirement_releases_it() {
    let harness = Harness::new("coordinator-claims-lifecycle").await;
    let (desk, phone) = (resource("desk"), resource("phone"));

    let desk_binding = harness.activate(&desk).await;
    let taken = harness.claim().await.expect("the bind takes the claim");
    assert_eq!(taken.0, this_node());
    let phone_binding = harness.activate(&phone).await;
    assert_eq!(harness.resources().await, vec![desk.clone(), phone.clone()]);

    harness.retire(&desk, &desk_binding).await;
    assert_eq!(
        harness.claim().await,
        Some(taken),
        "the claim stays while the account holds a binding"
    );
    assert_eq!(harness.resources().await, vec![phone.clone()]);

    harness.retire(&phone, &phone_binding).await;
    assert_eq!(harness.claim().await, None);
    assert!(harness.resources().await.is_empty());
    assert_eq!(
        harness.claim().await,
        None,
        "a read after the release takes no claim"
    );

    // The account binds again under a claim taken anew.
    harness.activate(&desk).await;
    assert_eq!(
        harness.claim().await.map(|(node, _)| node),
        Some(this_node())
    );
    assert_eq!(harness.resources().await, vec![desk]);
}

#[tokio::test]
async fn a_claim_another_node_holds_is_left_alone() {
    let harness = Harness::new("coordinator-claims-foreign").await;
    let epoch = harness
        .claim_store
        .acquire(&account_entity(), &other_node())
        .await
        .expect("the other node takes the account");

    for refused in [
        harness
            .coordinators
            .slot_status(&resource("desk"))
            .await
            .map(|_| ()),
        harness.coordinators.view(&account()).await.map(|_| ()),
    ] {
        assert!(
            matches!(
                refused,
                Err(AccountCoordinatorError::Claim(
                    AccountClaimError::HeldElsewhere
                ))
            ),
            "expected the claim error, got {refused:?}"
        );
    }
    assert_eq!(harness.claim().await, Some((other_node(), epoch)));
}

#[tokio::test]
async fn a_bind_that_names_a_released_claim_is_refused_and_the_publisher_repeats_it() {
    let harness = Harness::new("coordinator-claims-race").await;
    let desk = resource("desk");

    // The slot read takes the claim; the account holds no binding yet.
    let status = harness
        .coordinators
        .slot_status(&desk)
        .await
        .expect("slot status");
    assert!(harness.claim().await.is_some());
    assert!(harness
        .coordinators
        .release_claim_if_unbound(&account())
        .await
        .expect("release answers"));
    assert_eq!(harness.claim().await, None);

    let stale = AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::Bind {
            claim_identity: status.claim_identity,
            expected_revision: status.revision,
            binding_ref: binding(),
            lifetime: ResourceLifetime::fresh(),
        },
    );
    let reply = harness
        .coordinators
        .execute(&desk, stale)
        .await
        .expect("the coordinator answers");
    assert_eq!(
        reply,
        BindingReply::Executed(AccountResourceExecution::Applied(
            AccountResourceOperationOutcome::Rejected(
                AccountResourceTransitionError::ClaimMismatch
            )
        ))
    );

    // The publisher reads the slot again and binds under the current claim.
    harness.activate(&desk).await;
    assert_eq!(harness.resources().await, vec![desk]);
}

#[tokio::test]
async fn a_claim_the_mirror_holds_outlives_the_last_binding() {
    let harness = Harness::new("coordinator-claims-mirror-last").await;
    let mirror = harness.mirror_store();
    let desk = resource("desk");

    let binding = harness.activate(&desk).await;
    let taken = harness.claim().await.expect("the bind takes the claim");
    let mirrored = mirror
        .ensure_claimed(&account_entity(), &this_node())
        .await
        .expect("the mirror shares the claim");
    assert_eq!(mirrored, taken.1);

    harness.retire(&desk, &binding).await;
    assert_eq!(
        harness.claim().await,
        Some(taken),
        "the mirror still holds the claim"
    );

    mirror
        .release(&account_entity(), &this_node(), mirrored)
        .await
        .expect("the mirror releases");
    assert_eq!(harness.claim().await, None);
}

#[tokio::test]
async fn a_claim_the_mirror_releases_stays_while_the_account_holds_a_binding() {
    let harness = Harness::new("coordinator-claims-mirror-first").await;
    let mirror = harness.mirror_store();
    let desk = resource("desk");

    let binding = harness.activate(&desk).await;
    let taken = harness.claim().await.expect("the bind takes the claim");
    let mirrored = mirror
        .ensure_claimed(&account_entity(), &this_node())
        .await
        .expect("the mirror shares the claim");

    mirror
        .release(&account_entity(), &this_node(), mirrored)
        .await
        .expect("the mirror releases");
    assert_eq!(
        harness.claim().await,
        Some(taken),
        "the account still holds a binding"
    );
    assert_eq!(harness.resources().await, vec![desk.clone()]);

    harness.retire(&desk, &binding).await;
    assert_eq!(harness.claim().await, None);
}

#[tokio::test]
async fn the_mirror_store_passes_through_while_the_registry_is_the_authority() {
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let selected: Arc<OnceLock<Arc<AccountCoordinators>>> = Arc::default();
    let lookup = Arc::clone(&selected);
    let mirror = MirrorClaimStore::new(
        Arc::clone(&claim_store),
        Arc::new(move || lookup.get().cloned()),
    );

    let epoch = mirror
        .ensure_claimed(&account_entity(), &this_node())
        .await
        .expect("acquire");
    mirror
        .release(&account_entity(), &this_node(), epoch)
        .await
        .expect("release");
    assert!(claim_store
        .current_claim(&account_entity())
        .await
        .expect("claim lookup")
        .is_none());
}
