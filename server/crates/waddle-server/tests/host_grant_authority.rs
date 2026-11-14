//! The canonical host-grant store (ADR-0020 D5, first slice).
//!
//! These cases cover the canonical PostgreSQL store on its own: enrollment,
//! mutation and its linearisation on the scope's generation, the difference
//! between a recorded and an effective grant, exact retries and identity
//! reuse, revalidation of the initiator under the scope's guard, issuance to
//! remote accounts under a supplied trusted-home set, exhaustion, readback of
//! an unacknowledged commit, and the runtime role's capabilities.
//!
//! They need PostgreSQL only. Where a case needs effective grants, the
//! fixture activates the scope itself in place of a verified projection; the
//! projection, its marker and its recovery are covered against SpiceDB in
//! `host_grant_projection.rs`, and admission inside the acceptance
//! transaction in `xep0045_host_grant_admission.rs`.
#![cfg(feature = "test-support")]

mod host_grant_support;
mod ingress_support;

use std::time::Duration;

use host_grant_support::{bare, issuance_policy, subject, GrantFixture, OWNER, PEER, SENDER};
use jid::Jid;
use uuid::Uuid;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::permissions::host_grant::{
    affiliation_allows, with_host_grant_commit_fault, Activation, AdmissionState, BootstrapGrant,
    ClosedReason, EnrollScope, GrantGeneration, GrantOperationId, GrantSubject, GrantSubjectError,
    HostGrantAction, HostGrantCommitFault, HostGrantError, HostGrantStore, IssuanceBasis,
    IssuancePolicy, IssuanceRefusal, ManagedRoom, MutationReceipt, OperationKind, OperationOutcome,
    RetireScope, RoomAffiliation, ScopeKey, ScopeLifecycleId, HOST_GRANT_POLICY_VERSION,
    HOST_GRANT_SCOPE_MAX_GRANT_ROWS,
};
use waddle_server::permissions::{Object, ObjectType};
use waddle_xmpp::identity::CanonicalDomain;

const CAROL: &str = "carol@example.com";

async fn admitted(fixture: &GrantFixture, account: &str, action: HostGrantAction) -> bool {
    fixture
        .store
        .candidate_admission(&fixture.scope.room, &subject(account), action)
        .await
        .expect("candidate admission")
        .admitted
}

#[test]
fn a_grant_subject_is_a_canonical_bare_account_jid() {
    let full: Jid = "carol@example.com/phone".parse().expect("full JID");
    assert_eq!(
        GrantSubject::from_jid(&full),
        Err(GrantSubjectError::FullJid)
    );
    let service: Jid = "muc.example.com".parse().expect("service JID");
    assert_eq!(
        GrantSubject::from_jid(&service),
        Err(GrantSubjectError::ServiceIdentity)
    );
    assert_eq!(
        GrantSubject::new(bare("muc.example.com")),
        Err(GrantSubjectError::ServiceIdentity)
    );
    // The parser keeps a trailing dot; the identity profile's canonical form
    // has none, and the grant key is the canonical text.
    assert_eq!(
        GrantSubject::new(bare("carol@example.com.")),
        Err(GrantSubjectError::NotCanonical)
    );
    let account: Jid = CAROL.parse().expect("account JID");
    let accepted = GrantSubject::from_jid(&account).expect("canonical bare account JID");
    assert_eq!(accepted.as_str(), CAROL);
    assert_eq!(accepted.jid(), &bare(CAROL));
}

#[test]
fn issuance_refuses_service_identities_and_untrusted_privileged_grants() {
    let policy = issuance_policy(&["trusted.example"]);
    // A room or any other identity of a local service is not an account.
    assert_eq!(
        policy.check(&subject("room@muc.example.com"), RoomAffiliation::Member),
        Err(IssuanceRefusal::LocalService)
    );
    for affiliation in RoomAffiliation::ALL {
        assert_eq!(
            policy.check(&subject(CAROL), affiliation),
            Ok(IssuanceBasis::Local),
            "{affiliation:?}"
        );
        let privileged = matches!(affiliation, RoomAffiliation::Owner | RoomAffiliation::Admin);
        assert_eq!(affiliation.confers_privilege_or_moderation(), privileged);
        assert_eq!(
            policy.check(&subject("mallory@remote.example"), affiliation),
            if privileged {
                Err(IssuanceRefusal::UntrustedHome)
            } else {
                Ok(IssuanceBasis::Unprivileged)
            },
            "{affiliation:?}"
        );
        assert_eq!(
            policy.check(&subject("admin@trusted.example"), affiliation),
            Ok(if privileged {
                IssuanceBasis::TrustedHome
            } else {
                IssuanceBasis::Unprivileged
            }),
            "{affiliation:?}"
        );
        // Trusted-home entries match exactly: a subdomain is another home.
        assert_eq!(
            policy
                .check(&subject("admin@sub.trusted.example"), affiliation)
                .is_ok(),
            !privileged,
            "{affiliation:?}"
        );
    }

    let domain = |name: &str| CanonicalDomain::parse_exact(name).expect("canonical domain");
    assert_eq!(
        IssuancePolicy::new([domain("example.com")], [], [domain("example.com")]).err(),
        Some(IssuanceRefusal::TrustedHomeIsLocal)
    );
    for home in ["localhost", "192.0.2.1"] {
        assert_eq!(
            IssuancePolicy::new([domain("example.com")], [], [domain(home)]).err(),
            Some(IssuanceRefusal::InvalidTrustedHome),
            "{home}"
        );
    }
}

