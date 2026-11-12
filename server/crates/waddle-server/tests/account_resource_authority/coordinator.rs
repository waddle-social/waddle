//! The account coordinator over the SQLite binding store: the binding
//! lifecycle, selection, replacement fencing, one-time presentation,
//! bounded unregister and repair, owner migration and idle reclaim.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use waddle_server::account_bindings::{AccountBindingStore, DatabaseAccountBindingStore};
use waddle_server::account_coordinator::{
    AccountBindingAuthority, AccountClaimError, AccountClaimSource, AccountCoordinatorConfig,
    AccountCoordinatorError, AccountCoordinators, BindingReply, LocalNodeLiveness, RepairEvidence,
    RepairOutcome, UnregisterOutcome, UnregisterRequest,
};
use waddle_server::db::Database;
use waddle_xmpp::ownership::account_coordinator::{
    AccountResourceRouting, AccountResourcesError, BindingFenceError, Presentation,
};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecution, AccountResourceMutation,
    AccountResourceOperationId, AccountResourceOperationOutcome, AccountResourceRevision,
    AccountResourceSnapshot, AccountResourceTransitionError, BindingRetirementDisposition,
    DirectedPresenceTeardownDisposition, PresentationFlushDisposition,
};
use waddle_xmpp::ownership::{
    BindingRef, ConnectionRef, NodeIdentity, NodeLeaseIdentity, UserClaimIdentity,
};

use super::support::{
    bind, claim, complete_retirement, detach, fresh_binding, insert_session, operation, presence,
    publish, register, resource, resume, sm_binding, update,
};

const INITIAL: AccountResourceRevision = AccountResourceRevision::INITIAL;

/// A claim source the test moves from one claim identity to the next.
struct MovableClaim(Mutex<UserClaimIdentity>);

impl MovableClaim {
    fn new(claim: UserClaimIdentity) -> Arc<Self> {
        Arc::new(Self(Mutex::new(claim)))
    }

    fn move_to(&self, claim: UserClaimIdentity) {
        *self.0.lock().expect("claim lock") = claim;
    }

    fn current(&self) -> UserClaimIdentity {
        self.0.lock().expect("claim lock").clone()
    }
}

