use std::num::NonZeroUsize;

use jid::FullJid;

use super::{
    AccountResourceExecution, AccountResourceMutation, AccountResourceOperation,
    AccountResourceSlot, AccountResourceSnapshot, AccountResources, AccountResourcesError,
    BindingFenceError, BindingRef, Presentation, UserClaimIdentity,
};
use crate::ownership::account_resource_transition::{
    AccountResourceOperationId, AccountResourceOperationOutcome, ActorRegistrationReceipt,
    BindingEvidence, EndpointPublicationReceipt,
};
use crate::ownership::joint_identity::ResourceLifetime;
use crate::ownership::{ClaimEpoch, ConnectionRef, NodeIdentity, NodeLeaseIdentity};

fn claim() -> UserClaimIdentity {
    UserClaimIdentity::fresh(
        "alice@example.test".parse().expect("valid bare JID"),
        NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-a")),
        ClaimEpoch(1),
    )
}

fn account(max: usize) -> AccountResources {
    AccountResources::new(
        claim_for(&claim()),
        NonZeroUsize::new(max).expect("positive"),
    )
}

fn claim_for(claim: &UserClaimIdentity) -> UserClaimIdentity {
    claim.clone()
}

fn resource(name: &str) -> FullJid {
    format!("alice@example.test/{name}")
        .parse()
        .expect("valid full JID")
}

fn fresh_binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
}

fn commit(account: &mut AccountResources, jid: &FullJid, mutation: AccountResourceMutation) {
    let operation = AccountResourceOperation::new(AccountResourceOperationId::fresh(), mutation);
    match account
        .execute(jid, operation)
        .expect("slot accepts the operation")
    {
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
            ..
        }) => {}
        other => panic!("expected a committed mutation, got {other:?}"),
    }
}

fn revision(
    account: &AccountResources,
    jid: &FullJid,
) -> crate::ownership::account_resource_transition::AccountResourceRevision {
    account.slot(jid).map_or(
        crate::ownership::account_resource_transition::AccountResourceRevision::INITIAL,
        AccountResourceSlot::revision,
    )
}

/// Bind `jid` and take the binding to `Active` with the given presence.
fn bind_active(
    account: &mut AccountResources,
    jid: &FullJid,
    available: bool,
    priority: i8,
    carbons: bool,
) -> BindingRef {
    let claim = account.claim_identity().clone();
    let binding = fresh_binding();
    commit(
        account,
        jid,
        AccountResourceMutation::Bind {
            claim_identity: claim.clone(),
            expected_revision: revision(account, jid),
            binding_ref: binding.clone(),
            lifetime: ResourceLifetime::fresh(),
        },
    );
    commit(
        account,
        jid,
        AccountResourceMutation::PublishEndpoint {
            claim_identity: claim.clone(),
            expected_revision: revision(account, jid),
            binding_ref: binding.clone(),
            publication: EndpointPublicationReceipt::observed(BindingEvidence::for_binding(
                &binding,
            )),
        },
    );
    commit(
        account,
        jid,
        AccountResourceMutation::RegisterActor {
            claim_identity: claim.clone(),
            expected_revision: revision(account, jid),
            binding_ref: binding.clone(),
            registration: ActorRegistrationReceipt::observed(
                claim.clone(),
                BindingEvidence::for_binding(&binding),
            ),
        },
    );
    commit(
        account,
        jid,
        AccountResourceMutation::UpdateSnapshot {
            claim_identity: claim,
            expected_revision: revision(account, jid),
            binding_ref: binding.clone(),
            snapshot: AccountResourceSnapshot {
                presence_available: available,
                presence_priority: priority,
                carbons_enabled: carbons,
                ..AccountResourceSnapshot::default()
            },
        },
    );
    binding
}

#[test]
fn only_a_bind_creates_a_slot_and_the_account_bounds_them() {
    let mut account = account(1);
    let phone = resource("phone");
    let claim = account.claim_identity().clone();
    let binding = fresh_binding();
    let publish = AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::PublishEndpoint {
            claim_identity: claim.clone(),
            expected_revision: revision(&account, &phone),
            binding_ref: binding.clone(),
            publication: EndpointPublicationReceipt::observed(BindingEvidence::for_binding(
                &binding,
            )),
        },
    );
    assert_eq!(
        account.execute(&phone, publish).unwrap_err(),
        AccountResourcesError::UnknownResource
    );
    assert!(account.slot(&phone).is_none());

    bind_active(&mut account, &phone, true, 0, false);
    let laptop = resource("laptop");
    let second = AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::Bind {
            claim_identity: claim,
            expected_revision: revision(&account, &laptop),
            binding_ref: fresh_binding(),
            lifetime: ResourceLifetime::fresh(),
        },
    );
    assert_eq!(
        account.execute(&laptop, second).unwrap_err(),
        AccountResourcesError::ResourceLimit
    );

    let other: FullJid = "bob@example.test/phone".parse().expect("valid full JID");
    let foreign = AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::Bind {
            claim_identity: account.claim_identity().clone(),
            expected_revision: revision(&account, &other),
            binding_ref: fresh_binding(),
            lifetime: ResourceLifetime::fresh(),
        },
    );
    assert_eq!(
        account.execute(&other, foreign).unwrap_err(),
        AccountResourcesError::AccountMismatch
    );
}

