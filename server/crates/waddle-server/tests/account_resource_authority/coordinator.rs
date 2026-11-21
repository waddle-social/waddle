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
    AccountCoordinatorError, AccountCoordinators, BindingLiveness, BindingReply,
    ConnectionLiveness, LocalNodeLiveness, RepairEvidence, RepairOutcome, UnregisterOutcome,
    UnregisterRequest,
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
    bind, bind_session_generation, claim, complete_retirement, detach, fresh_binding,
    insert_session, operation, presence, publish, register, resource, resume, sm_binding, update,
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
    bind_session_generation(&harness.db, session, 2).await;
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
    bind_session_generation(&harness.db, session, 2).await;
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

#[tokio::test]
async fn a_terminal_session_is_released_by_retiring_its_detached_binding() {
    use waddle_server::account_coordinator::{ReleaseSessionRequest, SessionRelease};

    let harness = harness("account-coordinator-release-session", config()).await;
    let coordinators = &harness.coordinators;
    let jid = resource("resumable");
    let claim = harness.claim();
    let session = insert_session(&harness.db).await;
    let (binding, active) = activate(coordinators, &jid).await;
    let request = || ReleaseSessionRequest {
        begin: AccountResourceOperationId::fresh(),
        complete: AccountResourceOperationId::fresh(),
        session,
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
        directed_presence: DirectedPresenceTeardownDisposition::NoDirectedRecipients,
        presentation_flush: PresentationFlushDisposition::NoPendingPresentation,
    };
    assert_eq!(
        coordinators
            .release_session(&jid, request())
            .await
            .expect("release"),
        SessionRelease::NotReferenced
    );
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
    assert_eq!(
        coordinators
            .release_session(&jid, request())
            .await
            .expect("release"),
        SessionRelease::HeldByConnection(binding.clone(), AccountResourceBindingState::Active),
        "a binding that still has a connection is retired by that connection"
    );
    drive(coordinators, &jid, detach(&claim, updated, &binding)).await;
    assert_eq!(
        harness
            .store
            .resources_for_session(session)
            .await
            .expect("resources for the session"),
        vec![jid.clone()]
    );
    assert!(matches!(
        coordinators
            .release_session(&jid, request())
            .await
            .expect("release"),
        SessionRelease::Released { .. }
    ));
    assert!(
        harness
            .store
            .resources_for_session(session)
            .await
            .expect("resources for the session")
            .is_empty(),
        "the row no longer names the session, so the session row can be deleted"
    );
    assert_eq!(
        coordinators
            .release_session(&jid, request())
            .await
            .expect("repeat the release"),
        SessionRelease::NotReferenced
    );
}

#[tokio::test]
async fn an_account_over_its_slot_bound_serves_its_bindings_and_refuses_new_ones() {
    let harness = harness("account-coordinator-over-bound", config()).await;
    let first = resource("first");
    let second = resource("second");
    let claim = harness.claim();
    activate(&harness.coordinators, &first).await;
    activate(&harness.coordinators, &second).await;

    let bounded = directory(
        &harness.store,
        &harness.claims,
        AccountCoordinatorConfig {
            max_resources: NonZeroUsize::MIN,
            ..config()
        },
    );
    assert_eq!(
        resources(&bounded, &first).await,
        vec![first.clone(), second.clone()],
        "stored bindings beyond the bound stay served"
    );
    assert!(matches!(
        bounded
            .execute(
                &resource("third"),
                operation(bind(&claim, INITIAL, &fresh_binding()))
            )
            .await,
        Err(AccountCoordinatorError::Resources(
            AccountResourcesError::ResourceLimit
        ))
    ));
    let status = bounded.slot_status(&first).await.expect("slot status");
    let (binding, _) = status.binding.expect("the first binding");
    drive(
        &bounded,
        &first,
        update(&claim, status.revision, &binding, presence(true, 4, false)),
    )
    .await;
}

/// Liveness the test states per binding; anything else is unknown.
struct StatedLiveness(Vec<(BindingRef, BindingLiveness)>);

#[async_trait]
impl ConnectionLiveness for StatedLiveness {
    async fn liveness(
        &self,
        binding: &BindingRef,
        _session: Option<waddle_xmpp::ownership::SmSessionKey>,
    ) -> BindingLiveness {
        self.0
            .iter()
            .find(|(stated, _)| stated == binding)
            .map_or(BindingLiveness::Unknown, |(_, liveness)| *liveness)
    }
}

