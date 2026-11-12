//! Fences and idempotence of every binding operation of the store.

use waddle_server::account_bindings::{
    AccountBindingStore, AccountBindingStoreError, DatabaseAccountBindingStore, PresentationMark,
};
use waddle_server::db::Database;
use waddle_xmpp::ownership::account_coordinator::{
    BindingFenceError, Presentation, PresentationFlags,
};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecution, AccountResourceMutation,
    AccountResourceOperation, AccountResourceOperationOutcome, AccountResourceRevision,
    AccountResourceSnapshot, AccountResourceTransitionError, ACCOUNT_RESOURCE_OPERATION_CAPACITY,
};
use waddle_xmpp::ownership::{SmSessionKey, UserClaimIdentity};
use xmpp_parsers::presence::Show;

use super::support::{
    activate, begin_retirement, bind, bind_session_generation, claim, commit, complete_retirement,
    detach, fresh_binding, insert_session, operation, presence, publish, register, registration,
    reject, resource, resume, sm_binding, update,
};

const INITIAL: AccountResourceRevision = AccountResourceRevision::INITIAL;

/// A store under test and how it makes a stream-management session exist,
/// so a snapshot may reference it.
struct Fixture {
    store: Box<dyn AccountBindingStore>,
    db: Option<Database>,
}

impl Fixture {
    async fn sqlite(name: &str) -> Self {
        let db = Database::in_memory(name).await.expect("in-memory database");
        let store = DatabaseAccountBindingStore::open(db.clone(), false)
            .await
            .expect("open the store");
        Self {
            store: Box::new(store),
            db: Some(db),
        }
    }

    #[cfg(feature = "test-support")]
    fn memory() -> Self {
        Self {
            store: Box::new(waddle_server::account_bindings::InMemoryAccountBindingStore::new()),
            db: None,
        }
    }

    fn store(&self) -> &dyn AccountBindingStore {
        self.store.as_ref()
    }

    /// A stream-management session a snapshot may reference. The
    /// in-memory store keeps no session relation, so any key will do.
    async fn session(&self) -> SmSessionKey {
        match &self.db {
            Some(db) => insert_session(db).await,
            None => SmSessionKey::fresh(),
        }
    }

    /// Record that the session committed a resume to `generation`.
    async fn resumed(&self, session: SmSessionKey, generation: u64) {
        if let Some(db) = &self.db {
            bind_session_generation(db, session, generation).await;
        }
    }
}

async fn state_of(
    store: &dyn AccountBindingStore,
    name: &str,
) -> Option<AccountResourceBindingState> {
    store
        .load(&resource(name))
        .await
        .expect("load the row")
        .and_then(|stored| stored.image.binding.map(|binding| binding.state))
}

async fn bind_is_idempotent_by_operation_id(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("phone");
    let claim = claim(1);
    let binding = fresh_binding();
    let first = operation(bind(&claim, INITIAL, &binding));
    let applied = store.execute(&authority, &jid, &first).await.expect("bind");
    let AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { revision }) =
        applied.execution
    else {
        panic!("expected the bind to commit, got {:?}", applied.execution);
    };
    assert_eq!(
        applied
            .stored
            .expect("the committed row")
            .image
            .install_generation
            .value(),
        1
    );
    assert_eq!(
        state_of(store, "phone").await,
        Some(AccountResourceBindingState::Prepared)
    );

    let replayed = store
        .execute(&authority, &jid, &first)
        .await
        .expect("repeat the bind");
    assert_eq!(
        replayed.execution,
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed { revision })
    );
    assert_eq!(replayed.stored, None);

    let reused = AccountResourceOperation::new(first.id, bind(&claim, INITIAL, &fresh_binding()));
    assert!(matches!(
        store.execute(&authority, &jid, &reused).await,
        Err(AccountBindingStoreError::OperationIdConflict)
    ));
    let other_resource = store.execute(&authority, &resource("tablet"), &first).await;
    assert!(matches!(
        other_resource,
        Err(AccountBindingStoreError::OperationIdConflict)
    ));
}

