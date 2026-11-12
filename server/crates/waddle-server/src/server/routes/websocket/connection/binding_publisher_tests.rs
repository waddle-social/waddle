//! The binding publisher trait under both of its implementations.

use std::sync::Arc;

use jid::FullJid;
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::registry::{ConnectionEntry, OutboundStanza};

use super::binding::{
    BindingConfirmation, BindingPublication, BindingPublisher, BindingRetirement,
};
use crate::account_bindings::InMemoryAccountBindingStore;
use crate::account_coordinator::{
    AccountBindingAuthority, AccountCoordinatorConfig, AccountCoordinators,
    CoordinatorBindingPublisher, LocalNodeLiveness, ProcessClaimSource, RetireEvidence,
};
use crate::server::routes::websocket::registration::{RegistryBindingPublisher, RegistryRefusal};
use crate::server::routes::websocket::tests::create_test_websocket_state;

fn full_jid(resource: &str) -> FullJid {
    format!("alice@localhost/{resource}")
        .parse()
        .expect("valid full JID")
}

fn binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
}

fn coordinators() -> Arc<AccountCoordinators> {
    Arc::new(AccountCoordinators::new(
        Arc::new(InMemoryAccountBindingStore::new()),
        Arc::new(ProcessClaimSource),
        Arc::new(LocalNodeLiveness::new(NodeLeaseIdentity::local_process())),
        AccountCoordinatorConfig::default(),
    ))
}

fn entry(binding: &BindingRef) -> ConnectionEntry {
    let (outbound, _receiver) = tokio::sync::mpsc::channel::<OutboundStanza>(4);
    ConnectionEntry::new_for_binding(outbound, binding.clone())
}

#[tokio::test]
async fn coordinator_publishes_confirms_and_retires_a_binding() {
    let coordinators = coordinators();
    let publisher = CoordinatorBindingPublisher::new(Arc::clone(&coordinators));
    let jid = full_jid("laptop");
    let binding = binding();

    let published =
        match BindingPublisher::publish(&publisher, &jid, &binding, ResourceLifetime::fresh())
            .await
            .expect("publish outcome is known")
        {
            BindingPublication::Published(published) => published,
            other => panic!("expected a published binding, got {other:?}"),
        };
    assert_eq!(published.binding_ref, binding);

    let confirmation = BindingPublisher::confirm(&publisher, &jid, &published)
        .await
        .expect("confirm outcome is known");
    assert!(matches!(confirmation, BindingConfirmation::Confirmed(_)));
    assert!(BindingPublisher::withdraw(&publisher, &jid, &published).is_none());

    let retirement = BindingPublisher::retire(
        &publisher,
        &jid,
        &published,
        RetireEvidence::recovery_owned(),
    )
    .await
    .expect("retire outcome is known");
    assert_eq!(retirement, BindingRetirement::Retired);
    let status = coordinators.slot_status(&jid).await.expect("slot status");
    assert!(status.binding.is_none(), "the resource is free");

    // Retiring the same binding again finds it no longer current.
    let again = BindingPublisher::retire(
        &publisher,
        &jid,
        &published,
        RetireEvidence::recovery_owned(),
    )
    .await
    .expect("retire outcome is known");
    assert!(matches!(again, BindingRetirement::NotCurrent { .. }));
}

#[tokio::test]
async fn coordinator_reports_a_pending_replacement_until_the_displaced_binding_retires() {
    let publisher = CoordinatorBindingPublisher::new(coordinators());
    let jid = full_jid("laptop");
    let first = binding();
    let second = binding();

    let BindingPublication::Published(incumbent) =
        BindingPublisher::publish(&publisher, &jid, &first, ResourceLifetime::fresh())
            .await
            .expect("publish outcome is known")
    else {
        panic!("the first binding is published");
    };
    let pending = BindingPublisher::publish(&publisher, &jid, &second, ResourceLifetime::fresh())
        .await
        .expect("publish outcome is known");
    match pending {
        BindingPublication::ReplacementPending { displaced, .. } => assert_eq!(displaced, first),
        other => panic!("expected a pending replacement, got {other:?}"),
    }

    let retired = BindingPublisher::retire(
        &publisher,
        &jid,
        &incumbent,
        RetireEvidence::recovery_owned(),
    )
    .await
    .expect("retire outcome is known");
    assert_eq!(retired, BindingRetirement::Retired);

    let replaced = BindingPublisher::publish(&publisher, &jid, &second, ResourceLifetime::fresh())
        .await
        .expect("publish outcome is known");
    assert!(
        matches!(replaced, BindingPublication::Published(ref published) if published.binding_ref == second),
        "the replacing binding binds again under a new operation: {replaced:?}"
    );
}

#[tokio::test]
async fn registry_publishes_the_staged_endpoint_and_withdraws_it_by_owner() {
    let state = create_test_websocket_state().await;
    let jid = full_jid("laptop");
    let binding = binding();

    let publisher = RegistryBindingPublisher::with_endpoint(&state, entry(&binding));
    let BindingPublication::Published(published) = publisher
        .publish(&jid, &binding, ResourceLifetime::fresh())
        .await
        .expect("the registry publisher cannot fail")
    else {
        panic!("the staged endpoint is registered");
    };
    assert!(!published.resumed);
    assert!(state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &published.owner)
        .is_some());

    // The staged endpoint is consumed by the publication.
    let again = publisher
        .publish(&jid, &binding, ResourceLifetime::fresh())
        .await
        .expect("the registry publisher cannot fail");
    assert_eq!(
        again,
        BindingPublication::Refused(RegistryRefusal::NoEndpoint)
    );

    assert!(publisher.withdraw(&jid, &published).is_some());
    assert!(publisher.withdraw(&jid, &published).is_none());
    assert!(state
        .deps
        .protocol
        .connection_registry
        .entry(&jid)
        .is_none());
}

#[tokio::test]
async fn registry_refuses_an_endpoint_staged_for_another_binding() {
    let state = create_test_websocket_state().await;
    let jid = full_jid("laptop");
    let staged = binding();
    let other = binding();

    let publisher = RegistryBindingPublisher::with_endpoint(&state, entry(&staged));
    let publication = publisher
        .publish_resumed(&jid, &other, ())
        .await
        .expect("the registry publisher cannot fail");
    assert_eq!(
        publication,
        BindingPublication::Refused(RegistryRefusal::EndpointBindingMismatch)
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .entry(&jid)
        .is_none());
}