#[tokio::test]
async fn a_takeover_adopts_live_remote_bindings_and_retires_expired_ones() {
    let harness = harness("account-coordinator-remote-liveness", config()).await;
    let remote = |node: &str| {
        BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
            &NodeIdentity::new(node, "epoch"),
        )))
    };
    let live_jid = resource("remote-live");
    let expired_jid = resource("remote-expired");
    let unknown_jid = resource("remote-unknown");
    let live = remote("node-live");
    let expired = remote("node-expired");
    let unknown = remote("node-unknown");
    activate_binding(&harness.coordinators, &live_jid, &live).await;
    activate_binding(&harness.coordinators, &expired_jid, &expired).await;
    activate_binding(&harness.coordinators, &unknown_jid, &unknown).await;

    harness.claims.move_to(claim(2));
    let successor = AccountCoordinators::new(
        Arc::clone(&harness.store) as Arc<dyn AccountBindingStore>,
        Arc::clone(&harness.claims) as Arc<dyn AccountClaimSource>,
        Arc::new(StatedLiveness(vec![
            (live.clone(), BindingLiveness::Live),
            (expired.clone(), BindingLiveness::Expired),
        ])),
        config(),
    );
    let account = live_jid.to_bare();
    let report = successor
        .adopt_account(&account)
        .await
        .expect("adopt the account");
    assert_eq!(report.adopted, vec![(live_jid.clone(), live)]);
    assert_eq!(report.expired, vec![(expired_jid.clone(), expired.clone())]);
    assert_eq!(
        report.unadopted,
        vec![(unknown_jid.clone(), unknown.clone())]
    );
    assert_eq!(
        resources(&successor, &live_jid).await,
        vec![live_jid],
        "a binding hosted by a node with a fresh lease stays a destination"
    );
    assert_eq!(
        successor
            .slot_status(&expired_jid)
            .await
            .expect("slot status")
            .binding,
        None,
        "a binding on a lost connection is retired when the account is loaded"
    );
    assert!(
        successor
            .expired_bindings(&account)
            .await
            .expect("expired bindings")
            .is_empty(),
        "unknown liveness is not reported as expired"
    );
    assert_eq!(
        successor
            .slot_status(&unknown_jid)
            .await
            .expect("slot status")
            .binding,
        Some((unknown, AccountResourceBindingState::Active)),
        "a binding of unknown liveness is kept, not deleted"
    );
}

/// End the stored session row, as its expiry does.
async fn end_session(db: &Database, session: waddle_xmpp::ownership::SmSessionKey) {
    let connection = db.guard().await.expect("database connection");
    connection
        .execute(
            &format!(
                "UPDATE sm_durable_sessions SET state = 4, terminal_reason = 2 \
                 WHERE session_id = '{}'",
                session.as_uuid()
            ),
            (),
        )
        .await
        .expect("end the session");
}

#[tokio::test]
async fn a_takeover_keeps_a_lost_binding_detached_only_while_its_session_can_resume() {
    let harness = harness("account-coordinator-lost-sessions", config()).await;
    let current = harness.claim();
    let lost = || {
        BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
            &NodeIdentity::new("node-lost", "epoch"),
        )))
    };
    let resumable_jid = resource("lost-resumable");
    let ended_jid = resource("lost-ended");
    let detached_jid = resource("detached-ended");
    let mut bindings = Vec::new();
    for jid in [&resumable_jid, &ended_jid, &detached_jid] {
        let binding = lost();
        let revision = activate_binding(&harness.coordinators, jid, &binding).await;
        let session = insert_session(&harness.db).await;
        let snapshot = AccountResourceSnapshot {
            stream_management_ref: Some(sm_binding(session, 1)),
            ..presence(true, 0, false)
        };
        let revision = drive(
            &harness.coordinators,
            jid,
            update(&current, revision, &binding, snapshot),
        )
        .await;
        if jid == &detached_jid {
            drive(
                &harness.coordinators,
                jid,
                detach(&current, revision, &binding),
            )
            .await;
        }
        bindings.push((binding, session));
    }
    end_session(&harness.db, bindings[1].1).await;
    end_session(&harness.db, bindings[2].1).await;

    harness.claims.move_to(claim(2));
    let successor = AccountCoordinators::new(
        Arc::clone(&harness.store) as Arc<dyn AccountBindingStore>,
        Arc::clone(&harness.claims) as Arc<dyn AccountClaimSource>,
        Arc::new(StatedLiveness(
            bindings
                .iter()
                .map(|(binding, _)| (binding.clone(), BindingLiveness::Expired))
                .collect(),
        )),
        config(),
    );
    successor
        .adopt_account(&resumable_jid.to_bare())
        .await
        .expect("adopt the account");

    assert_eq!(
        successor
            .slot_status(&resumable_jid)
            .await
            .expect("slot status")
            .binding,
        Some((bindings[0].0.clone(), AccountResourceBindingState::Detached)),
        "a lost binding whose session can resume is kept as detached"
    );
    for jid in [&ended_jid, &detached_jid] {
        assert_eq!(
            successor
                .slot_status(jid)
                .await
                .expect("slot status")
                .binding,
            None,
            "a lost binding whose session has ended is retired: {jid}"
        );
    }
    for (_, session) in &bindings[1..] {
        assert!(
            harness
                .store
                .resources_for_session(*session)
                .await
                .expect("resources for the session")
                .is_empty(),
            "no row names an ended session"
        );
    }
}