async fn only_a_bind_creates_a_row(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("ghost");
    let claim = claim(1);
    let binding = fresh_binding();
    assert!(matches!(
        store
            .execute(
                &authority,
                &jid,
                &operation(publish(&claim, INITIAL, &binding))
            )
            .await,
        Err(AccountBindingStoreError::UnknownResource)
    ));
    assert_eq!(store.load(&jid).await.expect("load"), None);
    let foreign: jid::FullJid = "bob@example.test/phone".parse().expect("valid full JID");
    assert!(matches!(
        store
            .execute(
                &authority,
                &foreign,
                &operation(bind(&claim, INITIAL, &binding))
            )
            .await,
        Err(AccountBindingStoreError::AccountMismatch)
    ));
}

async fn publish_and_register_are_fenced(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("desk");
    let claim = claim(1);
    let binding = fresh_binding();
    let bound = commit(store, &jid, bind(&claim, INITIAL, &binding)).await;

    assert_eq!(
        reject(store, &jid, publish(&claim, INITIAL, &binding)).await,
        AccountResourceTransitionError::RevisionConflict
    );
    assert_eq!(
        reject(store, &jid, publish(&claim, bound, &fresh_binding())).await,
        AccountResourceTransitionError::BindingMismatch
    );
    assert_eq!(
        reject(
            store,
            &jid,
            publish(&super::support::claim(2), bound, &binding)
        )
        .await,
        AccountResourceTransitionError::ClaimMismatch
    );
    assert_eq!(
        reject(store, &jid, register(&claim, bound, &binding)).await,
        AccountResourceTransitionError::LifecycleConflict
    );
    assert_eq!(
        state_of(store, "desk").await,
        Some(AccountResourceBindingState::Prepared),
        "a rejection writes nothing"
    );

    let published = commit(store, &jid, publish(&claim, bound, &binding)).await;
    assert_eq!(
        state_of(store, "desk").await,
        Some(AccountResourceBindingState::Published)
    );
    let register_operation = operation(register(&claim, published, &binding));
    let registered = store
        .execute(&authority, &jid, &register_operation)
        .await
        .expect("register");
    assert!(matches!(
        registered.execution,
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { .. })
    ));
    assert_eq!(
        state_of(store, "desk").await,
        Some(AccountResourceBindingState::Active)
    );
    assert!(matches!(
        store
            .execute(&authority, &jid, &register_operation)
            .await
            .expect("repeat the registration")
            .execution,
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed { .. })
    ));
    assert_eq!(
        reject(store, &jid, bind(&claim, published, &fresh_binding())).await,
        AccountResourceTransitionError::RevisionConflict
    );
}

async fn snapshot_updates_are_fenced_and_stored(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("laptop");
    let claim = claim(1);
    let (binding, active) = activate(store, &jid, &claim, INITIAL).await;
    let snapshot = AccountResourceSnapshot {
        presence_show: Some(Show::Dnd),
        presence_status: Some("in a meeting".to_owned()),
        roster_interested: true,
        blocklist_interested: true,
        ..presence(true, 7, true)
    };
    assert_eq!(
        reject(
            store,
            &jid,
            update(&claim, active, &fresh_binding(), snapshot.clone())
        )
        .await,
        AccountResourceTransitionError::BindingMismatch
    );
    let update_operation = operation(update(&claim, active, &binding, snapshot.clone()));
    let updated = store
        .execute(&authority, &jid, &update_operation)
        .await
        .expect("update");
    let AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { revision }) =
        updated.execution
    else {
        panic!("expected the update to commit, got {:?}", updated.execution);
    };
    let stored = store
        .load(&jid)
        .await
        .expect("load")
        .expect("the resource row");
    let stored_binding = stored.image.binding.expect("a current binding");
    assert_eq!(stored_binding.snapshot, snapshot);
    assert_eq!(stored_binding.snapshot_revision, revision);
    assert_eq!(stored.image.revision, revision);
    assert_eq!(
        store
            .execute(&authority, &jid, &update_operation)
            .await
            .expect("repeat the update")
            .execution,
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed { revision })
    );
    assert_eq!(
        reject(
            store,
            &jid,
            update(&claim, active, &binding, presence(false, 0, false))
        )
        .await,
        AccountResourceTransitionError::RevisionConflict,
        "an update that names the revision before the last commit is stale"
    );
}