#[async_trait]
impl AccountClaimSource for MovableClaim {
    async fn current_claim(
        &self,
        _account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError> {
        Ok(self.current())
    }
}

struct Harness {
    coordinators: AccountCoordinators,
    store: Arc<DatabaseAccountBindingStore>,
    claims: Arc<MovableClaim>,
    db: Database,
}

impl Harness {
    /// The claim identity the harness's coordinators act under. Two claim
    /// identities built from the same parts are distinct incarnations, so
    /// mutations must name this one.
    fn claim(&self) -> UserClaimIdentity {
        self.claims.current()
    }
}

fn config() -> AccountCoordinatorConfig {
    AccountCoordinatorConfig::default()
}

async fn harness(name: &str, config: AccountCoordinatorConfig) -> Harness {
    let db = Database::in_memory(name).await.expect("in-memory database");
    let store = Arc::new(
        DatabaseAccountBindingStore::open(db.clone(), false)
            .await
            .expect("open the store"),
    );
    let claims = MovableClaim::new(claim(1));
    let coordinators = directory(&store, &claims, config);
    Harness {
        coordinators,
        store,
        claims,
        db,
    }
}

fn directory(
    store: &Arc<DatabaseAccountBindingStore>,
    claims: &Arc<MovableClaim>,
    config: AccountCoordinatorConfig,
) -> AccountCoordinators {
    AccountCoordinators::new(
        Arc::clone(store) as Arc<dyn AccountBindingStore>,
        Arc::clone(claims) as Arc<dyn AccountClaimSource>,
        Arc::new(LocalNodeLiveness::new(NodeLeaseIdentity::local_process())),
        config,
    )
}

/// Execute a mutation that must commit now and return its revision.
async fn drive(
    coordinators: &AccountCoordinators,
    jid: &FullJid,
    mutation: AccountResourceMutation,
) -> AccountResourceRevision {
    match coordinators
        .execute(jid, operation(mutation))
        .await
        .expect("coordinator answers")
    {
        BindingReply::Executed(AccountResourceExecution::Applied(
            AccountResourceOperationOutcome::Committed { revision },
        )) => revision,
        other => panic!("expected a committed mutation, got {other:?}"),
    }
}

/// Execute a mutation that must be rejected now and return the reason.
async fn refuse(
    coordinators: &AccountCoordinators,
    jid: &FullJid,
    mutation: AccountResourceMutation,
) -> AccountResourceTransitionError {
    match coordinators
        .execute(jid, operation(mutation))
        .await
        .expect("coordinator answers")
    {
        BindingReply::Executed(AccountResourceExecution::Applied(
            AccountResourceOperationOutcome::Rejected(error),
        )) => error,
        other => panic!("expected a rejected mutation, got {other:?}"),
    }
}

/// Bind `jid` with `binding` and take it to `Active`.
async fn activate_binding(
    coordinators: &AccountCoordinators,
    jid: &FullJid,
    binding: &BindingRef,
) -> AccountResourceRevision {
    let status = coordinators.slot_status(jid).await.expect("slot status");
    let claim = status.claim_identity;
    let revision = drive(coordinators, jid, bind(&claim, status.revision, binding)).await;
    let revision = drive(coordinators, jid, publish(&claim, revision, binding)).await;
    drive(coordinators, jid, register(&claim, revision, binding)).await
}

async fn activate(
    coordinators: &AccountCoordinators,
    jid: &FullJid,
) -> (BindingRef, AccountResourceRevision) {
    let binding = fresh_binding();
    let revision = activate_binding(coordinators, jid, &binding).await;
    (binding, revision)
}

async fn state_of(
    coordinators: &AccountCoordinators,
    jid: &FullJid,
) -> Option<AccountResourceBindingState> {
    coordinators
        .slot_status(jid)
        .await
        .expect("slot status")
        .binding
        .map(|(_, state)| state)
}

async fn resources(coordinators: &AccountCoordinators, jid: &FullJid) -> Vec<FullJid> {
    coordinators
        .view(&jid.to_bare())
        .await
        .expect("account view")
        .resources
}

fn unregister_request(binding: &BindingRef) -> UnregisterRequest {
    UnregisterRequest {
        operation: AccountResourceOperationId::fresh(),
        binding_ref: binding.clone(),
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
    }
}

fn repair_evidence() -> RepairEvidence {
    RepairEvidence {
        operation: AccountResourceOperationId::fresh(),
        directed_presence: DirectedPresenceTeardownDisposition::NoDirectedRecipients,
        presentation_flush: PresentationFlushDisposition::NoPendingPresentation,
    }
}

#[tokio::test]
async fn the_binding_lifecycle_decides_what_is_routable() {
    let harness = harness("account-coordinator-lifecycle", config()).await;
    let coordinators = &harness.coordinators;
    let jid = resource("phone");
    let claim = harness.claim();
    let binding = fresh_binding();
    let session = insert_session(&harness.db).await;

    assert_eq!(state_of(coordinators, &jid).await, None);
    let bound = drive(coordinators, &jid, bind(&claim, INITIAL, &binding)).await;
    assert_eq!(
        state_of(coordinators, &jid).await,
        Some(AccountResourceBindingState::Prepared)
    );
    assert!(resources(coordinators, &jid).await.is_empty());

    let published = drive(coordinators, &jid, publish(&claim, bound, &binding)).await;
    assert_eq!(
        state_of(coordinators, &jid).await,
        Some(AccountResourceBindingState::Published)
    );
    assert!(resources(coordinators, &jid).await.is_empty());
    assert_eq!(
        coordinators
            .bind_success(&jid, &binding)
            .await
            .expect("bind success"),
        None,
        "a published binding is not yet acknowledged to the client"
    );

    let active = drive(coordinators, &jid, register(&claim, published, &binding)).await;
    assert_eq!(
        state_of(coordinators, &jid).await,
        Some(AccountResourceBindingState::Active)
    );
    assert_eq!(resources(coordinators, &jid).await, vec![jid.clone()]);
    assert!(coordinators
        .bind_success(&jid, &binding)
        .await
        .expect("bind success")
        .is_some());

    let snapshot = AccountResourceSnapshot {
        stream_management_ref: Some(sm_binding(session, 1)),
        roster_interested: true,
        ..presence(true, 5, true)
    };
    let updated = drive(
        coordinators,
        &jid,
        update(&claim, active, &binding, snapshot),
    )
    .await;
    assert_eq!(
        state_of(coordinators, &jid).await,
        Some(AccountResourceBindingState::Active)
    );
    let view = coordinators
        .view(&jid.to_bare())
        .await
        .expect("account view");
    assert_eq!(view.available, vec![(jid.clone(), 5)]);
    assert_eq!(view.routable, vec![jid.clone()]);
    assert_eq!(view.roster_interested, vec![jid.clone()]);
    assert!(view.blocklist_interested.is_empty());

    let detached = drive(coordinators, &jid, detach(&claim, updated, &binding)).await;
    assert_eq!(
        state_of(coordinators, &jid).await,
        Some(AccountResourceBindingState::Detached)
    );
    assert!(resources(coordinators, &jid).await.is_empty());

    let lifetime = harness
        .store
        .load(&jid)
        .await
        .expect("load")
        .and_then(|stored| stored.image.binding)
        .expect("the detached binding")
        .lifetime;
    let resumed = fresh_binding();
    let prepared = drive(
        coordinators,
        &jid,
        resume(
            &claim,
            detached,
            &binding,
            &resumed,
            sm_binding(session, 1),
            lifetime,
        ),
    )
    .await;
    assert_eq!(
        coordinators
            .slot_status(&jid)
            .await
            .expect("slot status")
            .binding,
        Some((resumed.clone(), AccountResourceBindingState::Prepared))
    );
    let published = drive(coordinators, &jid, publish(&claim, prepared, &resumed)).await;
    let active = drive(coordinators, &jid, register(&claim, published, &resumed)).await;
    let view = coordinators
        .view(&jid.to_bare())
        .await
        .expect("account view");
    assert_eq!(
        view.available,
        vec![(jid.clone(), 5)],
        "a resume keeps the slot's snapshot"
    );
    assert_eq!(
        coordinators
            .bind_success(&jid, &binding)
            .await
            .expect("bind success"),
        None,
        "the binding before the resume is no longer the active one"
    );

    let UnregisterOutcome::Retiring { revision: retiring } = coordinators
        .unregister(&jid, unregister_request(&resumed))
        .await
        .expect("unregister")
    else {
        panic!("expected the retirement to start");
    };
    assert_eq!(retiring.value(), active.value() + 1);
    assert_eq!(
        state_of(coordinators, &jid).await,
        Some(AccountResourceBindingState::Retiring)
    );
    assert!(resources(coordinators, &jid).await.is_empty());
    drive(
        coordinators,
        &jid,
        complete_retirement(&claim, retiring, &resumed, &claim, updated),
    )
    .await;
    assert_eq!(state_of(coordinators, &jid).await, None);
}

/// One resource of a selection case: name, availability, priority, carbons.
type Case = &'static [(&'static str, bool, i8, bool)];

