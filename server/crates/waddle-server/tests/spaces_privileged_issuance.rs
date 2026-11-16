//! Issuance of the privileged relations no writer names (ADR-0020 D8).
//!
//! The channel `manager` and `moderator`, the Space `moderator` and the
//! role `manager` relations confer management permissions or room
//! moderation, and no handler, command or repair path writes them by name:
//! they reach the store only through the permission actor's write messages.
//! An actor that decides issuance writes them for an account of a served
//! domain or of a trusted home and refuses them for any other account, in
//! each of the three messages that write a relation; every other relation,
//! and a subject that is no account, is written as before.
//!
//! The suites' actors over the local backend establish the refusal at the
//! write, not SpiceDB behaviour.
#![cfg(feature = "test-support")]

use std::sync::Arc;

use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::permissions::host_grant::IssuancePolicy;
use waddle_server::permissions::{
    ExclusiveRelationSwap, ListSubjects, Object, ObjectType, PermissionActor, PermissionError,
    Relation, ReplaceExclusiveRelation, Subject, SubjectType, SwapExclusiveRelation, Tuple,
    WriteTuple,
};
use waddle_xmpp::identity::CanonicalDomain;

const LOCAL: &str = "alice@example.com";
const TRUSTED: &str = "carol@home-a.test";
const UNTRUSTED: &str = "bob@home-b.test";

fn domain(name: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(name).expect("a canonical domain")
}

/// `example.com` serving its accounts and its MUC and Spaces services, with
/// `home-a.test` as its only trusted home.
fn policy() -> IssuancePolicy {
    IssuancePolicy::new(
        [domain("example.com")],
        [domain("muc.example.com"), domain("spaces.example.com")],
        [domain("home-a.test")],
    )
    .expect("a valid issuance policy")
}