async fn detach_and_resume_keep_the_slot(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("resumable");
    let claim = claim(1);
    let session = fixture.session().await;
    let (binding, active) = activate(store, &jid, &claim, INITIAL).await;
    assert_eq!(
        reject(store, &jid, detach(&claim, active, &binding)).await,
        AccountResourceTransitionError::ResumeRequiresStreamManagement
    );
    let snapshot = AccountResourceSnapshot {
        stream_management_ref: Some(sm_binding(session, 1)),
        ..presence(true, 3, true)
    };
    let updated = commit(store, &jid, update(&claim, active, &binding, snapshot)).await;
    assert_eq!(
        store
            .mark_presented(&authority, &jid, &binding, Presentation::OfflineMessages)
            .await
            .expect("mark"),
        PresentationMark::First
    );
    let detached = commit(store, &jid, detach(&claim, updated, &binding)).await;
    assert_eq!(
        state_of(store, "resumable").await,
        Some(AccountResourceBindingState::Detached)
    );
    let before = store
        .load(&jid)
        .await
        .expect("load")
        .expect("the resource row");
    let lifetime = before.image.binding.expect("a detached binding").lifetime;

    let resumed_binding = fresh_binding();
    assert_eq!(
        reject(
            store,
            &jid,
            resume(
                &claim,
                detached,
                &resumed_binding,
                &fresh_binding(),
                sm_binding(session, 1),
                lifetime,
            )
        )
        .await,
        AccountResourceTransitionError::BindingMismatch,
        "a resume must name the detached binding"
    );
    fixture.resumed(session, 2).await;
    let resume_operation = operation(resume(
        &claim,
        detached,
        &binding,
        &resumed_binding,
        sm_binding(session, 1),
        lifetime,
    ));
    let resumed = store
        .execute(&authority, &jid, &resume_operation)
        .await
        .expect("resume");
    let AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { revision }) =
        resumed.execution
    else {
        panic!("expected the resume to commit, got {:?}", resumed.execution);
    };
    let stored = resumed.stored.expect("the committed row");
    let current = stored.image.binding.expect("the resumed binding");
    assert_eq!(current.binding_ref, resumed_binding);
    assert_eq!(current.state, AccountResourceBindingState::Prepared);
    assert_eq!(current.lifetime, lifetime);
    assert_eq!(current.snapshot.presence_priority, 3);
    assert_eq!(
        current.snapshot.stream_management_ref,
        Some(sm_binding(session, 2))
    );
    assert_eq!(stored.image.install_generation.value(), 2);
    assert_eq!(
        stored.presented,
        PresentationFlags {
            offline_messages: true,
            pending_subscriptions: false,
        },
        "a resume keeps the session's presentation record"
    );
    assert_eq!(
        store
            .execute(&authority, &jid, &resume_operation)
            .await
            .expect("repeat the resume")
            .execution,
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed { revision })
    );
    assert_eq!(
        store
            .mark_presented(&authority, &jid, &binding, Presentation::OfflineMessages)
            .await
            .expect("mark"),
        PresentationMark::Fenced(BindingFenceError::Superseded)
    );
    assert_eq!(
        store
            .mark_presented(
                &authority,
                &jid,
                &resumed_binding,
                Presentation::OfflineMessages
            )
            .await
            .expect("mark"),
        PresentationMark::AlreadyPresented
    );
}