const SELECTION_CASES: &[Case] = &[
    &[],
    &[("solo", true, 0, false)],
    &[("solo", false, 5, true)],
    &[("low", true, 1, false), ("high", true, 10, true)],
    &[
        ("a", true, 3, false),
        ("b", true, 3, true),
        ("c", true, 2, false),
    ],
    &[("negative", true, -1, true), ("zero", true, 0, false)],
    &[("negative", true, -1, false), ("lower", true, -128, true)],
    &[("away", false, 100, true), ("here", true, 1, false)],
    &[("max", true, 127, false), ("also-max", true, 127, false)],
];

#[tokio::test]
async fn selection_follows_the_bare_jid_rules_on_a_table_of_cases() {
    for (index, case) in SELECTION_CASES.iter().enumerate() {
        let harness = harness(&format!("account-coordinator-selection-{index}"), config()).await;
        let coordinators = &harness.coordinators;
        let claim = harness.claim();
        for (name, available, priority, carbons) in case.iter().copied() {
            let jid = resource(name);
            let (binding, revision) = activate(coordinators, &jid).await;
            drive(
                coordinators,
                &jid,
                update(
                    &claim,
                    revision,
                    &binding,
                    presence(available, priority, carbons),
                ),
            )
            .await;
        }
        let view = coordinators
            .view(&claim.bare_jid().clone())
            .await
            .expect("account view");

        let mut every: Vec<FullJid> = case.iter().map(|(name, ..)| resource(name)).collect();
        every.sort();
        assert_eq!(view.resources, every, "case {index}: every resource");

        let mut available: Vec<(FullJid, i8)> = case
            .iter()
            .filter(|(_, available, ..)| *available)
            .map(|(name, _, priority, _)| (resource(name), *priority))
            .collect();
        available.sort();
        assert_eq!(view.available, available, "case {index}: available");

        let highest = available
            .iter()
            .map(|(_, priority)| *priority)
            .filter(|priority| *priority >= 0)
            .max();
        let routable: Vec<FullJid> = available
            .iter()
            .filter(|(_, priority)| Some(*priority) == highest)
            .map(|(jid, _)| jid.clone())
            .collect();
        assert_eq!(view.routable, routable, "case {index}: routable");

        let mut routing: Vec<AccountResourceRouting> = case
            .iter()
            .map(
                |(name, available, priority, carbons)| AccountResourceRouting {
                    jid: resource(name),
                    available: *available,
                    priority: *priority,
                    carbons_enabled: *carbons,
                },
            )
            .collect();
        routing.sort_by(|left, right| left.jid.cmp(&right.jid));
        assert_eq!(view.routing, routing, "case {index}: routing snapshot");
    }
}