#[tokio::test]
async fn the_binding_publisher_maps_publish_confirm_and_retire_onto_the_coordinator() {
    use waddle_server::account_coordinator::{
        ConfirmOutcome, CoordinatorBindingPublisher, PublishOutcome, RetireEvidence, RetireOutcome,
    };
    use waddle_xmpp::ownership::joint_identity::ResourceLifetime;

    let harness = harness("account-coordinator-publisher", config()).await;
    let coordinators = Arc::new(directory(&harness.store, &harness.claims, config()));
    let publisher = CoordinatorBindingPublisher::new(
        Arc::clone(&coordinators) as Arc<dyn AccountBindingAuthority>
    );
    let jid = resource("published");
    let evidence = || RetireEvidence {
        begin: AccountResourceOperationId::fresh(),
        complete: AccountResourceOperationId::fresh(),
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
        directed_presence: DirectedPresenceTeardownDisposition::NoDirectedRecipients,
        presentation_flush: PresentationFlushDisposition::NoPendingPresentation,
    };

    let first = fresh_binding();
    let PublishOutcome::Published(published) = publisher
        .publish(&jid, &first, ResourceLifetime::fresh())
        .await
        .expect("publish")
    else {
        panic!("expected the first binding to be published");
    };
    assert_eq!(
        state_of(&coordinators, &jid).await,
        Some(AccountResourceBindingState::Published)
    );
    assert_eq!(
        publisher
            .publish(&jid, &first, ResourceLifetime::fresh())
            .await
            .expect("repeat the publish"),
        PublishOutcome::Published(published.clone()),
        "a repeated publish reports the binding as it stands"
    );
    assert!(resources(&coordinators, &jid).await.is_empty());
    assert!(matches!(
        publisher.confirm(&jid, &published).await.expect("confirm"),
        ConfirmOutcome::Confirmed(_)
    ));
    assert!(matches!(
        publisher
            .confirm(&jid, &published)
            .await
            .expect("repeat the confirm"),
        ConfirmOutcome::Confirmed(_)
    ));
    assert_eq!(resources(&coordinators, &jid).await, vec![jid.clone()]);

    let second = fresh_binding();
    let pending = publisher
        .publish(&jid, &second, ResourceLifetime::fresh())
        .await
        .expect("publish the replacement");
    assert!(matches!(
        &pending,
        PublishOutcome::ReplacementPending { displaced, .. } if displaced == &first
    ));
    assert!(matches!(
        publisher
            .retire(&jid, &published, evidence())
            .await
            .expect("retire the displaced binding"),
        RetireOutcome::Retired { .. }
    ));
    let PublishOutcome::Published(replacement) = publisher
        .publish(&jid, &second, ResourceLifetime::fresh())
        .await
        .expect("publish the replacement again")
    else {
        panic!("expected the replacement to be published");
    };
    assert_eq!(
        publisher
            .confirm(&jid, &published)
            .await
            .expect("confirm the displaced binding"),
        ConfirmOutcome::NotCurrent,
        "a retired binding is never acknowledged to its client"
    );
    assert!(matches!(
        publisher
            .confirm(&jid, &replacement)
            .await
            .expect("confirm the replacement"),
        ConfirmOutcome::Confirmed(_)
    ));
    assert_eq!(
        publisher
            .retire_remote(&jid, &published, evidence())
            .await
            .expect("retire the displaced binding again"),
        RetireOutcome::Stale(BindingFenceError::Superseded),
        "retiring an old binding leaves its replacement alone"
    );
    assert_eq!(
        state_of(&coordinators, &jid).await,
        Some(AccountResourceBindingState::Active)
    );
}