async fn retirement_frees_the_row_and_forget_deletes_it(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("leaving");
    let claim = claim(1);
    let (binding, active) = activate(store, &jid, &claim, INITIAL).await;
    assert_eq!(
        store
            .mark_presented(
                &authority,
                &jid,
                &binding,
                Presentation::PendingSubscriptions
            )
            .await
            .expect("mark"),
        PresentationMark::First
    );
    assert_eq!(
        reject(
            store,
            &jid,
            begin_retirement(&claim, active, &fresh_binding())
        )
        .await,
        AccountResourceTransitionError::BindingMismatch
    );
    let begin_operation = operation(begin_retirement(&claim, active, &binding));
    let retiring = match store
        .execute(&authority, &jid, &begin_operation)
        .await
        .expect("begin retirement")
        .execution
    {
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
            revision,
        }) => revision,
        other => panic!("expected the retirement to start, got {other:?}"),
    };
    assert_eq!(
        store
            .execute(&authority, &jid, &begin_operation)
            .await
            .expect("repeat the start")
            .execution,
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
            revision: retiring
        })
    );
    assert_eq!(
        state_of(store, "leaving").await,
        Some(AccountResourceBindingState::Retiring)
    );
    let snapshot_revision = store
        .load(&jid)
        .await
        .expect("load")
        .and_then(|stored| stored.image.binding)
        .expect("a retiring binding")
        .snapshot_revision;
    assert_eq!(
        reject(
            store,
            &jid,
            complete_retirement(&claim, retiring, &binding, &claim, retiring)
        )
        .await,
        AccountResourceTransitionError::RetirementEvidenceMismatch,
        "evidence for another snapshot revision does not complete the retirement"
    );
    assert!(!store
        .forget(&authority, &jid, retiring)
        .await
        .expect("forget"));
    let complete_operation = operation(complete_retirement(
        &claim,
        retiring,
        &binding,
        &claim,
        snapshot_revision,
    ));
    let retired = match store
        .execute(&authority, &jid, &complete_operation)
        .await
        .expect("complete retirement")
        .execution
    {
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
            revision,
        }) => revision,
        other => panic!("expected the retirement to complete, got {other:?}"),
    };
    let stored = store.load(&jid).await.expect("load").expect("the free row");
    assert_eq!(stored.image.binding, None);
    assert_eq!(stored.presented, PresentationFlags::default());
    assert_eq!(
        store
            .execute(&authority, &jid, &complete_operation)
            .await
            .expect("repeat the completion")
            .execution,
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
            revision: retired
        })
    );
    assert_eq!(
        store
            .mark_presented(&authority, &jid, &binding, Presentation::OfflineMessages)
            .await
            .expect("mark"),
        PresentationMark::Fenced(BindingFenceError::NotBound)
    );

    assert!(!store
        .forget(&authority, &jid, retiring)
        .await
        .expect("forget"));
    assert!(store
        .forget(&authority, &jid, retired)
        .await
        .expect("forget"));
    assert_eq!(store.load(&jid).await.expect("load"), None);
    assert!(
        matches!(
            store
                .execute(&authority, &jid, &complete_operation)
                .await
                .expect_err("the forgotten resource has no row"),
            AccountBindingStoreError::UnknownResource
        ),
        "forgetting a resource drops its recorded operations"
    );
}