#[tokio::test]
async fn a_replacement_waits_for_the_old_binding_and_fences_it_afterwards() {
    let harness = harness("account-coordinator-replacement", config()).await;
    let coordinators = &harness.coordinators;
    let jid = resource("desk");
    let claim = harness.claim();
    let (old, active) = activate(coordinators, &jid).await;
    let new = fresh_binding();

    let pending = coordinators
        .execute(&jid, operation(bind(&claim, INITIAL, &new)))
        .await
        .expect("coordinator answers");
    assert_eq!(
        pending,
        BindingReply::ReplacementPending {
            displaced: old.clone(),
            revision: active,
        }
    );
    assert_eq!(
        resources(coordinators, &jid).await,
        vec![jid.clone()],
        "the old binding stays the destination until it retires"
    );
    assert!(matches!(
        coordinators
            .execute(&jid, operation(bind(&claim, INITIAL, &fresh_binding())))
            .await,
        Err(AccountCoordinatorError::ReplacementAlreadyPending)
    ));
    assert_eq!(
        refuse(
            coordinators,
            &jid,
            update(&claim, active, &new, presence(true, 1, false))
        )
        .await,
        AccountResourceTransitionError::BindingMismatch,
        "a pending replacement cannot write the slot"
    );

    let UnregisterOutcome::Retiring { revision: retiring } = coordinators
        .unregister(&jid, unregister_request(&old))
        .await
        .expect("unregister")
    else {
        panic!("expected the old binding to start retiring");
    };
    assert!(resources(coordinators, &jid).await.is_empty());
    let freed = drive(
        coordinators,
        &jid,
        complete_retirement(
            &claim,
            retiring,
            &old,
            &claim,
            AccountResourceRevision::new(1),
        ),
    )
    .await;

    let bound = drive(coordinators, &jid, bind(&claim, freed, &new)).await;
    let published = drive(coordinators, &jid, publish(&claim, bound, &new)).await;
    let replaced = drive(coordinators, &jid, register(&claim, published, &new)).await;
    assert_eq!(resources(coordinators, &jid).await, vec![jid.clone()]);
    assert!(coordinators
        .bind_success(&jid, &new)
        .await
        .expect("bind success")
        .is_some());
    assert_eq!(
        coordinators
            .bind_success(&jid, &old)
            .await
            .expect("bind success"),
        None
    );

    assert_eq!(
        refuse(
            coordinators,
            &jid,
            update(&claim, replaced, &old, presence(false, 0, false))
        )
        .await,
        AccountResourceTransitionError::BindingMismatch,
        "a delayed command of the replaced binding cannot change its replacement"
    );
    assert_eq!(
        coordinators
            .unregister(&jid, unregister_request(&old))
            .await
            .expect("unregister"),
        UnregisterOutcome::Stale(BindingFenceError::Superseded)
    );
    assert_eq!(
        state_of(coordinators, &jid).await,
        Some(AccountResourceBindingState::Active)
    );
}