#[test]
fn outcast_and_unaffiliated_subjects_hold_no_action_in_the_policy() {
    use HostGrantAction::{
        Delete, DeleteOwnMessage, EditOwnMessage, Manage, Moderate, Read, SendMessage, View,
    };
    for action in HostGrantAction::ALL {
        assert!(!affiliation_allows(RoomAffiliation::Outcast, action));
        assert!(!affiliation_allows(RoomAffiliation::None, action));
        // No explicit affiliation deletes a channel.
        assert_eq!(
            affiliation_allows(RoomAffiliation::Owner, action),
            action != Delete
        );
        assert_eq!(
            affiliation_allows(RoomAffiliation::Admin, action),
            action != Delete
        );
        assert_eq!(
            affiliation_allows(RoomAffiliation::Member, action),
            matches!(
                action,
                View | Read | SendMessage | EditOwnMessage | DeleteOwnMessage
            )
        );
    }
    assert!(!affiliation_allows(RoomAffiliation::Member, Moderate));
    assert!(!affiliation_allows(RoomAffiliation::Member, Manage));
}

#[tokio::test]
async fn enrollment_is_recorded_once_and_admits_nothing_until_activated() {
    let Some(fixture) = GrantFixture::enrolled("hg_enroll").await else {
        return;
    };
    let state = fixture.state(&fixture.scope).await;
    assert_eq!(state.generation, GrantGeneration::FIRST);
    assert_eq!(state.admission, AdmissionState::Closed);
    assert_eq!(state.closed_reason, Some(ClosedReason::Enrolled));
    assert_eq!(state.projected_generation, None);
    assert_eq!(state.policy_version, HOST_GRANT_POLICY_VERSION);
    assert_eq!(
        state.authority_incarnation,
        fixture
            .store
            .authority_incarnation()
            .await
            .expect("incarnation")
    );
    assert!(state.projection_pending());
    assert_eq!(
        fixture
            .store
            .pending_scopes()
            .await
            .expect("pending scopes"),
        vec![fixture.scope.lifecycle]
    );

    // Bootstrap grants are recorded, not effective.
    assert_eq!(
        fixture.affiliations(OWNER).await,
        Some((RoomAffiliation::Owner, RoomAffiliation::None))
    );
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    for action in HostGrantAction::ALL {
        assert!(!admitted(&fixture, OWNER, action).await, "{action:?}");
    }
    // An operation of a scope that never activated is still refused by the
    // initiator check: nobody holds effective authority yet.
    assert!(matches!(
        fixture.set(OWNER, CAROL, RoomAffiliation::Member).await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));

    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    let state = fixture.state(&fixture.scope).await;
    assert_eq!(state.admission, AdmissionState::Active);
    assert_eq!(state.closed_reason, None);
    assert_eq!(state.projected_generation, Some(GrantGeneration::FIRST));
    assert!(fixture
        .store
        .pending_scopes()
        .await
        .expect("pending scopes")
        .is_empty());
    assert_eq!(
        fixture.affiliations(OWNER).await,
        Some((RoomAffiliation::Owner, RoomAffiliation::Owner))
    );
    assert!(admitted(&fixture, SENDER, HostGrantAction::SendMessage).await);
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::AlreadyActive
    );

    // The scope key holds one lifecycle, a lifecycle identity is used once,
    // and a room is bound once.
    let enrollment = |lifecycle, node: &str, room: &str| EnrollScope {
        operation: GrantOperationId::generate(),
        lifecycle,
        key: ScopeKey::new(bare("spaces.example.com"), node).expect("scope key"),
        rooms: vec![ManagedRoom::new(bare(room)).expect("managed room")],
        bootstrap: vec![BootstrapGrant {
            room: bare(room),
            subject: subject(OWNER),
            affiliation: RoomAffiliation::Owner,
        }],
    };
    let same_key = enrollment(
        ScopeLifecycleId::generate(),
        &fixture.scope.space_node,
        "other-room@muc.example.com",
    );
    assert!(matches!(
        fixture.store.enroll_scope(&same_key, &fixture.policy).await,
        Err(HostGrantError::ScopeAlreadyEnrolled)
    ));
    let same_lifecycle = enrollment(
        fixture.scope.lifecycle,
        "another-space",
        "other-room@muc.example.com",
    );
    assert!(matches!(
        fixture
            .store
            .enroll_scope(&same_lifecycle, &fixture.policy)
            .await,
        Err(HostGrantError::ScopeAlreadyEnrolled)
    ));
    let same_room = enrollment(
        ScopeLifecycleId::generate(),
        "another-space",
        fixture.scope.room.as_str(),
    );
    assert!(matches!(
        fixture
            .store
            .enroll_scope(&same_room, &fixture.policy)
            .await,
        Err(HostGrantError::RoomAlreadyBound)
    ));
    assert!(fixture
        .store
        .scope(same_room.lifecycle)
        .await
        .expect("read scope")
        .is_none());

    // An exact retry of an enrollment returns its recorded outcome.
    let fresh = enrollment(
        ScopeLifecycleId::generate(),
        "another-space",
        "other-room@muc.example.com",
    );
    let recorded = fixture
        .store
        .enroll_scope(&fresh, &fixture.policy)
        .await
        .expect("enroll");
    let MutationReceipt::Recorded(record) = recorded else {
        panic!("a first enrollment is recorded");
    };
    assert_eq!(record.kind, OperationKind::Enroll);
    assert_eq!(
        fixture
            .store
            .enroll_scope(&fresh, &fixture.policy)
            .await
            .expect("retry"),
        MutationReceipt::Replayed(record)
    );

    // A room without an owner, or a room the enrollment does not name, is
    // refused before anything is written.
    let mut ownerless = enrollment(
        ScopeLifecycleId::generate(),
        "ownerless-space",
        "ownerless@muc.example.com",
    );
    ownerless.bootstrap[0].affiliation = RoomAffiliation::Member;
    assert!(matches!(
        fixture
            .store
            .enroll_scope(&ownerless, &fixture.policy)
            .await,
        Err(HostGrantError::LastOwner)
    ));
    let unmanaged = bare("muc.example.com");
    assert!(matches!(
        ManagedRoom::new(unmanaged),
        Err(HostGrantError::UnmanagedRoom)
    ));
    fixture.close().await;
}