async fn database(name: &str) -> Arc<Database> {
    let db = Database::in_memory(name).await.expect("in-memory database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("deployment migrations");
    Arc::new(db)
}

/// An actor that decides issuance as a serving replica does.
async fn deciding(name: &str) -> ActorRef<PermissionActor> {
    PermissionActor::spawn(
        PermissionActor::new_for_tests(database(name).await).with_privileged_issuance(policy()),
    )
}

/// The four relations and the object each is a relation of.
fn privileged() -> [(ObjectType, &'static str); 4] {
    [
        (ObjectType::Channel, "manager"),
        (ObjectType::Channel, "moderator"),
        (ObjectType::Space, "moderator"),
        (ObjectType::Role, "manager"),
    ]
}

fn tuple(object: ObjectType, relation: &str, account: &str) -> Tuple {
    Tuple::new(
        Object::new(object, "lounge"),
        Relation::new(relation),
        Subject::user(account),
    )
}

async fn write(actor: &ActorRef<PermissionActor>, tuple: Tuple) -> Result<(), PermissionError> {
    match actor.ask(WriteTuple { tuple }).await {
        Ok(()) => Ok(()),
        Err(SendError::HandlerError(error)) => Err(error),
        Err(error) => panic!("the permission actor did not answer: {error}"),
    }
}

async fn holders(
    actor: &ActorRef<PermissionActor>,
    object: ObjectType,
    relation: &str,
) -> Vec<String> {
    actor
        .ask(ListSubjects {
            object: Object::new(object, "lounge"),
            relation: Relation::new(relation),
        })
        .await
        .expect("list subjects")
        .into_iter()
        .map(|subject| subject.id)
        .collect()
}

#[tokio::test]
async fn a_privileged_relation_is_written_for_a_served_account_and_a_trusted_home() {
    let actor = deciding("spaces-issuance-admitted").await;
    for (object, relation) in privileged() {
        write(&actor, tuple(object, relation, LOCAL))
            .await
            .unwrap_or_else(|error| panic!("{relation} for a served account: {error}"));
        write(&actor, tuple(object, relation, TRUSTED))
            .await
            .unwrap_or_else(|error| panic!("{relation} for a trusted home: {error}"));
        let mut held = holders(&actor, object, relation).await;
        held.sort();
        assert_eq!(held, [LOCAL, TRUSTED], "{relation}");
    }
}

#[tokio::test]
async fn a_privileged_relation_is_refused_for_any_other_home() {
    let actor = deciding("spaces-issuance-refused").await;
    for (object, relation) in privileged() {
        let refused = write(&actor, tuple(object, relation, UNTRUSTED)).await;
        assert!(
            matches!(refused, Err(PermissionError::UntrustedPrivilegedSubject)),
            "{relation}: {refused:?}"
        );
        assert!(
            holders(&actor, object, relation).await.is_empty(),
            "{relation}"
        );
    }
}

/// An address of one of the deployment's own services, a subject that
/// names no account and a domain-only address hold no privileged relation.
#[tokio::test]
async fn a_subject_that_is_no_account_of_an_admitted_home_is_refused() {
    let actor = deciding("spaces-issuance-no-account").await;
    for subject in [
        "lounge@muc.example.com",
        "general@spaces.example.com",
        "example.com",
        "home-a.test",
        "not a jid@",
    ] {
        let refused = write(&actor, tuple(ObjectType::Channel, "moderator", subject)).await;
        assert!(
            matches!(refused, Err(PermissionError::UntrustedPrivilegedSubject)),
            "{subject}: {refused:?}"
        );
    }
    assert!(holders(&actor, ObjectType::Channel, "moderator")
        .await
        .is_empty());
}

/// Member-level relations, and the relations whose writers decide issuance
/// themselves, are not this boundary's to refuse.
#[tokio::test]
async fn other_relations_are_written_for_any_account() {
    let actor = deciding("spaces-issuance-other-relations").await;
    for (object, relation) in [
        (ObjectType::Channel, "member"),
        (ObjectType::Channel, "writer"),
        (ObjectType::Channel, "viewer"),
        (ObjectType::Channel, "outcast"),
        (ObjectType::Space, "member"),
        (ObjectType::Role, "assignee"),
    ] {
        write(&actor, tuple(object, relation, UNTRUSTED))
            .await
            .unwrap_or_else(|error| panic!("{relation}: {error}"));
        assert_eq!(holders(&actor, object, relation).await, [UNTRUSTED]);
    }
}

/// A userset subject names no principal: a channel's parent Space is
/// written whatever the relation's name.
#[tokio::test]
async fn a_userset_subject_is_not_asked_about() {
    let actor = deciding("spaces-issuance-userset").await;
    let parent = Tuple::new(
        Object::new(ObjectType::Channel, "lounge"),
        Relation::new("parent"),
        Subject::userset(SubjectType::Space, "general", ""),
    );
    write(&actor, parent).await.expect("the parent relation");
}

#[tokio::test]
async fn an_exclusive_replacement_is_refused_for_an_untrusted_home() {
    let actor = deciding("spaces-issuance-replace").await;
    let family = || vec![Relation::new("member"), Relation::new("moderator")];
    let replace = |account: &str, replacement: &str| ReplaceExclusiveRelation {
        object: Object::new(ObjectType::Channel, "lounge"),
        subject: Subject::user(account),
        family: family(),
        replacement: Some(Relation::new(replacement)),
    };

    let refused = actor.ask(replace(UNTRUSTED, "moderator")).await;
    assert!(
        matches!(
            refused,
            Err(SendError::HandlerError(
                PermissionError::UntrustedPrivilegedSubject
            ))
        ),
        "{refused:?}"
    );
    assert!(holders(&actor, ObjectType::Channel, "moderator")
        .await
        .is_empty());

    actor
        .ask(replace(UNTRUSTED, "member"))
        .await
        .expect("a member-level replacement");
    actor
        .ask(replace(TRUSTED, "moderator"))
        .await
        .expect("a replacement for a trusted home");
    assert_eq!(
        holders(&actor, ObjectType::Channel, "moderator").await,
        [TRUSTED]
    );
    // Removing a relation names no replacement and is never refused.
    actor
        .ask(ReplaceExclusiveRelation {
            object: Object::new(ObjectType::Channel, "lounge"),
            subject: Subject::user(UNTRUSTED),
            family: family(),
            replacement: None,
        })
        .await
        .expect("a removal");
}

#[tokio::test]
async fn an_exclusive_swap_is_refused_for_an_untrusted_home() {
    let actor = deciding("spaces-issuance-swap").await;
    write(&actor, tuple(ObjectType::Channel, "member", UNTRUSTED))
        .await
        .expect("a member relation");
    let swap = |replacement: Option<&str>| SwapExclusiveRelation {
        object: Object::new(ObjectType::Channel, "lounge"),
        subject: Subject::user(UNTRUSTED),
        family: vec![Relation::new("member"), Relation::new("manager")],
        expected: Some(Relation::new("member")),
        replacement: replacement.map(Relation::new),
    };

    let refused = actor.ask(swap(Some("manager"))).await;
    assert!(
        matches!(
            refused,
            Err(SendError::HandlerError(
                PermissionError::UntrustedPrivilegedSubject
            ))
        ),
        "{refused:?}"
    );
    assert_eq!(
        holders(&actor, ObjectType::Channel, "member").await,
        [UNTRUSTED],
        "a refused swap changes nothing"
    );
    assert_eq!(
        actor.ask(swap(None)).await.expect("a swap to no relation"),
        ExclusiveRelationSwap::Swapped
    );
}

/// An actor that was given no policy asks nothing, as before: the suites
/// that write these relations over the local backend build such actors.
#[tokio::test]
async fn an_actor_without_a_policy_refuses_nothing() {
    let actor = PermissionActor::spawn(PermissionActor::new_for_tests(
        database("spaces-issuance-no-policy").await,
    ));
    for (object, relation) in privileged() {
        write(&actor, tuple(object, relation, UNTRUSTED))
            .await
            .unwrap_or_else(|error| panic!("{relation}: {error}"));
    }
}

/// A policy that serves no account domain, which is what a replica whose
/// domains are not canonical attaches, writes these relations for nobody.
#[tokio::test]
async fn a_policy_that_serves_nothing_refuses_every_account() {
    let actor = PermissionActor::spawn(
        PermissionActor::new_for_tests(database("spaces-issuance-empty-policy").await)
            .with_privileged_issuance(IssuancePolicy::default()),
    );
    for account in [LOCAL, TRUSTED, UNTRUSTED] {
        let refused = write(&actor, tuple(ObjectType::Space, "moderator", account)).await;
        assert!(
            matches!(refused, Err(PermissionError::UntrustedPrivilegedSubject)),
            "{account}: {refused:?}"
        );
    }
}