#[tokio::test]
async fn a_presentation_happens_once_across_a_resume() {
    let harness = harness("account-coordinator-presentation", config()).await;
    let coordinators = &harness.coordinators;
    let jid = resource("tablet");
    let claim = harness.claim();
    let session = insert_session(&harness.db).await;
    let (binding, active) = activate(coordinators, &jid).await;

    assert!(coordinators
        .mark_presented(&jid, &binding, Presentation::OfflineMessages)
        .await
        .expect("mark"));
    let snapshot = AccountResourceSnapshot {
        stream_management_ref: Some(sm_binding(session, 1)),
        ..presence(true, 0, false)
    };
    let updated = drive(
        coordinators,
        &jid,
        update(&claim, active, &binding, snapshot),
    )
    .await;
    assert!(
        !coordinators
            .mark_presented(&jid, &binding, Presentation::OfflineMessages)
            .await
            .expect("mark"),
        "enabling stream management after the flush does not flush again"
    );

    let detached = drive(coordinators, &jid, detach(&claim, updated, &binding)).await;
    let lifetime = harness
        .store
        .load(&jid)
        .await
        .expect("load")
        .and_then(|stored| stored.image.binding)
        .expect("the detached binding")
        .lifetime;
    let resumed = fresh_binding();
    let prepared = drive(
        coordinators,
        &jid,
        resume(
            &claim,
            detached,
            &binding,
            &resumed,
            sm_binding(session, 1),
            lifetime,
        ),
    )
    .await;
    let published = drive(coordinators, &jid, publish(&claim, prepared, &resumed)).await;
    drive(coordinators, &jid, register(&claim, published, &resumed)).await;

    assert!(
        !coordinators
            .mark_presented(&jid, &resumed, Presentation::OfflineMessages)
            .await
            .expect("mark"),
        "the resumed binding belongs to the session that was already presented"
    );
    assert!(coordinators
        .mark_presented(&jid, &resumed, Presentation::PendingSubscriptions)
        .await
        .expect("mark"));
    assert!(matches!(
        coordinators
            .mark_presented(&jid, &binding, Presentation::PendingSubscriptions)
            .await,
        Err(AccountCoordinatorError::Fenced(
            BindingFenceError::Superseded
        ))
    ));

    // A coordinator that loads the account afresh reads the same record.
    let reloaded = directory(&harness.store, &harness.claims, config());
    assert!(!reloaded
        .mark_presented(&jid, &resumed, Presentation::OfflineMessages)
        .await
        .expect("mark"));
}