#[tokio::test]
async fn cluster_lease_liveness_maps_every_state_and_tracks_rotated_local_incarnation() {
    use std::collections::HashMap;
    use waddle_server::account_coordinator::{ClusterLeaseLiveness, NodeLeaseRead};
    use waddle_xmpp::ownership::NodeLeaseState;

    struct MapLeaseRead(HashMap<NodeIdentity, Option<NodeLeaseState>>);

    #[async_trait]
    impl NodeLeaseRead for MapLeaseRead {
        async fn lease_state(&self, node: &NodeIdentity) -> Option<NodeLeaseState> {
            self.0.get(node).copied().flatten()
        }
    }

    let active_node = NodeIdentity::new("node-a", "epoch-new");
    let rotated_old = NodeIdentity::new("node-a", "epoch-old");
    let draining_node = NodeIdentity::new("node-b", "epoch-b");
    let expired_node = NodeIdentity::new("node-c", "epoch-c");
    let unreadable_node = NodeIdentity::new("node-d", "epoch-d");

    let mut states = HashMap::new();
    states.insert(active_node.clone(), Some(NodeLeaseState::Active));
    states.insert(rotated_old.clone(), Some(NodeLeaseState::Expired));
    states.insert(draining_node.clone(), Some(NodeLeaseState::Draining));
    states.insert(expired_node.clone(), Some(NodeLeaseState::Expired));
    states.insert(unreadable_node.clone(), None);

    let liveness = ClusterLeaseLiveness::new(Arc::new(MapLeaseRead(states)));
    let binding_for = |node: &NodeIdentity| {
        BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
            node,
        )))
    };

    assert_eq!(
        liveness.liveness(&binding_for(&active_node), None).await,
        BindingLiveness::Live
    );
    assert_eq!(
        liveness.liveness(&binding_for(&rotated_old), None).await,
        BindingLiveness::Expired,
        "a superseded local incarnation must not read as live"
    );
    assert_eq!(
        liveness.liveness(&binding_for(&draining_node), None).await,
        BindingLiveness::Draining
    );
    assert_eq!(
        liveness.liveness(&binding_for(&expired_node), None).await,
        BindingLiveness::Expired
    );
    assert_eq!(
        liveness
            .liveness(&binding_for(&unreadable_node), None)
            .await,
        BindingLiveness::Unknown
    );
    assert_eq!(
        liveness.liveness(&fresh_binding(), None).await,
        BindingLiveness::Unknown,
        "a standalone process binding has no clustered lease"
    );
}