async fn claim_replacement_and_adoption_are_fenced(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("moved");
    let old: UserClaimIdentity = claim(1);
    let new: UserClaimIdentity = claim(2);
    let (binding, active) = activate(store, &jid, &old, INITIAL).await;
    let replace = |revision| AccountResourceMutation::ReplaceClaim {
        claim_identity: old.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        replacement_claim_identity: new.clone(),
    };
    assert_eq!(
        reject(store, &jid, replace(INITIAL)).await,
        AccountResourceTransitionError::RevisionConflict
    );
    let replace_operation = operation(replace(active));
    let replaced = match store
        .execute(&authority, &jid, &replace_operation)
        .await
        .expect("replace the claim")
        .execution
    {
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
            revision,
        }) => revision,
        other => panic!("expected the claim to be replaced, got {other:?}"),
    };
    assert_eq!(
        store
            .execute(&authority, &jid, &replace_operation)
            .await
            .expect("repeat the replacement")
            .execution,
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
            revision: replaced
        })
    );
    assert_eq!(
        reject(
            store,
            &jid,
            update(&old, replaced, &binding, presence(true, 1, false))
        )
        .await,
        AccountResourceTransitionError::ClaimMismatch,
        "the previous claim can no longer write the resource"
    );
    assert_eq!(
        reject(
            store,
            &jid,
            update(&new, replaced, &binding, presence(true, 1, false))
        )
        .await,
        AccountResourceTransitionError::BindingOwnerMismatch,
        "the binding is not the new claim's until it is adopted"
    );
    let adopt = |expected: &UserClaimIdentity| AccountResourceMutation::AdoptBinding {
        claim_identity: new.clone(),
        expected_revision: replaced,
        binding_ref: binding.clone(),
        expected_binding_claim_identity: expected.clone(),
        actor_registration: Some(registration(&new, &binding)),
    };
    assert_eq!(
        reject(store, &jid, adopt(&new)).await,
        AccountResourceTransitionError::BindingOwnerMismatch
    );
    let adopted = commit(store, &jid, adopt(&old)).await;
    let stored = store
        .load(&jid)
        .await
        .expect("load")
        .expect("the resource row");
    assert_eq!(stored.image.claim_identity, new);
    let current = stored.image.binding.expect("the adopted binding");
    assert_eq!(current.binding_claim_identity, new);
    assert_eq!(current.binding_ref, binding);
    assert_eq!(current.state, AccountResourceBindingState::Active);
    commit(
        store,
        &jid,
        update(&new, adopted, &binding, presence(true, 1, false)),
    )
    .await;
}

async fn presentations_are_recorded_once(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("flush");
    let claim = claim(1);
    assert_eq!(
        store
            .mark_presented(
                &authority,
                &jid,
                &fresh_binding(),
                Presentation::OfflineMessages
            )
            .await
            .expect("mark"),
        PresentationMark::Fenced(BindingFenceError::UnknownResource)
    );
    let (binding, _) = activate(store, &jid, &claim, INITIAL).await;
    for presentation in [
        Presentation::OfflineMessages,
        Presentation::PendingSubscriptions,
    ] {
        assert_eq!(
            store
                .mark_presented(&authority, &jid, &binding, presentation)
                .await
                .expect("mark"),
            PresentationMark::First
        );
        assert_eq!(
            store
                .mark_presented(&authority, &jid, &binding, presentation)
                .await
                .expect("mark"),
            PresentationMark::AlreadyPresented
        );
    }
    assert_eq!(
        store
            .mark_presented(
                &authority,
                &jid,
                &fresh_binding(),
                Presentation::OfflineMessages
            )
            .await
            .expect("mark"),
        PresentationMark::Fenced(BindingFenceError::Superseded)
    );
}

async fn recorded_operations_are_bounded_per_resource(fixture: &Fixture) {
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("busy");
    let other = resource("quiet");
    let claim = claim(1);
    let binding = fresh_binding();
    let first = operation(bind(&claim, INITIAL, &binding));
    store.execute(&authority, &jid, &first).await.expect("bind");
    let quiet_binding = fresh_binding();
    let quiet = operation(bind(&claim, INITIAL, &quiet_binding));
    store
        .execute(&authority, &other, &quiet)
        .await
        .expect("bind");

    let mut revision = commit(
        store,
        &jid,
        publish(&claim, AccountResourceRevision::new(1), &binding),
    )
    .await;
    revision = commit(store, &jid, register(&claim, revision, &binding)).await;
    for priority in 0..ACCOUNT_RESOURCE_OPERATION_CAPACITY {
        let priority = i8::try_from(priority % 100).expect("small priority");
        revision = commit(
            store,
            &jid,
            update(&claim, revision, &binding, presence(true, priority, false)),
        )
        .await;
    }
    assert_eq!(
        store
            .execute(&authority, &jid, &first)
            .await
            .expect("repeat the pruned bind")
            .execution,
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Rejected(
            AccountResourceTransitionError::RevisionConflict
        )),
        "a pruned operation is decided afresh and its stale revision refuses it"
    );
    assert!(
        matches!(
            store
                .execute(&authority, &other, &quiet)
                .await
                .expect("repeat the other resource's bind")
                .execution,
            AccountResourceExecution::Replayed(_)
        ),
        "pruning one resource keeps another resource's operations"
    );
    let account = store
        .load_account(&jid.to_bare())
        .await
        .expect("load the account");
    assert_eq!(
        account
            .iter()
            .map(|stored| stored.image.full_jid.clone())
            .collect::<Vec<_>>(),
        vec![jid, other]
    );
}