#[tokio::test]
async fn unregister_is_bounded_and_repair_completes_or_reports() {
    let harness = harness(
        "account-coordinator-unregister",
        AccountCoordinatorConfig {
            retirement_deadline: Duration::ZERO,
            max_pending_retirements: 1,
            ..config()
        },
    )
    .await;
    let coordinators = &harness.coordinators;
    let jid = resource("laptop");
    let other = resource("watch");
    let account = jid.to_bare();
    let claim = harness.claim();
    let (binding, _) = activate(coordinators, &jid).await;
    let (other_binding, other_active) = activate(coordinators, &other).await;

    assert_eq!(
        coordinators
            .unregister(&jid, unregister_request(&fresh_binding()))
            .await
            .expect("unregister"),
        UnregisterOutcome::Stale(BindingFenceError::Superseded)
    );
    assert_eq!(
        coordinators
            .repair(&jid, &binding, repair_evidence())
            .await
            .expect("repair"),
        RepairOutcome::NotRetiring(AccountResourceBindingState::Active),
        "repair does not start a retirement"
    );
    assert!(coordinators
        .overdue_retirements(&account)
        .await
        .expect("overdue retirements")
        .is_empty());

    let request = unregister_request(&binding);
    let UnregisterOutcome::Retiring { revision } = coordinators
        .unregister(&jid, request.clone())
        .await
        .expect("unregister")
    else {
        panic!("expected the retirement to start");
    };
    assert_eq!(
        coordinators
            .unregister(&jid, request)
            .await
            .expect("repeat the unregister"),
        UnregisterOutcome::Retiring { revision }
    );
    assert_eq!(
        coordinators
            .overdue_retirements(&account)
            .await
            .expect("overdue retirements"),
        vec![(jid.clone(), binding.clone())]
    );
    assert_eq!(resources(coordinators, &jid).await, vec![other.clone()]);
    let reloaded = directory(
        &harness.store,
        &harness.claims,
        AccountCoordinatorConfig {
            retirement_deadline: Duration::ZERO,
            ..config()
        },
    );
    assert_eq!(
        reloaded
            .overdue_retirements(&account)
            .await
            .expect("overdue retirements"),
        vec![(jid.clone(), binding.clone())],
        "a coordinator that loads a retiring binding waits for its evidence again"
    );
    assert!(
        matches!(
            coordinators
                .execute(
                    &other,
                    operation(super::support::begin_retirement(
                        &claim,
                        other_active,
                        &other_binding
                    ))
                )
                .await,
            Err(AccountCoordinatorError::RetirementBacklogFull)
        ),
        "the set of retirements awaiting evidence is bounded"
    );

    assert_eq!(
        coordinators
            .repair(
                &jid,
                &binding,
                RepairEvidence {
                    directed_presence: DirectedPresenceTeardownDisposition::Unknown,
                    ..repair_evidence()
                }
            )
            .await
            .expect("repair"),
        RepairOutcome::Rejected(AccountResourceTransitionError::RetirementEvidenceMismatch),
        "an unknown teardown does not complete a retirement"
    );
    assert!(resources(coordinators, &jid).await == vec![other.clone()]);
    let completed = coordinators
        .repair(&jid, &binding, repair_evidence())
        .await
        .expect("repair");
    assert!(matches!(completed, RepairOutcome::Completed { .. }));
    assert_eq!(
        coordinators
            .repair(&jid, &binding, repair_evidence())
            .await
            .expect("repeat the repair"),
        completed
    );
    assert!(coordinators
        .overdue_retirements(&account)
        .await
        .expect("overdue retirements")
        .is_empty());

    let (replacement, _) = activate(coordinators, &jid).await;
    assert_eq!(
        coordinators
            .repair(&jid, &binding, repair_evidence())
            .await
            .expect("repair"),
        RepairOutcome::HeldByNewerBinding(replacement.clone()),
        "repair never touches a newer binding"
    );
    assert_eq!(
        coordinators
            .slot_status(&jid)
            .await
            .expect("slot status")
            .binding,
        Some((replacement, AccountResourceBindingState::Active))
    );
}