#[tokio::test]
async fn takeover_leaves_draining_bindings_unadopted_and_settles_only_after_expiry() {
    use std::collections::HashMap;
    use waddle_server::account_coordinator::{
        retire_expired_bindings_page, EXPIRED_BINDING_SWEEP_ACCOUNTS,
    };

    struct MutableLiveness(Mutex<HashMap<BindingRef, BindingLiveness>>);

    impl MutableLiveness {
        fn set(&self, binding: &BindingRef, liveness: BindingLiveness) {
            self.0
                .lock()
                .expect("liveness lock")
                .insert(binding.clone(), liveness);
        }
    }

    #[async_trait]
    impl ConnectionLiveness for MutableLiveness {
        async fn liveness(
            &self,
            binding: &BindingRef,
            _session: Option<waddle_xmpp::ownership::SmSessionKey>,
        ) -> BindingLiveness {
            self.0
                .lock()
                .expect("liveness lock")
                .get(binding)
                .copied()
                .unwrap_or(BindingLiveness::Unknown)
        }
    }

    let harness = harness("account-coordinator-draining-takeover", config()).await;
    let current = harness.claim();
    let draining_node = || {
        BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
            &NodeIdentity::new("node-draining", "epoch-1"),
        )))
    };

    let ephemeral_jid = resource("draining-ephemeral");
    let sm_jid = resource("draining-sm");
    let ephemeral_binding = draining_node();
    let sm_binding_ref = draining_node();

    activate_binding(&harness.coordinators, &ephemeral_jid, &ephemeral_binding).await;
    let sm_rev = activate_binding(&harness.coordinators, &sm_jid, &sm_binding_ref).await;
    let session = insert_session(&harness.db).await;
    let snapshot = AccountResourceSnapshot {
        stream_management_ref: Some(sm_binding(session, 1)),
        ..presence(true, 0, false)
    };
    drive(
        &harness.coordinators,
        &sm_jid,
        update(&current, sm_rev, &sm_binding_ref, snapshot),
    )
    .await;

    let liveness = Arc::new(MutableLiveness(Mutex::new(HashMap::from([
        (ephemeral_binding.clone(), BindingLiveness::Draining),
        (sm_binding_ref.clone(), BindingLiveness::Draining),
    ]))));

    harness.claims.move_to(claim(2));
    let successor = Arc::new(AccountCoordinators::new(
        Arc::clone(&harness.store) as Arc<dyn AccountBindingStore>,
        Arc::clone(&harness.claims) as Arc<dyn AccountClaimSource>,
        Arc::clone(&liveness) as Arc<dyn ConnectionLiveness>,
        config(),
    ));
    let account = ephemeral_jid.to_bare();
    let report = successor
        .adopt_account(&account)
        .await
        .expect("adopt account while host is draining");

    assert!(
        report.adopted.is_empty(),
        "draining bindings must not be adopted as live destinations"
    );
    assert!(
        report.expired.is_empty(),
        "draining bindings must not be treated as expired during rolling drain"
    );
    assert_eq!(report.unadopted.len(), 2);
    assert!(
        resources(&successor, &ephemeral_jid).await.is_empty(),
        "unadopted draining bindings must not be routed to"
    );
    assert_eq!(
        successor
            .slot_status(&ephemeral_jid)
            .await
            .expect("ephemeral status")
            .binding,
        Some((
            ephemeral_binding.clone(),
            AccountResourceBindingState::Active
        )),
        "ephemeral binding remains intact while host is still draining"
    );
    assert_eq!(
        successor
            .slot_status(&sm_jid)
            .await
            .expect("sm status")
            .binding,
        Some((sm_binding_ref.clone(), AccountResourceBindingState::Active)),
        "SM binding is not prematurely detached while host is still draining"
    );

    let mut cursor = None;
    assert_eq!(
        retire_expired_bindings_page(&successor, &mut cursor, EXPIRED_BINDING_SWEEP_ACCOUNTS).await,
        0,
        "expired-binding sweep leaves draining bindings alone"
    );

    // Once the draining pod's lease expires, the sweep retires the ephemeral
    // binding and adopts-then-detaches the SM binding so its resume right
    // stays intact.
    liveness.set(&ephemeral_binding, BindingLiveness::Expired);
    liveness.set(&sm_binding_ref, BindingLiveness::Expired);

    assert_eq!(
        retire_expired_bindings_page(&successor, &mut cursor, EXPIRED_BINDING_SWEEP_ACCOUNTS).await,
        1,
        "ephemeral binding is retired once its node expires"
    );
    assert_eq!(
        successor
            .slot_status(&ephemeral_jid)
            .await
            .expect("ephemeral status after expiry")
            .binding,
        None
    );
    assert_eq!(
        successor
            .slot_status(&sm_jid)
            .await
            .expect("sm status after expiry")
            .binding,
        Some((sm_binding_ref, AccountResourceBindingState::Detached)),
        "unadopted SM binding transitions to Detached upon expiry to preserve XEP-0198 resume"
    );
    assert!(resources(&successor, &sm_jid).await.is_empty());
}

