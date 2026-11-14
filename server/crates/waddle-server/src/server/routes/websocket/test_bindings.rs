//! Test-only publication of connection-registry entries to the account
//! coordinator.
//!
//! The account coordinator is the only account-resource authority: routing
//! reads a resource's binding, presence, carbons and push interest from it,
//! never from the connection registry. A production connection publishes and
//! confirms its binding when it binds and sends its resource snapshot after
//! each handled frame (`super::account_binding`). Fixtures that stand in for
//! a connection by registering a channel in the connection registry and
//! setting its presence or carbons there do the connection's half with
//! [`sync_test_bindings`]: every locally hosted entry is published under the
//! binding it carries, its snapshot is sent, and a binding this module
//! published whose entry is gone or replaced is retired.
//!
//! A binding published by a real connection (through registration) is left
//! to that connection: its snapshot names its stream-management session,
//! which only the connection knows.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use jid::FullJid;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecution, AccountResourceMutation,
    AccountResourceOperation, AccountResourceOperationId, AccountResourceOperationOutcome,
    AccountResourceSnapshot,
};
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::registry::ConnectionRegistry;

use super::stream_management::sm_show_from_name;
use super::WebSocketState;
use crate::account_coordinator::{
    AccountBindingAuthority, BindingReply, ConfirmOutcome, CoordinatorBindingPublisher,
    PublishOutcome, RetireEvidence, RoutedAccountAuthority,
};
use crate::server::AppState;

/// The bindings this module published, per application, with the full JID
/// each one binds. Keyed by the application's address so fixtures that
/// share a process never retire each other's bindings.
type Published = HashMap<usize, HashMap<BindingRef, FullJid>>;

fn published() -> &'static Mutex<Published> {
    static PUBLISHED: OnceLock<Mutex<Published>> = OnceLock::new();
    PUBLISHED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn application_key(app: &AppState) -> usize {
    std::ptr::from_ref(app) as usize
}

fn record(app: &AppState, binding: &BindingRef, jid: &FullJid) {
    published()
        .lock()
        .expect("test binding table")
        .entry(application_key(app))
        .or_default()
        .insert(binding.clone(), jid.clone());
}