#[tokio::test]
async fn an_operation_identity_replays_in_memory_and_after_a_reload() {
    let harness = harness("account-coordinator-replay", config()).await;
    let coordinators = &harness.coordinators;
    let jid = resource("replay");
    let claim = harness.claim();
    let binding = fresh_binding();
    let bind_operation = operation(bind(&claim, INITIAL, &binding));
    let BindingReply::Executed(AccountResourceExecution::Applied(
        AccountResourceOperationOutcome::Committed { revision },
    )) = coordinators
        .execute(&jid, bind_operation.clone())
        .await
        .expect("bind")
    else {
        panic!("expected the bind to commit");
    };
    let replayed = BindingReply::Executed(AccountResourceExecution::Replayed(
        AccountResourceOperationOutcome::Committed { revision },
    ));
    assert_eq!(
        coordinators
            .execute(&jid, bind_operation.clone())
            .await
            .expect("repeat the bind"),
        replayed
    );
    let mut reused = bind_operation.clone();
    reused.mutation = bind(&claim, INITIAL, &fresh_binding());
    assert!(matches!(
        coordinators.execute(&jid, reused).await,
        Err(AccountCoordinatorError::OperationIdConflict)
    ));

    // A coordinator without the journal resolves the identity from the
    // store and reflects the committed binding.
    let reloaded = directory(&harness.store, &harness.claims, config());
    assert_eq!(
        reloaded
            .execute(&jid, bind_operation)
            .await
            .expect("repeat the bind after a reload"),
        replayed
    );
    assert_eq!(
        reloaded
            .slot_status(&jid)
            .await
            .expect("slot status")
            .binding,
        Some((binding, AccountResourceBindingState::Prepared))
    );
    assert!(matches!(
        reloaded
            .execute(
                &resource("unknown"),
                operation(publish(&claim, INITIAL, &fresh_binding()))
            )
            .await,
        Err(AccountCoordinatorError::Resources(
            AccountResourcesError::UnknownResource
        ))
    ));
}

#[tokio::test]
async fn a_moved_claim_adopts_live_bindings_and_fences_the_rest() {
    let harness = harness("account-coordinator-migration", config()).await;
    let jid = resource("live");
    let gone = resource("gone");
    let old = harness.claim();
    let new = claim(2);
    let (live, _) = activate(&harness.coordinators, &jid).await;
    let dead = BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
        &NodeIdentity::new("node-gone", "epoch-gone"),
    )));
    activate_binding(&harness.coordinators, &gone, &dead).await;

    harness.claims.move_to(new.clone());
    let report = harness
        .coordinators
        .adopt_account(&jid.to_bare())
        .await
        .expect("adopt the account");
    assert_eq!(report.adopted, vec![(jid.clone(), live.clone())]);
    assert_eq!(report.unadopted, vec![(gone.clone(), dead.clone())]);
    assert!(report.refused.is_empty());

    let coordinators = &harness.coordinators;
    assert_eq!(
        resources(coordinators, &jid).await,
        vec![jid.clone()],
        "a binding on a connection that is not live is not a destination"
    );
    let status = coordinators.slot_status(&jid).await.expect("slot status");
    assert_eq!(status.claim_identity, new);
    assert_eq!(
        status.binding,
        Some((live.clone(), AccountResourceBindingState::Active)),
        "adoption keeps the binding's exact reference"
    );
    assert_eq!(
        refuse(
            coordinators,
            &jid,
            update(&old, status.revision, &live, presence(true, 1, false))
        )
        .await,
        AccountResourceTransitionError::ClaimMismatch,
        "the previous claim cannot write the account any more"
    );
    drive(
        coordinators,
        &jid,
        update(&new, status.revision, &live, presence(true, 1, false)),
    )
    .await;

    assert!(matches!(
        coordinators
            .unregister(&gone, unregister_request(&dead))
            .await
            .expect("unregister"),
        UnregisterOutcome::Retiring { .. }
    ));
    assert!(matches!(
        coordinators
            .repair(&gone, &dead, repair_evidence())
            .await
            .expect("repair"),
        RepairOutcome::Completed { .. }
    ));
    assert_eq!(state_of(coordinators, &gone).await, None);
}