#[tokio::test]
async fn a_reduction_is_effective_at_commit_and_an_increase_waits_for_activation() {
    let Some(fixture) = GrantFixture::active("hg_effective").await else {
        return;
    };
    // A new grant is recorded and not effective.
    let receipt = fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant");
    let MutationReceipt::Recorded(record) = receipt else {
        panic!("a first submission is recorded");
    };
    assert_eq!(record.generation.value(), 2);
    assert_eq!(record.kind, OperationKind::SetAffiliation);
    assert_eq!(
        fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    assert!(!admitted(&fixture, CAROL, HostGrantAction::SendMessage).await);
    let state = fixture.state(&fixture.scope).await;
    assert!(state.projection_pending());
    // The scope stays active: grants that were effective keep admitting.
    assert_eq!(state.admission, AdmissionState::Active);
    assert!(admitted(&fixture, SENDER, HostGrantAction::SendMessage).await);

    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    assert_eq!(
        fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    assert!(admitted(&fixture, CAROL, HostGrantAction::SendMessage).await);

    // A revoke is effective at its commit, without any activation.
    fixture
        .set(OWNER, PEER, RoomAffiliation::None)
        .await
        .expect("revoke");
    assert_eq!(
        fixture.affiliations(PEER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    assert!(!admitted(&fixture, PEER, HostGrantAction::Read).await);
    assert!(fixture.state(&fixture.scope).await.projection_pending());

    // So is a ban, and lifting it together with a new grant is an increase:
    // the subject stays an outcast until the grant is activated.
    fixture
        .set(OWNER, CAROL, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    assert_eq!(
        fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );
    fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant after ban");
    assert_eq!(
        fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Outcast))
    );
    assert!(!admitted(&fixture, CAROL, HostGrantAction::Read).await);

    // A promotion keeps the present standing until activation; a demotion
    // takes the lower standing at once.
    fixture
        .set(OWNER, SENDER, RoomAffiliation::Admin)
        .await
        .expect("promotion");
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Admin, RoomAffiliation::Member))
    );
    assert!(admitted(&fixture, SENDER, HostGrantAction::SendMessage).await);
    assert!(!admitted(&fixture, SENDER, HostGrantAction::Moderate).await);
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    assert!(admitted(&fixture, SENDER, HostGrantAction::Moderate).await);
    assert!(admitted(&fixture, CAROL, HostGrantAction::Read).await);
    fixture
        .set(OWNER, SENDER, RoomAffiliation::Member)
        .await
        .expect("demotion");
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    assert!(!admitted(&fixture, SENDER, HostGrantAction::Moderate).await);
    assert!(admitted(&fixture, SENDER, HostGrantAction::SendMessage).await);
    fixture.close().await;
}

