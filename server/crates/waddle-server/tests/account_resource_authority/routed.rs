//! The routed account authority: a request goes to the node that owns the
//! account, a request whose answer was lost is repeated under its identity
//! and applies once, and a request the receiver does not own follows the
//! claim. The owning node is a second coordinator directory over the same
//! SQLite store, reached through a scripted cluster.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use waddle_server::account_bindings::{AccountBindingStore, DatabaseAccountBindingStore};
use waddle_server::account_coordinator::{
    AccountAuthorityAnswer, AccountAuthorityPeers, AccountAuthorityRequest,
    AccountBindingAuthority, AccountClaimError, AccountClaimSource, AccountCoordinatorConfig,
    AccountCoordinatorError, AccountCoordinators, AccountOwner, AccountRoutingPolicy, BindingReply,
    ForwardFailure, LocalNodeLiveness, RoutedAccountAuthority,
};
use waddle_server::db::Database;
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecution, AccountResourceMutation,
    AccountResourceOperationOutcome, AccountResourceRevision,
};
use waddle_xmpp::ownership::{BindingRef, NodeIdentity, NodeLeaseIdentity, UserClaimIdentity};

use super::support::{bind, claim, fresh_binding, operation, publish, register, resource};

/// The claim the owning node holds.
struct HeldClaim(UserClaimIdentity);

#[async_trait]
impl AccountClaimSource for HeldClaim {
    async fn current_claim(
        &self,
        _account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError> {
        Ok(self.0.clone())
    }
}

/// What the scripted cluster does with one forwarded request.
#[derive(Debug, Clone, Copy)]
enum Forward {
    /// The owner serves it and its answer arrives.
    Serve,
    /// The owner serves it and its answer is lost.
    ServeAndLoseTheAnswer,
    /// The request never reaches the owner.
    NotDelivered,
    /// The receiver no longer holds the account's claim.
    RefuseAsNotOwner,
}

struct ScriptedCluster {
    owner: Arc<AccountCoordinators>,
    owners: Mutex<VecDeque<AccountOwner>>,
    forwards: Mutex<VecDeque<Forward>>,
    forwarded: AtomicUsize,
}

impl ScriptedCluster {
    fn new(owner: Arc<AccountCoordinators>) -> Arc<Self> {
        Arc::new(Self {
            owner,
            owners: Mutex::new(VecDeque::new()),
            forwards: Mutex::new(VecDeque::new()),
            forwarded: AtomicUsize::new(0),
        })
    }

    fn script(&self, forwards: &[Forward]) {
        self.forwards
            .lock()
            .expect("script lock")
            .extend(forwards.iter().copied());
    }

    fn owners_then_remote(&self, owners: &[AccountOwner]) {
        self.owners
            .lock()
            .expect("owner lock")
            .extend(owners.iter().cloned());
    }

    fn forwarded(&self) -> usize {
        self.forwarded.load(Ordering::SeqCst)
    }
}

fn owner_node() -> NodeIdentity {
    NodeIdentity::new("owner-node", "epoch-1")
}

#[async_trait]
impl AccountAuthorityPeers for ScriptedCluster {
    async fn owner(&self, _account: &BareJid) -> Result<AccountOwner, AccountClaimError> {
        Ok(self
            .owners
            .lock()
            .expect("owner lock")
            .pop_front()
            .unwrap_or(AccountOwner::Remote(owner_node())))
    }

    async fn forward(
        &self,
        owner: &NodeIdentity,
        request: AccountAuthorityRequest,
    ) -> Result<Result<AccountAuthorityAnswer, AccountCoordinatorError>, ForwardFailure> {
        assert_eq!(owner, &owner_node());
        self.forwarded.fetch_add(1, Ordering::SeqCst);
        let step = self
            .forwards
            .lock()
            .expect("script lock")
            .pop_front()
            .unwrap_or(Forward::Serve);
        match step {
            Forward::Serve => Ok(self.owner.serve(request).await),
            Forward::ServeAndLoseTheAnswer => {
                let _lost = self.owner.serve(request).await;
                Err(ForwardFailure::Unknown)
            }
            Forward::NotDelivered => Err(ForwardFailure::NotDelivered),
            Forward::RefuseAsNotOwner => Ok(Err(AccountCoordinatorError::Claim(
                AccountClaimError::HeldElsewhere,
            ))),
        }
    }