#[test]
fn a_binding_is_a_destination_only_once_active() {
    let mut account = account(4);
    let phone = resource("phone");
    let claim = account.claim_identity().clone();
    let binding = fresh_binding();
    let expected_revision = revision(&account, &phone);
    commit(
        &mut account,
        &phone,
        AccountResourceMutation::Bind {
            claim_identity: claim.clone(),
            expected_revision,
            binding_ref: binding.clone(),
            lifetime: ResourceLifetime::fresh(),
        },
    );
    assert_eq!(account.binding(&phone), Some(&binding));
    assert!(account.resources().is_empty());
    let expected_revision = revision(&account, &phone);
    commit(
        &mut account,
        &phone,
        AccountResourceMutation::PublishEndpoint {
            claim_identity: claim,
            expected_revision,
            binding_ref: binding.clone(),
            publication: EndpointPublicationReceipt::observed(BindingEvidence::for_binding(
                &binding,
            )),
        },
    );
    assert!(account.resources().is_empty());
    assert!(account.routable_resources().is_empty());
}

/// One account and the selections expected from it. A resource is
/// `(name, available, priority, carbons)`.
struct SelectionCase {
    resources: &'static [(&'static str, bool, i8, bool)],
    available: &'static [&'static str],
    routable: &'static [&'static str],
}

#[test]
fn bare_jid_selection_follows_the_user_actor_rules() {
    let table = [
        // No resources: nothing is routable.
        SelectionCase {
            resources: &[],
            available: &[],
            routable: &[],
        },
        // The highest non-negative priority wins.
        SelectionCase {
            resources: &[("a", true, 1, false), ("b", true, 5, true)],
            available: &["a", "b"],
            routable: &["b"],
        },
        // Ties at the highest priority all receive the stanza.
        SelectionCase {
            resources: &[
                ("a", true, 5, false),
                ("b", true, 5, false),
                ("c", true, 0, false),
            ],
            available: &["a", "b", "c"],
            routable: &["a", "b"],
        },
        // A negative priority is available but never a bare-JID destination.
        SelectionCase {
            resources: &[("a", true, -1, false)],
            available: &["a"],
            routable: &[],
        },
        // An unavailable resource is neither, whatever its priority.
        SelectionCase {
            resources: &[("a", false, 9, false), ("b", true, 0, false)],
            available: &["b"],
            routable: &["b"],
        },
    ];
    for SelectionCase {
        resources,
        available,
        routable,
    } in table
    {
        let mut account = account(8);
        for (name, is_available, priority, carbons) in resources {
            bind_active(
                &mut account,
                &resource(name),
                *is_available,
                *priority,
                *carbons,
            );
        }
        let names = |jids: Vec<FullJid>| -> Vec<String> {
            jids.into_iter()
                .map(|jid| jid.resource().to_string())
                .collect()
        };
        assert_eq!(account.resources().len(), resources.len(), "{resources:?}");
        assert_eq!(
            names(
                account
                    .available_resources()
                    .into_iter()
                    .map(|(jid, _)| jid)
                    .collect()
            ),
            available,
            "{resources:?}"
        );
        assert_eq!(
            names(account.routable_resources()),
            routable,
            "{resources:?}"
        );
        let snapshot = account.routing_snapshot();
        assert!(snapshot.windows(2).all(|pair| pair[0].jid < pair[1].jid));
        for (name, is_available, priority, carbons) in resources {
            let row = snapshot
                .iter()
                .find(|row| row.jid == resource(name))
                .expect("every active resource is in the routing snapshot");
            assert_eq!(
                (row.available, row.priority, row.carbons_enabled),
                (*is_available, *priority, *carbons)
            );
        }
    }
}

#[test]
fn a_command_must_name_the_current_binding() {
    let mut account = account(4);
    let phone = resource("phone");
    assert_eq!(
        account.check_fence(&phone, &fresh_binding()),
        Err(BindingFenceError::UnknownResource)
    );
    let binding = bind_active(&mut account, &phone, true, 0, false);
    assert_eq!(account.check_fence(&phone, &binding), Ok(()));
    assert_eq!(
        account.check_fence(&phone, &fresh_binding()),
        Err(BindingFenceError::Superseded)
    );
    // A second attempt on the same transport is still a different binding.
    let same_socket = BindingRef::fresh(binding.connection_ref().clone());
    assert_eq!(
        account.check_fence(&phone, &same_socket),
        Err(BindingFenceError::Superseded)
    );
}

#[test]
fn a_presentation_is_recorded_once_per_binding_without_a_session() {
    let mut account = account(4);
    let phone = resource("phone");
    let binding = bind_active(&mut account, &phone, true, 0, false);
    assert_eq!(
        account.presented(&phone, &binding, Presentation::OfflineMessages),
        Ok(false)
    );
    assert_eq!(
        account.mark_presented(&phone, &binding, Presentation::OfflineMessages),
        Ok(true)
    );
    assert_eq!(
        account.mark_presented(&phone, &binding, Presentation::OfflineMessages),
        Ok(false)
    );
    assert_eq!(
        account.presented(&phone, &binding, Presentation::PendingSubscriptions),
        Ok(false)
    );
    assert_eq!(
        account.mark_presented(&phone, &fresh_binding(), Presentation::OfflineMessages),
        Err(BindingFenceError::Superseded)
    );
}