#[tokio::test]
async fn an_outcast_is_denied_every_action_the_policy_protects() {
    let Some(fixture) = GrantFixture::active("hg_outcast_actions").await else {
        return;
    };
    // An admin holds every action but channel deletion.
    fixture
        .set(OWNER, CAROL, RoomAffiliation::Admin)
        .await
        .expect("admin");
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    for action in HostGrantAction::ALL {
        assert_eq!(
            admitted(&fixture, CAROL, action).await,
            action != HostGrantAction::Delete,
            "{action:?}"
        );
    }
    // Banned, the former admin holds none, including moderation, management
    // and the author's own correction and retraction.
    fixture
        .set(OWNER, CAROL, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    for action in HostGrantAction::ALL {
        assert!(!admitted(&fixture, CAROL, action).await, "{action:?}");
    }
    // A recorded ban wins even over an effective standing that another
    // writer left behind: admission refuses a recorded outcast.
    fixture
        .ingress
        .execute(
            "UPDATE host_grants SET effective_affiliation = 'owner' WHERE subject_jid = ?",
            waddle_server::db_params![CAROL],
        )
        .await;
    for action in HostGrantAction::ALL {
        assert!(!admitted(&fixture, CAROL, action).await, "{action:?}");
    }
    fixture.close().await;
}

#[tokio::test]
async fn an_exact_retry_returns_the_recorded_outcome_and_changed_input_conflicts() {
    let Some(fixture) = GrantFixture::active("hg_retry").await else {
        return;
    };
    let command = fixture.command(OWNER, CAROL, RoomAffiliation::Member).await;
    let MutationReceipt::Recorded(record) = fixture
        .store
        .set_affiliation(&command, &fixture.policy)
        .await
        .expect("grant")
    else {
        panic!("a first submission is recorded");
    };
    assert_eq!(
        fixture
            .store
            .operation_outcome(command.operation)
            .await
            .expect("readback"),
        OperationOutcome::Committed(record.clone())
    );
    assert_eq!(
        fixture
            .store
            .operation_outcome(GrantOperationId::generate())
            .await
            .expect("readback"),
        OperationOutcome::Absent
    );

    // The exact retry is the original outcome and advances nothing.
    assert_eq!(
        fixture
            .store
            .set_affiliation(&command, &fixture.policy)
            .await
            .expect("retry"),
        MutationReceipt::Replayed(record.clone())
    );
    assert_eq!(fixture.generation().await, record.generation);

    // The identity with any other input is a conflict and changes nothing.
    let mut other_affiliation = command.clone();
    other_affiliation.affiliation = RoomAffiliation::Admin;
    let mut other_subject = command.clone();
    other_subject.subject = subject("dave@example.com");
    let mut other_generation = command.clone();
    other_generation.expected_generation = record.generation;
    let mut other_initiator = command.clone();
    other_initiator.initiator = subject(SENDER);
    for changed in [
        other_affiliation,
        other_subject,
        other_generation,
        other_initiator,
    ] {
        assert!(matches!(
            fixture
                .store
                .set_affiliation(&changed, &fixture.policy)
                .await,
            Err(HostGrantError::OperationConflict)
        ));
    }
    assert_eq!(fixture.generation().await, record.generation);
    assert_eq!(
        fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    assert_eq!(fixture.affiliations("dave@example.com").await, None);

    // After a later revoke the retry is still only the historical outcome:
    // it does not grant again.
    fixture
        .set(OWNER, CAROL, RoomAffiliation::None)
        .await
        .expect("revoke");
    let after_revoke = fixture.generation().await;
    assert_eq!(
        fixture
            .store
            .set_affiliation(&command, &fixture.policy)
            .await
            .expect("retry after revoke"),
        MutationReceipt::Replayed(record)
    );
    assert_eq!(fixture.generation().await, after_revoke);
    assert_eq!(
        fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    fixture.close().await;
}

#[tokio::test]
async fn the_initiator_is_revalidated_under_the_scope_guard() {
    let Some(fixture) = GrantFixture::active("hg_initiator").await else {
        return;
    };
    // An admin whose standing is recorded but not yet effective has none.
    fixture
        .set(OWNER, SENDER, RoomAffiliation::Admin)
        .await
        .expect("promotion");
    assert!(matches!(
        fixture.set(SENDER, CAROL, RoomAffiliation::Member).await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );

    // The admin prepares a grant; the owner revokes the admin before the
    // admin submits it.
    let prepared = fixture
        .command(SENDER, CAROL, RoomAffiliation::Member)
        .await;
    fixture
        .set(OWNER, SENDER, RoomAffiliation::None)
        .await
        .expect("revoke the admin");
    let current = fixture.generation().await;
    assert!(matches!(
        fixture.store.set_affiliation(&prepared, &fixture.policy).await,
        Err(HostGrantError::StaleGeneration { current: reported }) if reported == current
    ));
    // Deciding again at the new generation does not help: the authority the
    // command relied on is gone.
    assert!(matches!(
        fixture.set(SENDER, CAROL, RoomAffiliation::Member).await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));
    assert_eq!(fixture.affiliations(CAROL).await, None);
    assert_eq!(fixture.generation().await, current);

    // Only an owner changes owner and admin standing, an outcast or a plain
    // member changes nothing, and the room keeps an owner.
    fixture
        .set(OWNER, PEER, RoomAffiliation::Admin)
        .await
        .expect("admin");
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    fixture
        .set(PEER, CAROL, RoomAffiliation::Member)
        .await
        .expect("an admin grants membership");
    for (initiator, target, affiliation) in [
        (PEER, CAROL, RoomAffiliation::Admin),
        (PEER, CAROL, RoomAffiliation::Owner),
        (PEER, OWNER, RoomAffiliation::Member),
        (PEER, PEER, RoomAffiliation::Owner),
        (CAROL, "dave@example.com", RoomAffiliation::Member),
        ("stranger@example.com", CAROL, RoomAffiliation::None),
    ] {
        assert!(
            matches!(
                fixture.set(initiator, target, affiliation).await,
                Err(HostGrantError::InitiatorNotAuthorized)
            ),
            "{initiator} -> {target} {affiliation:?}"
        );
    }
    fixture
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban the admin");
    assert!(matches!(
        fixture.set(PEER, CAROL, RoomAffiliation::None).await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));
    assert!(matches!(
        fixture.set(OWNER, OWNER, RoomAffiliation::Member).await,
        Err(HostGrantError::LastOwner)
    ));
    assert!(matches!(
        fixture
            .store
            .set_affiliation(
                &{
                    let mut command = fixture.command(OWNER, CAROL, RoomAffiliation::None).await;
                    command.room = bare("elsewhere@muc.example.com");
                    command
                },
                &fixture.policy
            )
            .await,
        Err(HostGrantError::UnknownRoom)
    ));
    fixture.close().await;
}

#[tokio::test]
async fn a_privileged_grant_to_a_remote_account_needs_a_trusted_home() {
    let Some(fixture) = GrantFixture::active("hg_trusted_home").await else {
        return;
    };
    let untrusting = issuance_policy(&[]);
    let trusting = issuance_policy(&["trusted.example"]);
    let remote_admin = "admin@trusted.example";
    let generation = fixture.generation().await;

    // Without the home in the supplied set, owner and admin are refused and
    // nothing is recorded.
    for affiliation in [RoomAffiliation::Owner, RoomAffiliation::Admin] {
        let command = fixture.command(OWNER, remote_admin, affiliation).await;
        assert!(matches!(
            fixture.store.set_affiliation(&command, &untrusting).await,
            Err(HostGrantError::Issuance(IssuanceRefusal::UntrustedHome))
        ));
        assert_eq!(
            fixture
                .store
                .operation_outcome(command.operation)
                .await
                .expect("readback"),
            OperationOutcome::Absent
        );
    }
    assert_eq!(fixture.generation().await, generation);
    assert_eq!(fixture.affiliations(remote_admin).await, None);

    // A member-level grant to any remote account is issued.
    let member = fixture
        .command(OWNER, "guest@remote.example", RoomAffiliation::Member)
        .await;
    fixture
        .store
        .set_affiliation(&member, &untrusting)
        .await
        .expect("member-level grant");
    let basis = |account: &'static str| {
        let store = fixture.store.clone();
        let lifecycle = fixture.scope.lifecycle;
        async move {
            store
                .grants(lifecycle)
                .await
                .expect("grant rows")
                .into_iter()
                .find(|grant| grant.subject.as_str() == account)
                .map(|grant| grant.issuance_basis)
        }
    };
    assert_eq!(
        basis("guest@remote.example").await,
        Some(IssuanceBasis::Unprivileged)
    );

    // With the home in the supplied set, the privileged grant is recorded
    // with its basis; another home is still refused.
    let admin = fixture
        .command(OWNER, remote_admin, RoomAffiliation::Admin)
        .await;
    let receipt = fixture
        .store
        .set_affiliation(&admin, &trusting)
        .await
        .expect("trusted-home grant");
    assert!(matches!(receipt, MutationReceipt::Recorded(_)));
    assert_eq!(
        fixture.affiliations(remote_admin).await,
        Some((RoomAffiliation::Admin, RoomAffiliation::None))
    );
    assert_eq!(basis(remote_admin).await, Some(IssuanceBasis::TrustedHome));
    let other_home = fixture
        .command(OWNER, "admin@remote.example", RoomAffiliation::Admin)
        .await;
    assert!(matches!(
        fixture.store.set_affiliation(&other_home, &trusting).await,
        Err(HostGrantError::Issuance(IssuanceRefusal::UntrustedHome))
    ));

    // The set is an input of each issuance: without the home, a further
    // privileged grant is refused, and a demotion or a revoke is not.
    let owner = fixture
        .command(OWNER, remote_admin, RoomAffiliation::Owner)
        .await;
    assert!(matches!(
        fixture.store.set_affiliation(&owner, &untrusting).await,
        Err(HostGrantError::Issuance(IssuanceRefusal::UntrustedHome))
    ));
    let demotion = fixture
        .command(OWNER, remote_admin, RoomAffiliation::Member)
        .await;
    fixture
        .store
        .set_affiliation(&demotion, &untrusting)
        .await
        .expect("demotion");
    assert_eq!(basis(remote_admin).await, Some(IssuanceBasis::Unprivileged));

    // A local service identity is refused as a subject.
    let room_as_subject = fixture
        .command(OWNER, "room@muc.example.com", RoomAffiliation::Member)
        .await;
    assert!(matches!(
        fixture
            .store
            .set_affiliation(&room_as_subject, &trusting)
            .await,
        Err(HostGrantError::Issuance(IssuanceRefusal::LocalService))
    ));

    // Bootstrap grants pass the same rule.
    let bootstrap = |lifecycle| {
        let room = bare(&format!(
            "remote-owned-{}@muc.example.com",
            Uuid::new_v4().simple()
        ));
        EnrollScope {
            operation: GrantOperationId::generate(),
            lifecycle,
            key: ScopeKey::new(
                bare("spaces.example.com"),
                format!("space-{}", Uuid::new_v4().simple()),
            )
            .expect("scope key"),
            rooms: vec![ManagedRoom::new(room.clone()).expect("managed room")],
            bootstrap: vec![BootstrapGrant {
                room,
                subject: subject("owner@trusted.example"),
                affiliation: RoomAffiliation::Owner,
            }],
        }
    };
    let refused = bootstrap(ScopeLifecycleId::generate());
    assert!(matches!(
        fixture.store.enroll_scope(&refused, &untrusting).await,
        Err(HostGrantError::Issuance(IssuanceRefusal::UntrustedHome))
    ));
    assert!(fixture
        .store
        .scope(refused.lifecycle)
        .await
        .expect("read scope")
        .is_none());
    let accepted = bootstrap(ScopeLifecycleId::generate());
    fixture
        .store
        .enroll_scope(&accepted, &trusting)
        .await
        .expect("trusted-home bootstrap");
    assert!(fixture
        .store
        .scope(accepted.lifecycle)
        .await
        .expect("read scope")
        .is_some());
    fixture.close().await;
}

#[tokio::test]
async fn generation_exhaustion_closes_the_scope_and_no_identity_is_reused() {
    let Some(fixture) = GrantFixture::active("hg_exhaustion").await else {
        return;
    };
    let stale = fixture.command(OWNER, PEER, RoomAffiliation::None).await;
    // Bring the scope to its last working generation.
    let last = i64::MAX - 1;
    fixture
        .ingress
        .execute(
            "UPDATE host_grant_scopes SET generation = ?, projected_generation = ? \
             WHERE lifecycle_id = ?",
            waddle_server::db_params![last, last, fixture.scope.lifecycle.to_storage()],
        )
        .await;
    assert_eq!(fixture.generation().await.value(), last);
    assert!(GrantGeneration::new(last)
        .expect("generation")
        .next()
        .is_none());

    // A command decided at an earlier generation fails as stale.
    assert!(matches!(
        fixture.store.set_affiliation(&stale, &fixture.policy).await,
        Err(HostGrantError::StaleGeneration { current }) if current.value() == last
    ));

    // The next operation finds no working generation: the scope is closed at
    // the terminal generation and the change is not made.
    let exhausting = fixture.command(OWNER, PEER, RoomAffiliation::None).await;
    assert!(matches!(
        fixture
            .store
            .set_affiliation(&exhausting, &fixture.policy)
            .await,
        Err(HostGrantError::ScopeExhausted)
    ));
    let state = fixture.state(&fixture.scope).await;
    assert_eq!(state.generation, GrantGeneration::TERMINAL);
    assert_eq!(state.admission, AdmissionState::Closed);
    assert_eq!(state.closed_reason, Some(ClosedReason::Exhausted));
    assert_eq!(state.operation, exhausting.operation);
    assert_eq!(
        fixture.affiliations(PEER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    // Closed means closed for everyone, including the grants left in place.
    for account in [OWNER, SENDER, PEER] {
        for action in HostGrantAction::ALL {
            assert!(!admitted(&fixture, account, action).await, "{account}");
        }
    }
    let OperationOutcome::Committed(record) = fixture
        .store
        .operation_outcome(exhausting.operation)
        .await
        .expect("readback")
    else {
        panic!("the exhausting operation is recorded");
    };
    assert_eq!(record.kind, OperationKind::Exhaust);
    assert_eq!(record.generation, GrantGeneration::TERMINAL);

    // Its exact retry repeats the outcome; nothing reopens the scope.
    assert!(matches!(
        fixture
            .store
            .set_affiliation(&exhausting, &fixture.policy)
            .await,
        Err(HostGrantError::ScopeExhausted)
    ));
    assert!(matches!(
        fixture.set(OWNER, CAROL, RoomAffiliation::Member).await,
        Err(HostGrantError::ScopeExhausted)
    ));
    assert!(matches!(
        fixture.store.projection_plan(fixture.scope.lifecycle).await,
        Err(HostGrantError::ScopeExhausted)
    ));
    assert_eq!(state, fixture.state(&fixture.scope).await);

    // A retired scope stays closed as well, and neither its key nor its
    // lifecycle identity is enrolled again.
    let other = fixture.enroll_another().await;
    assert_eq!(
        fixture.activate_current(&other).await,
        Activation::Activated
    );
    let retire = RetireScope {
        operation: GrantOperationId::generate(),
        lifecycle: other.lifecycle,
        expected_generation: GrantGeneration::FIRST,
    };
    let MutationReceipt::Recorded(retired) =
        fixture.store.retire_scope(&retire).await.expect("retire")
    else {
        panic!("a first retirement is recorded");
    };
    assert_eq!(retired.kind, OperationKind::Retire);
    assert_eq!(
        fixture.store.retire_scope(&retire).await.expect("retry"),
        MutationReceipt::Replayed(retired)
    );
    let state = fixture.state(&other).await;
    assert_eq!(state.admission, AdmissionState::Closed);
    assert_eq!(state.closed_reason, Some(ClosedReason::Retired));
    assert!(
        !fixture
            .store
            .candidate_admission(&other.room, &subject(SENDER), HostGrantAction::Read)
            .await
            .expect("candidate admission")
            .admitted
    );
    assert!(matches!(
        fixture
            .store
            .retire_scope(&RetireScope {
                operation: GrantOperationId::generate(),
                lifecycle: other.lifecycle,
                expected_generation: state.generation,
            })
            .await,
        Err(HostGrantError::ScopeRetired)
    ));
    let marker = fixture
        .store
        .recorded_marker(other.lifecycle, GrantGeneration::FIRST)
        .await
        .expect("journal")
        .expect("enrollment marker");
    assert!(matches!(
        fixture.store.activate(&marker).await,
        Err(HostGrantError::ScopeRetired)
    ));
    for (lifecycle, node) in [
        (ScopeLifecycleId::generate(), other.space_node.as_str()),
        (other.lifecycle, "a-new-space"),
        (fixture.scope.lifecycle, "another-new-space"),
        (
            ScopeLifecycleId::generate(),
            fixture.scope.space_node.as_str(),
        ),
    ] {
        let room = bare(&format!(
            "reuse-{}@muc.example.com",
            Uuid::new_v4().simple()
        ));
        let reuse = EnrollScope {
            operation: GrantOperationId::generate(),
            lifecycle,
            key: ScopeKey::new(bare("spaces.example.com"), node).expect("scope key"),
            rooms: vec![ManagedRoom::new(room.clone()).expect("managed room")],
            bootstrap: vec![BootstrapGrant {
                room,
                subject: subject(OWNER),
                affiliation: RoomAffiliation::Owner,
            }],
        };
        assert!(matches!(
            fixture.store.enroll_scope(&reuse, &fixture.policy).await,
            Err(HostGrantError::ScopeAlreadyEnrolled)
        ));
    }
    // A retired scope's objects stay closed to legacy permission writes.
    for object in [
        Object::new(ObjectType::Channel, other.channel_id.as_str()),
        Object::new(ObjectType::Space, other.space_node.as_str()),
    ] {
        assert!(fixture
            .store
            .enrolled_object(&object)
            .await
            .expect("enrolled object"));
    }
    assert!(!fixture
        .store
        .enrolled_object(&Object::new(ObjectType::Channel, "unmanaged-channel"))
        .await
        .expect("enrolled object"));
    fixture.close().await;
}

#[tokio::test]
async fn a_scope_holds_a_bounded_number_of_grant_rows() {
    let Some(fixture) = GrantFixture::active("hg_grant_limit").await else {
        return;
    };
    // The fixture starts with three rows; fill the scope to its bound with
    // revoked subjects, which count because a revoke is a row.
    for index in 3..HOST_GRANT_SCOPE_MAX_GRANT_ROWS {
        fixture
            .ingress
            .execute(
                "INSERT INTO host_grants (lifecycle_id, room_jid, subject_jid, affiliation, \
                 effective_affiliation, changed_generation, operation_id, issuance_basis) \
                 VALUES (?, ?, ?, 'none', 'none', 1, ?, 'local')",
                waddle_server::db_params![
                    fixture.scope.lifecycle.to_storage(),
                    fixture.scope.room.as_str(),
                    format!("filler-{index}@example.com"),
                    GrantOperationId::generate().to_storage()
                ],
            )
            .await;
    }
    assert!(matches!(
        fixture.set(OWNER, CAROL, RoomAffiliation::Member).await,
        Err(HostGrantError::GrantLimit)
    ));
    // A change to a subject that already holds a row is still recorded, so a
    // full scope can always revoke and ban.
    fixture
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban in a full scope");
    fixture
        .set(OWNER, "filler-3@example.com", RoomAffiliation::Outcast)
        .await
        .expect("ban of a revoked subject in a full scope");
    fixture.close().await;
}

#[tokio::test]
async fn an_unacknowledged_commit_is_resolved_by_exact_readback() {
    let Some(fixture) = GrantFixture::active("hg_commit_unknown").await else {
        return;
    };
    // The commit happened and its reply was lost: the caller learns nothing,
    // the journal holds the operation, and the grant is recorded but not
    // effective. Retrying returns the recorded outcome.
    let grant = fixture.command(OWNER, CAROL, RoomAffiliation::Member).await;
    assert!(matches!(
        with_host_grant_commit_fault(
            HostGrantCommitFault::ReplyLostAfterCommit,
            fixture.store.set_affiliation(&grant, &fixture.policy)
        )
        .await,
        Err(HostGrantError::CommitOutcomeUnknown)
    ));
    let OperationOutcome::Committed(record) = fixture
        .store
        .operation_outcome(grant.operation)
        .await
        .expect("readback")
    else {
        panic!("the operation committed");
    };
    assert_eq!(record.generation.value(), 2);
    assert_eq!(
        fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    assert!(!admitted(&fixture, CAROL, HostGrantAction::Read).await);
    assert_eq!(
        fixture
            .store
            .set_affiliation(&grant, &fixture.policy)
            .await
            .expect("retry"),
        MutationReceipt::Replayed(record)
    );
    assert_eq!(fixture.generation().await.value(), 2);

    // A revoke whose reply was lost is effective all the same: its
    // linearization point is the commit, not the acknowledgement.
    let revoke = fixture.command(OWNER, PEER, RoomAffiliation::None).await;
    assert!(matches!(
        with_host_grant_commit_fault(
            HostGrantCommitFault::ReplyLostAfterCommit,
            fixture.store.set_affiliation(&revoke, &fixture.policy)
        )
        .await,
        Err(HostGrantError::CommitOutcomeUnknown)
    ));
    assert!(!admitted(&fixture, PEER, HostGrantAction::Read).await);
    assert!(matches!(
        fixture
            .store
            .operation_outcome(revoke.operation)
            .await
            .expect("readback"),
        OperationOutcome::Committed(_)
    ));

    // The transaction ended without a commit: the same error for the
    // caller, and the readback says the operation is absent. Nothing
    // changed, and the retry records it.
    let lost = fixture.command(OWNER, SENDER, RoomAffiliation::None).await;
    let before = fixture.generation().await;
    assert!(matches!(
        with_host_grant_commit_fault(
            HostGrantCommitFault::LostBeforeCommit,
            fixture.store.set_affiliation(&lost, &fixture.policy)
        )
        .await,
        Err(HostGrantError::CommitOutcomeUnknown)
    ));
    assert_eq!(
        fixture
            .store
            .operation_outcome(lost.operation)
            .await
            .expect("readback"),
        OperationOutcome::Absent
    );
    assert_eq!(fixture.generation().await, before);
    assert!(admitted(&fixture, SENDER, HostGrantAction::SendMessage).await);
    assert!(matches!(
        fixture
            .store
            .set_affiliation(&lost, &fixture.policy)
            .await
            .expect("retry"),
        MutationReceipt::Recorded(_)
    ));
    assert!(!admitted(&fixture, SENDER, HostGrantAction::SendMessage).await);
    fixture.close().await;
}

#[tokio::test]
async fn a_cancelled_caller_does_not_abandon_the_transaction() {
    let Some(fixture) = GrantFixture::active("hg_cancelled_caller").await else {
        return;
    };
    let revoke = fixture.command(OWNER, PEER, RoomAffiliation::None).await;
    // The caller stops waiting on its first poll. The transaction runs on a
    // task of its own and resolves; the operation is found by its identity.
    assert!(tokio::time::timeout(
        Duration::ZERO,
        fixture.store.set_affiliation(&revoke, &fixture.policy)
    )
    .await
    .is_err());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match fixture
            .store
            .operation_outcome(revoke.operation)
            .await
            .expect("readback")
        {
            OperationOutcome::Committed(record) => {
                assert_eq!(record.generation.value(), 2);
                break;
            }
            OperationOutcome::Absent => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the abandoned operation never resolved"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    assert!(!admitted(&fixture, PEER, HostGrantAction::Read).await);
    fixture.close().await;
}

/// Run `sql` as the role of `pool` and return the SQLSTATE it fails with.
async fn refused(pool: &sqlx::PgPool, sql: &str) -> Option<String> {
    match sqlx::query(sql).execute(pool).await {
        Ok(_) => None,
        Err(sqlx::Error::Database(error)) => error.code().map(|code| code.into_owned()),
        Err(other) => panic!("{sql}: {other}"),
    }
}

#[tokio::test]
async fn the_runtime_role_holds_exactly_the_store_privileges() {
    let Some(fixture) = GrantFixture::active("hg_runtime_role").await else {
        return;
    };
    let owner_url = fixture.ingress.db.database_url().to_owned();
    let admin = sqlx::PgPool::connect(&owner_url)
        .await
        .expect("owner connection");
    let role = format!("hg_runtime_{}", Uuid::new_v4().simple());
    let password = Uuid::new_v4().simple().to_string();
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&admin)
        .await
        .expect("database name");
    // The owner phase's grant matrix for the host-grant relations, and what
    // a guarded store transaction needs besides: the protocol epoch row.
    for statement in [
        format!(
            "CREATE ROLE {role} LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB \
             NOCREATEROLE NOREPLICATION NOBYPASSRLS"
        ),
        format!("REVOKE ALL ON DATABASE {database} FROM PUBLIC"),
        format!("GRANT CONNECT ON DATABASE {database} TO {role}"),
        format!("GRANT USAGE ON SCHEMA public TO {role}"),
        format!(
            "GRANT SELECT ON host_grant_authority, host_grant_scope_lifecycles, \
             host_grant_rooms TO {role}"
        ),
        format!("GRANT SELECT, UPDATE ON host_grant_scopes TO {role}"),
        format!("GRANT SELECT, INSERT, UPDATE ON host_grants TO {role}"),
        format!(
            "GRANT SELECT, INSERT ON host_grant_operations, host_grant_operation_changes \
             TO {role}"
        ),
        format!("GRANT SELECT ON ingress_protocol_epoch TO {role}"),
        format!("GRANT UPDATE (id) ON ingress_protocol_epoch TO {role}"),
    ] {
        sqlx::query(&statement)
            .execute(&admin)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    let mut runtime_url = url::Url::parse(&owner_url).expect("database URL");
    runtime_url.set_username(&role).expect("runtime user");
    runtime_url
        .set_password(Some(&password))
        .expect("runtime password");
    let runtime_db = Database::from_config(
        "host-grant-runtime",
        &DatabaseConfig::new(DatabaseDriver::Postgres, runtime_url.to_string()),
    )
    .await
    .expect("runtime database");
    let runtime = HostGrantStore::open(runtime_db.clone()).expect("runtime store");
    let raw = sqlx::PgPool::connect(runtime_url.as_str())
        .await
        .expect("runtime connection");

    // The role matches the profile; the owner's own login does not.
    runtime
        .verify_runtime_privileges()
        .await
        .expect("restricted runtime role");
    assert!(matches!(
        fixture.store.verify_runtime_privileges().await,
        Err(HostGrantError::UnsafeRuntimePrivileges {
            component: "runtime role"
        })
    ));

    // It records operations, activates them and reads the policy state.
    let grant = fixture.command(OWNER, CAROL, RoomAffiliation::Member).await;
    assert!(matches!(
        runtime
            .set_affiliation(&grant, &fixture.policy)
            .await
            .expect("runtime mutation"),
        MutationReceipt::Recorded(_)
    ));
    let plan = runtime
        .projection_plan(fixture.scope.lifecycle)
        .await
        .expect("runtime plan");
    assert_eq!(
        runtime.activate(plan.marker()).await.expect("activation"),
        Activation::Activated
    );
    assert!(
        runtime
            .candidate_admission(
                &fixture.scope.room,
                &subject(CAROL),
                HostGrantAction::SendMessage
            )
            .await
            .expect("runtime read")
            .admitted
    );

    // It cannot enroll a scope or bind a room.
    let room = bare("runtime-enrolled@muc.example.com");
    let enrollment = EnrollScope {
        operation: GrantOperationId::generate(),
        lifecycle: ScopeLifecycleId::generate(),
        key: ScopeKey::new(bare("spaces.example.com"), "runtime-enrolled").expect("scope key"),
        rooms: vec![ManagedRoom::new(room.clone()).expect("managed room")],
        bootstrap: vec![BootstrapGrant {
            room,
            subject: subject(OWNER),
            affiliation: RoomAffiliation::Owner,
        }],
    };
    assert!(matches!(
        runtime.enroll_scope(&enrollment, &fixture.policy).await,
        Err(HostGrantError::Database(_))
    ));
    assert!(fixture
        .store
        .scope(enrollment.lifecycle)
        .await
        .expect("read scope")
        .is_none());

    // It cannot delete or truncate any host-grant row, rewrite the journal,
    // change a room binding, or change the authority incarnation.
    let lifecycle = fixture.scope.lifecycle.to_storage();
    for statement in [
        "DELETE FROM host_grant_operations".to_owned(),
        "UPDATE host_grant_operations SET fingerprint = repeat('0', 64)".to_owned(),
        "DELETE FROM host_grant_operation_changes".to_owned(),
        "UPDATE host_grant_operation_changes SET affiliation = 'owner'".to_owned(),
        "DELETE FROM host_grants".to_owned(),
        "DELETE FROM host_grant_scopes".to_owned(),
        "TRUNCATE host_grant_scopes".to_owned(),
        "TRUNCATE host_grants".to_owned(),
        "DELETE FROM host_grant_scope_lifecycles".to_owned(),
        "UPDATE host_grant_scope_lifecycles SET space_node = 'renamed'".to_owned(),
        format!(
            "INSERT INTO host_grant_scope_lifecycles (lifecycle_id, service_jid, space_node, \
             enrolled_at_ms) VALUES ('{}', 'spaces.example.com', 'forged', 0)",
            ScopeLifecycleId::generate().to_storage()
        ),
        format!(
            "INSERT INTO host_grant_rooms (room_jid, channel_id, lifecycle_id, access_model, \
             bound_at_ms) VALUES ('forged@muc.example.com', 'forged', '{lifecycle}', \
             'members_only', 0)"
        ),
        "UPDATE host_grant_rooms SET lifecycle_id = lifecycle_id".to_owned(),
        "DELETE FROM host_grant_rooms".to_owned(),
        "UPDATE host_grant_authority SET incarnation = repeat('0', 32)".to_owned(),
        "DELETE FROM host_grant_authority".to_owned(),
    ] {
        assert_eq!(
            refused(&raw, &statement).await.as_deref(),
            Some("42501"),
            "{statement}"
        );
    }

    // A privilege beyond the profile is found, whoever granted it.
    sqlx::query(&format!("GRANT DELETE ON host_grants TO {role}"))
        .execute(&admin)
        .await
        .expect("excess grant");
    assert!(matches!(
        runtime.verify_runtime_privileges().await,
        Err(HostGrantError::UnsafeRuntimePrivileges {
            component: "host_grants"
        })
    ));

    raw.close().await;
    drop(runtime);
    drop(runtime_db);
    fixture.close().await;
    // The role is cluster-wide; its database is gone.
    let cluster_url = waddle_ws_test_support::require_test_postgres_url().expect("cluster URL");
    let cluster = sqlx::PgPool::connect(&cluster_url)
        .await
        .expect("cluster connection");
    sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
        .execute(&cluster)
        .await
        .expect("drop the runtime role");
    cluster.close().await;
    admin.close().await;
}