    async fn displace(
        &self,
        _host: &NodeIdentity,
        _full_jid: &FullJid,
        _binding_ref: &BindingRef,
    ) -> Result<bool, ForwardFailure> {
        Ok(false)
    }
}

struct Harness {
    owner: Arc<AccountCoordinators>,
    cluster: Arc<ScriptedCluster>,
    routed: RoutedAccountAuthority,
}

fn directory(
    store: &Arc<DatabaseAccountBindingStore>,
    claims: &Arc<HeldClaim>,
) -> Arc<AccountCoordinators> {
    Arc::new(AccountCoordinators::new(
        Arc::clone(store) as Arc<dyn AccountBindingStore>,
        Arc::clone(claims) as Arc<dyn AccountClaimSource>,
        Arc::new(LocalNodeLiveness::new(NodeLeaseIdentity::local_process())),
        AccountCoordinatorConfig::default(),
    ))
}

async fn harness(name: &str) -> Harness {
    let db = Database::in_memory(name).await.expect("in-memory database");
    let store = Arc::new(
        DatabaseAccountBindingStore::open(db, false)
            .await
            .expect("open the store"),
    );
    let claims = Arc::new(HeldClaim(claim(1)));
    let owner = directory(&store, &claims);
    let local = directory(&store, &claims);
    let cluster = ScriptedCluster::new(Arc::clone(&owner));
    let routed = RoutedAccountAuthority::new(local, Arc::clone(&cluster) as _).with_policy(
        AccountRoutingPolicy {
            deadline: Duration::from_millis(500),
            first_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        },
    );
    Harness {
        owner,
        cluster,
        routed,
    }
}

/// Execute a mutation at the owner that must commit now.
async fn commit_at_owner(
    owner: &AccountCoordinators,
    jid: &FullJid,
    mutation: AccountResourceMutation,
) -> AccountResourceRevision {
    match owner
        .execute(jid, operation(mutation))
        .await
        .expect("the owner answers")
    {
        BindingReply::Executed(AccountResourceExecution::Applied(
            AccountResourceOperationOutcome::Committed { revision },
        )) => revision,
        other => panic!("expected a committed mutation, got {other:?}"),
    }
}

#[tokio::test]
async fn a_bind_whose_answer_was_lost_is_repeated_and_applies_once() {
    let harness = harness("routed_bind_lost_answer").await;
    let jid = resource("phone");
    let binding = fresh_binding();
    let status = harness.routed.slot_status(&jid).await.expect("slot status");
    assert_eq!(harness.cluster.forwarded(), 1);

    harness
        .cluster
        .script(&[Forward::ServeAndLoseTheAnswer, Forward::Serve]);
    let reply = harness
        .routed
        .execute(
            &jid,
            operation(bind(&status.claim_identity, status.revision, &binding)),
        )
        .await
        .expect("the repeated bind is answered");
    let BindingReply::Executed(AccountResourceExecution::Replayed(
        AccountResourceOperationOutcome::Committed { revision },
    )) = reply
    else {
        panic!("the repeat returns the recorded outcome, got {reply:?}");
    };
    assert_eq!(harness.cluster.forwarded(), 3);

    let after = harness.owner.slot_status(&jid).await.expect("slot status");
    assert_eq!(after.revision, revision);
    assert_eq!(
        after.binding,
        Some((binding, AccountResourceBindingState::Prepared))
    );
    assert_eq!(
        revision.value(),
        status.revision.value() + 1,
        "the bind applied once"
    );
}

#[tokio::test]
async fn a_presentation_whose_answer_was_lost_is_not_repeated() {
    let harness = harness("routed_presentation_lost_answer").await;
    let jid = resource("phone");
    let binding = fresh_binding();
    let status = harness.owner.slot_status(&jid).await.expect("slot status");
    let claim = status.claim_identity;
    let revision = commit_at_owner(
        &harness.owner,
        &jid,
        bind(&claim, status.revision, &binding),
    )
    .await;
    let revision = commit_at_owner(&harness.owner, &jid, publish(&claim, revision, &binding)).await;
    commit_at_owner(&harness.owner, &jid, register(&claim, revision, &binding)).await;

    harness.cluster.script(&[Forward::ServeAndLoseTheAnswer]);
    let outcome = harness
        .routed
        .mark_presented(&jid, &binding, Presentation::OfflineMessages)
        .await;
    assert!(
        matches!(outcome, Err(AccountCoordinatorError::OwnerUndecided)),
        "{outcome:?}"
    );
    assert_eq!(
        harness.cluster.forwarded(),
        1,
        "the presentation was sent once"
    );
    assert!(
        !harness
            .owner
            .mark_presented(&jid, &binding, Presentation::OfflineMessages)
            .await
            .expect("the owner answers"),
        "the lost attempt recorded the presentation"
    );
}

#[tokio::test]
async fn a_request_the_receiver_does_not_own_follows_the_claim() {
    let harness = harness("routed_follows_the_claim").await;
    let jid = resource("phone");
    harness
        .cluster
        .owners_then_remote(&[AccountOwner::Remote(owner_node()), AccountOwner::Local]);
    harness.cluster.script(&[Forward::RefuseAsNotOwner]);
    let status = harness
        .routed
        .slot_status(&jid)
        .await
        .expect("this node serves the account once it owns it");
    assert_eq!(status.revision, AccountResourceRevision::INITIAL);
    assert_eq!(harness.cluster.forwarded(), 1);
}

#[tokio::test]
async fn an_owner_that_cannot_be_reached_leaves_the_request_undecided() {
    let harness = harness("routed_owner_unreachable").await;
    let jid = resource("phone");
    harness.cluster.script(&[Forward::NotDelivered; 1024]);
    let outcome = harness.routed.slot_status(&jid).await;
    assert!(
        matches!(outcome, Err(AccountCoordinatorError::OwnerUndecided)),
        "{outcome:?}"
    );
    assert!(harness.cluster.forwarded() > 1, "the request was retried");
}