fn recorded(app: &AppState) -> Vec<(BindingRef, FullJid)> {
    published()
        .lock()
        .expect("test binding table")
        .get(&application_key(app))
        .map(|bindings| {
            bindings
                .iter()
                .map(|(binding, jid)| (binding.clone(), jid.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn forget(app: &AppState, binding: &BindingRef) {
    if let Some(bindings) = published()
        .lock()
        .expect("test binding table")
        .get_mut(&application_key(app))
    {
        bindings.remove(binding);
    }
}

fn is_recorded(app: &AppState, binding: &BindingRef) -> bool {
    published()
        .lock()
        .expect("test binding table")
        .get(&application_key(app))
        .is_some_and(|bindings| bindings.contains_key(binding))
}

fn authority(app: &AppState) -> Option<Arc<RoutedAccountAuthority>> {
    app.account_authority().authority().cloned()
}

/// Bring the account coordinator in line with the application's connection
/// registry: publish and confirm every locally hosted entry's binding, send
/// each entry's presence, carbons and push interest as its snapshot, and
/// retire the bindings this module published whose entry is gone or now
/// carries another binding. Safe to repeat; an application without a
/// selected coordinator is left alone.
#[cfg(test)]
pub async fn sync_test_bindings(state: &WebSocketState) {
    sync_registry_bindings(state, &state.deps.protocol.connection_registry).await;
}

/// [`sync_test_bindings`] for a fixture that delivers through its own
/// connection registry and reads the account coordinator of `state`.
pub async fn sync_registry_bindings(state: &WebSocketState, registry: &ConnectionRegistry) {
    sync_application_bindings(&state.deps.app_state, registry).await;
}

/// [`sync_test_bindings`] for a fixture that has only the application
/// state, not the WebSocket route state.
pub async fn sync_application_bindings(app: &AppState, registry: &ConnectionRegistry) {
    let Some(authority) = authority(app) else {
        return;
    };
    for (binding, jid) in recorded(app) {
        let current = registry
            .entry(&jid)
            .filter(|entry| entry.is_locally_hosted())
            .and_then(|entry| entry.binding_ref().cloned());
        if current.as_ref() != Some(&binding) {
            retire(app, &authority, &jid, &binding).await;
        }
    }
    let mut jids = registry.list_connections();
    jids.sort();
    for jid in jids {
        sync_binding(app, &authority, registry, &jid).await;
    }
}

/// [`sync_test_bindings`] for one full JID.
pub async fn sync_test_binding(state: &WebSocketState, jid: &FullJid) {
    let Some(authority) = authority(&state.deps.app_state) else {
        return;
    };
    sync_binding(
        &state.deps.app_state,
        &authority,
        &state.deps.protocol.connection_registry,
        jid,
    )
    .await;
}

async fn sync_binding(
    app: &AppState,
    authority: &Arc<RoutedAccountAuthority>,
    registry: &ConnectionRegistry,
    jid: &FullJid,
) {
    let Some(entry) = registry
        .entry(jid)
        .filter(|entry| entry.is_locally_hosted())
    else {
        return;
    };
    let Some(binding) = entry.binding_ref().cloned() else {
        return;
    };
    let status = authority
        .slot_status(jid)
        .await
        .expect("account coordinator answers a slot read");
    match &status.binding {
        Some((current, AccountResourceBindingState::Active)) if current == &binding => {
            if !is_recorded(app, &binding) {
                // Published by a real connection, which keeps its own
                // snapshot.
                return;
            }
        }
        Some((current, _)) if current != &binding => {
            retire(app, authority, jid, current).await;
            publish(app, authority, jid, &binding).await;
        }
        _ => publish(app, authority, jid, &binding).await,
    }
    let available = entry.is_presence_available();
    let advertised = registry.presence_state(jid).filter(|_| available);
    let snapshot = match advertised {
        Some(advertised) => AccountResourceSnapshot {
            presence_available: true,
            presence_show: advertised.show.as_deref().and_then(sm_show_from_name),
            presence_status: advertised.status,
            presence_priority: advertised.priority,
            presence_payloads: advertised.payloads,
            ..AccountResourceSnapshot::default()
        },
        None => AccountResourceSnapshot {
            presence_available: available,
            presence_priority: entry.presence_priority(),
            ..AccountResourceSnapshot::default()
        },
    };
    let snapshot = AccountResourceSnapshot {
        carbons_enabled: entry.is_carbons_enabled(),
        roster_interested: registry.is_roster_interested(jid),
        blocklist_interested: registry.is_blocklist_interested(jid),
        ..snapshot
    };
    update_snapshot(authority, jid, binding, snapshot).await;
}

async fn update_snapshot(
    authority: &Arc<RoutedAccountAuthority>,
    jid: &FullJid,
    binding: BindingRef,
    snapshot: AccountResourceSnapshot,
) {
    let status = authority
        .slot_status(jid)
        .await
        .expect("account coordinator answers a slot read");
    let operation = AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::UpdateSnapshot {
            claim_identity: status.claim_identity,
            expected_revision: status.revision,
            binding_ref: binding,
            snapshot,
        },
    );
    let reply = authority
        .execute(jid, operation)
        .await
        .expect("account coordinator answers a snapshot update");
    assert!(
        matches!(
            reply,
            BindingReply::Executed(
                AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Committed { .. }
                ) | AccountResourceExecution::Replayed(
                    AccountResourceOperationOutcome::Committed { .. }
                )
            )
        ),
        "the coordinator commits the test snapshot of {jid}: {reply:?}"
    );
}

async fn publish(
    app: &AppState,
    authority: &Arc<RoutedAccountAuthority>,
    jid: &FullJid,
    binding: &BindingRef,
) {
    publish_and_confirm(app, authority, jid, binding).await;
    record(app, binding, jid);
}

/// Bind `jid` at the account coordinator of `state` under `binding`, a
/// binding whose connection another node hosts, with `snapshot` as the
/// resource's state. Routing on this node then reaches the resource through
/// a bound request addressed to that node, as it reaches a resource that a
/// connection on another node bound. Registry syncs leave the binding
/// alone: no local entry carries it. Only clustered routing reaches another
/// node's binding, so only clustered tests use it.
#[cfg(all(test, feature = "clustering"))]
pub async fn bind_remote_test_resource(
    state: &WebSocketState,
    jid: &FullJid,
    binding: &BindingRef,
    snapshot: AccountResourceSnapshot,
) {
    let app = &state.deps.app_state;
    let authority = authority(app).expect("the fixture selects an account coordinator");
    publish_and_confirm(app, &authority, jid, binding).await;
    update_snapshot(&authority, jid, binding.clone(), snapshot).await;
}

async fn publish_and_confirm(
    app: &AppState,
    authority: &Arc<RoutedAccountAuthority>,
    jid: &FullJid,
    binding: &BindingRef,
) {
    let publisher =
        CoordinatorBindingPublisher::new(Arc::clone(authority) as Arc<dyn AccountBindingAuthority>);
    let published = match publisher
        .publish(jid, binding, ResourceLifetime::fresh())
        .await
        .expect("account coordinator answers a publication")
    {
        PublishOutcome::Published(published) => published,
        PublishOutcome::ReplacementPending { displaced, .. } => {
            retire(app, authority, jid, &displaced).await;
            match publisher
                .publish(jid, binding, ResourceLifetime::fresh())
                .await
                .expect("account coordinator answers a publication")
            {
                PublishOutcome::Published(published) => published,
                other => panic!(
                    "the coordinator publishes {jid} after its predecessor retired: {other:?}"
                ),
            }
        }
        PublishOutcome::Refused(error) => {
            panic!("the coordinator refused the test binding of {jid}: {error:?}")
        }
    };
    match publisher
        .confirm(jid, &published)
        .await
        .expect("account coordinator answers a confirmation")
    {
        ConfirmOutcome::Confirmed(_) => {}
        other => panic!("the coordinator confirms the test binding of {jid}: {other:?}"),
    }
}

async fn retire(
    app: &AppState,
    authority: &Arc<RoutedAccountAuthority>,
    jid: &FullJid,
    binding: &BindingRef,
) {
    let publisher =
        CoordinatorBindingPublisher::new(Arc::clone(authority) as Arc<dyn AccountBindingAuthority>);
    publisher
        .retire_binding(jid, binding, RetireEvidence::recovery_owned())
        .await
        .expect("account coordinator answers a retirement");
    forget(app, binding);
}