async fn every_case(fixture: &Fixture) {
    bind_is_idempotent_by_operation_id(fixture).await;
    only_a_bind_creates_a_row(fixture).await;
    publish_and_register_are_fenced(fixture).await;
    snapshot_updates_are_fenced_and_stored(fixture).await;
    detach_and_resume_keep_the_slot(fixture).await;
    retirement_frees_the_row_and_forget_deletes_it(fixture).await;
    claim_replacement_and_adoption_are_fenced(fixture).await;
    presentations_are_recorded_once(fixture).await;
    recorded_operations_are_bounded_per_resource(fixture).await;
}

#[tokio::test]
async fn sqlite_store_fences_and_replays_every_operation() {
    every_case(&Fixture::sqlite("account-bindings-cases").await).await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn in_memory_store_fences_and_replays_every_operation() {
    every_case(&Fixture::memory()).await;
}

#[tokio::test]
async fn sqlite_open_is_repeatable_and_adopts_existing_rows() {
    let db = Database::in_memory("account-bindings-open")
        .await
        .expect("in-memory database");
    let first = DatabaseAccountBindingStore::open(db.clone(), false)
        .await
        .expect("first open");
    let jid = resource("kept");
    let (binding, _) = activate(&first, &jid, &claim(1), INITIAL).await;
    let second = DatabaseAccountBindingStore::open(db, false)
        .await
        .expect("second open over the same relations");
    let stored = second
        .load(&jid)
        .await
        .expect("load")
        .expect("the row the first store wrote");
    assert_eq!(
        stored.image.binding.expect("a current binding").binding_ref,
        binding
    );
    assert!(!second.is_clustered());
}

#[tokio::test]
async fn a_clustered_store_refuses_sqlite() {
    let db = Database::in_memory("account-bindings-clustered")
        .await
        .expect("in-memory database");
    assert!(matches!(
        DatabaseAccountBindingStore::open(db, true).await,
        Err(AccountBindingStoreError::ClusterRequiresPostgres)
    ));
}

#[tokio::test]
async fn sqlite_checks_the_session_reference_at_commit() {
    let fixture = Fixture::sqlite("account-bindings-session-reference").await;
    let store = fixture.store();
    let authority = super::support::claim(1);
    let jid = resource("orphan");
    let claim = claim(1);
    let (binding, active) = activate(store, &jid, &claim, INITIAL).await;
    let reference = |session, generation| AccountResourceSnapshot {
        stream_management_ref: Some(sm_binding(session, generation)),
        ..presence(true, 0, false)
    };
    let attempt = |snapshot| operation(update(&claim, active, &binding, snapshot));
    assert!(
        matches!(
            store
                .execute(
                    &authority,
                    &jid,
                    &attempt(reference(SmSessionKey::fresh(), 1))
                )
                .await,
            Err(AccountBindingStoreError::SessionUnknown)
        ),
        "a snapshot that references no stored session does not commit"
    );
    let session = fixture.session().await;
    assert!(
        matches!(
            store
                .execute(&authority, &jid, &attempt(reference(session, 2)))
                .await,
            Err(AccountBindingStoreError::SessionGenerationMismatch)
        ),
        "a snapshot must name the generation the session is bound at"
    );
    let stored = store
        .load(&jid)
        .await
        .expect("load")
        .expect("the resource row");
    assert_eq!(stored.image.revision, active);
    assert!(store
        .resources_for_session(session)
        .await
        .expect("resources for the session")
        .is_empty());
    commit(
        store,
        &jid,
        update(&claim, active, &binding, reference(session, 1)),
    )
    .await;
    assert_eq!(
        store
            .resources_for_session(session)
            .await
            .expect("resources for the session"),
        vec![jid]
    );
}