#[tokio::test]
async fn idle_accounts_are_reclaimed_and_bounded_slots_are_reused() {
    let harness = harness(
        "account-coordinator-idle",
        AccountCoordinatorConfig {
            max_resources: NonZeroUsize::MIN,
            idle_after: Duration::ZERO,
            ..config()
        },
    )
    .await;
    let coordinators = &harness.coordinators;
    let first = resource("first");
    let second = resource("second");
    let claim = harness.claim();
    let (binding, _) = activate(coordinators, &first).await;
    assert_eq!(coordinators.reclaim_idle().await, 0);
    assert_eq!(coordinators.loaded_accounts().await, 1);
    assert!(matches!(
        coordinators
            .execute(&second, operation(bind(&claim, INITIAL, &fresh_binding())))
            .await,
        Err(AccountCoordinatorError::Resources(
            AccountResourcesError::ResourceLimit
        ))
    ));

    assert!(matches!(
        coordinators
            .unregister(&first, unregister_request(&binding))
            .await
            .expect("unregister"),
        UnregisterOutcome::Retiring { .. }
    ));
    assert!(matches!(
        coordinators
            .repair(&first, &binding, repair_evidence())
            .await
            .expect("repair"),
        RepairOutcome::Completed { .. }
    ));
    let (second_binding, _) = activate(coordinators, &second).await;
    assert_eq!(
        harness.store.load(&first).await.expect("load"),
        None,
        "a free slot is given up for another resource"
    );

    assert!(matches!(
        coordinators
            .unregister(&second, unregister_request(&second_binding))
            .await
            .expect("unregister"),
        UnregisterOutcome::Retiring { .. }
    ));
    assert!(matches!(
        coordinators
            .repair(&second, &second_binding, repair_evidence())
            .await
            .expect("repair"),
        RepairOutcome::Completed { .. }
    ));
    assert_eq!(coordinators.reclaim_idle().await, 1);
    assert_eq!(coordinators.loaded_accounts().await, 0);
    assert_eq!(harness.store.load(&second).await.expect("load"), None);

    // The account is served again by a coordinator created on demand.
    assert!(resources(coordinators, &second).await.is_empty());
    assert_eq!(coordinators.loaded_accounts().await, 1);
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn a_store_failure_is_a_typed_error_and_never_an_empty_selection() {
    use waddle_server::account_bindings::InMemoryAccountBindingStore;

    let store = Arc::new(InMemoryAccountBindingStore::new());
    let claim = claim(1);
    let coordinators = AccountCoordinators::new(
        Arc::clone(&store) as Arc<dyn AccountBindingStore>,
        MovableClaim::new(claim.clone()) as Arc<dyn AccountClaimSource>,
        Arc::new(LocalNodeLiveness::new(NodeLeaseIdentity::local_process())),
        config(),
    );
    let jid = resource("fragile");
    let (binding, active) = activate(&coordinators, &jid).await;

    store.set_unavailable(true);
    let update_operation = operation(update(&claim, active, &binding, presence(true, 2, false)));
    assert!(matches!(
        coordinators.execute(&jid, update_operation.clone()).await,
        Err(AccountCoordinatorError::Store(_))
    ));
    assert!(
        matches!(
            coordinators.view(&jid.to_bare()).await,
            Err(AccountCoordinatorError::Store(_))
        ),
        "a read the coordinator cannot answer is an error, not an empty account"
    );

    store.set_unavailable(false);
    assert_eq!(resources(&coordinators, &jid).await, vec![jid.clone()]);
    assert!(matches!(
        coordinators
            .execute(&jid, update_operation)
            .await
            .expect("repeat the update"),
        BindingReply::Executed(AccountResourceExecution::Applied(
            AccountResourceOperationOutcome::Committed { .. }
        ))
    ));
}