#[tokio::test]
async fn expired_binding_reaper_cursor_scans_short_pages_every_sweep_and_paginates_large_sets() {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use waddle_server::account_coordinator::retire_expired_bindings_page;

    struct AccountClaimMap(Mutex<HashMap<BareJid, UserClaimIdentity>>);

    impl AccountClaimMap {
        fn set(&self, account: &BareJid, claim_id: UserClaimIdentity) {
            self.0
                .lock()
                .expect("claim map lock")
                .insert(account.clone(), claim_id);
        }
    }

    #[async_trait]
    impl AccountClaimSource for AccountClaimMap {
        async fn current_claim(
            &self,
            account: &BareJid,
        ) -> Result<UserClaimIdentity, AccountClaimError> {
            self.0
                .lock()
                .expect("claim map lock")
                .get(account)
                .cloned()
                .ok_or(AccountClaimError::HeldElsewhere)
        }
    }

    struct ToggleLiveness(AtomicBool);

    #[async_trait]
    impl ConnectionLiveness for ToggleLiveness {
        async fn liveness(
            &self,
            _binding: &BindingRef,
            _session: Option<waddle_xmpp::ownership::SmSessionKey>,
        ) -> BindingLiveness {
            if self.0.load(Ordering::SeqCst) {
                BindingLiveness::Expired
            } else {
                BindingLiveness::Live
            }
        }
    }

    let db = Database::in_memory("account-coordinator-reaper-cursor")
        .await
        .expect("in-memory database");
    let store = Arc::new(
        DatabaseAccountBindingStore::open(db, false)
            .await
            .expect("open store"),
    );
    let claims = Arc::new(AccountClaimMap(Mutex::new(HashMap::new())));
    let expired = Arc::new(ToggleLiveness(AtomicBool::new(false)));
    let coordinators = Arc::new(AccountCoordinators::new(
        Arc::clone(&store) as Arc<dyn AccountBindingStore>,
        Arc::clone(&claims) as Arc<dyn AccountClaimSource>,
        Arc::clone(&expired) as Arc<dyn ConnectionLiveness>,
        config(),
    ));
    let make_claim = |account: &BareJid, epoch: i64| {
        UserClaimIdentity::fresh(
            account.clone(),
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-a")),
            waddle_xmpp::ownership::ClaimEpoch(epoch),
        )
    };

    // Part 1: short page (< page_limit) must reset cursor to None so the very
    // next sweep does NOT waste a full interval on an empty tail query.
    let alice: FullJid = "alice@example.test/desk".parse().expect("alice jid");
    let bob: FullJid = "bob@example.test/desk".parse().expect("bob jid");
    for jid in [&alice, &bob] {
        claims.set(&jid.to_bare(), make_claim(&jid.to_bare(), 1));
        activate_binding(&coordinators, jid, &fresh_binding()).await;
    }

    expired.0.store(true, Ordering::SeqCst);
    let mut cursor = None;
    let retired = retire_expired_bindings_page(&coordinators, &mut cursor, 10).await;
    assert_eq!(retired, 2);
    assert_eq!(
        cursor, None,
        "a short page (< limit) must reset the cursor immediately instead of skipping the next sweep"
    );
    assert_eq!(
        coordinators
            .slot_status(&alice)
            .await
            .expect("alice status")
            .binding,
        None
    );
    assert_eq!(
        coordinators
            .slot_status(&bob)
            .await
            .expect("bob status")
            .binding,
        None
    );

    // Re-activate alice and expire it again; the immediately following sweep
    // must settle it without an empty wrap-around tick.
    expired.0.store(false, Ordering::SeqCst);
    activate_binding(&coordinators, &alice, &fresh_binding()).await;
    expired.0.store(true, Ordering::SeqCst);
    let retired_next = retire_expired_bindings_page(&coordinators, &mut cursor, 10).await;
    assert_eq!(retired_next, 1);
    assert_eq!(cursor, None);
    assert_eq!(
        coordinators
            .slot_status(&alice)
            .await
            .expect("alice status after second sweep")
            .binding,
        None
    );

    // Part 2: multi-page set (5 accounts with limit 2) paginates across 3
    // sweeps and resets to None on the short final page.
    expired.0.store(false, Ordering::SeqCst);
    let mut users = Vec::new();
    for index in 1..=5 {
        let jid: FullJid = format!("user{index}@example.test/r")
            .parse()
            .expect("user jid");
        claims.set(&jid.to_bare(), make_claim(&jid.to_bare(), 1));
        activate_binding(&coordinators, &jid, &fresh_binding()).await;
        users.push(jid);
    }
    expired.0.store(true, Ordering::SeqCst);

    let page1 = retire_expired_bindings_page(&coordinators, &mut cursor, 2).await;
    assert_eq!(page1, 2);
    assert!(cursor.is_some(), "full page advances pagination cursor");

    let page2 = retire_expired_bindings_page(&coordinators, &mut cursor, 2).await;
    assert_eq!(page2, 2);
    assert!(cursor.is_some(), "second full page advances cursor");

    let page3 = retire_expired_bindings_page(&coordinators, &mut cursor, 2).await;
    assert_eq!(page3, 1);
    assert_eq!(cursor, None, "short final page wraps cursor back to None");

    for jid in &users {
        assert_eq!(
            coordinators
                .slot_status(jid)
                .await
                .expect("user status")
                .binding,
            None,
            "every account in the multi-page sweep must be settled: {jid}"
        );
    }
}
