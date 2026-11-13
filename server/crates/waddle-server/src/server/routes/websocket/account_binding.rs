//! A connection's binding at the account coordinator.
//!
//! A connection drives the account coordinator's slot for its resource
//! (`AppState::account_authority`) through the binding publisher and
//! [`AccountBindingAuthority`]. [`AccountBindingLink`] is what the
//! connection keeps of that: the published binding and the snapshot the
//! coordinator last committed for it. An application that never selected
//! the coordinator (a fixture that skipped startup) holds no link and every
//! function here does nothing.
//!
//! The connection states what happened to its own binding and the
//! coordinator decides:
//!
//! - registration publishes and confirms the binding
//!   (`super::registration`), resolving a pending replacement here;
//! - after each handled frame the connection's presence, carbons, roster
//!   and block-list interest and its stream-management reference are
//!   compared with the committed snapshot and sent as `UpdateSnapshot` when
//!   they differ;
//! - a one-time presentation (offline messages, pending subscription
//!   requests) is recorded before it is made;
//! - teardown detaches the binding when its session stays resumable and
//!   retires it otherwise.

use std::sync::Arc;
use std::time::Duration;

use jid::FullJid;
use tracing::{debug, warn};
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecution, AccountResourceMutation,
    AccountResourceOperation, AccountResourceOperationId, AccountResourceOperationOutcome,
    AccountResourceSnapshot, BindingEvidence, BindingRetirementDisposition,
    EndpointPublicationReceipt, EndpointRetirementEvidence, SuccessfulResumeReceipt,
};
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::registry::{ConnectionEntry, ForceDetachOrigin, ForceDetachRequest};

use super::cleanup::ConnectionShutdownOutcome;
use super::connection::{BindingPublisher, BindingRetirement, CoordinatorResume};
use super::registration::recovery_owned_retirement;
use super::state::WsConnState;
use super::stream_management::sm_show_from_name;
use super::WebSocketState;
use crate::account_coordinator::{
    AccountBindingAuthority, BindingReply, CoordinatorBindingPublisher, PublishedBinding,
    ReleaseSessionRequest, RetireEvidence, RetireOutcome, RoutedAccountAuthority, SessionRelease,
};

/// How often a replacing connection re-reads the slot of the binding it
/// displaces while that binding retires.
const DISPLACED_RETIREMENT_POLL: Duration = Duration::from_millis(50);

/// What a connection holds of its binding at the account coordinator.
#[derive(Debug, Clone)]
pub(super) struct AccountBindingLink {
    full_jid: FullJid,
    published: PublishedBinding,
    /// The snapshot the coordinator last committed for this binding, once
    /// the connection has sent one.
    committed: Option<AccountResourceSnapshot>,
}

impl AccountBindingLink {
    pub(super) fn new(full_jid: FullJid, published: PublishedBinding) -> Self {
        Self {
            full_jid,
            published,
            committed: None,
        }
    }

    pub(super) fn published(&self) -> &PublishedBinding {
        &self.published
    }
}

/// The routed account authority, once startup selected the account
/// coordinator. It reaches the coordinator of the node that
/// owns each account, wherever the connection is.
pub(super) fn selected_authority(state: &WebSocketState) -> Option<Arc<RoutedAccountAuthority>> {
    state
        .deps
        .app_state
        .account_authority()
        .authority()
        .cloned()
}

fn committed(reply: &BindingReply) -> bool {
    matches!(
        reply,
        BindingReply::Executed(
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { .. })
                | AccountResourceExecution::Replayed(
                    AccountResourceOperationOutcome::Committed { .. }
                )
        )
    )
}

/// The resource snapshot the connection currently projects.
///
/// A registered connection's presence lives on its connection-registry
/// entry, where the presence handlers write it; the connection's own
/// presence fields hold only what a resume restored, for the turns before
/// its entry exists. The entry is read owner-gated, so a connection whose
/// full JID was taken by a replacement does not project the replacement's
/// presence.
fn projected_snapshot(
    state: &WebSocketState,
    conn: &WsConnState,
    jid: &FullJid,
) -> AccountResourceSnapshot {
    let registry = &state.deps.protocol.connection_registry;
    let entry = conn
        .registry_owner
        .as_ref()
        .and_then(|owner| registry.entry_if_owner(jid, owner));
    let (presence_available, presence_show, presence_status, presence_priority, presence_payloads) =
        match entry {
            Some(entry) => {
                let available = entry.is_presence_available();
                let advertised = registry.presence_state(jid).filter(|_| available);
                match advertised {
                    Some(advertised) => (
                        true,
                        advertised.show.as_deref().and_then(sm_show_from_name),
                        advertised.status,
                        advertised.priority,
                        advertised.payloads,
                    ),
                    None => (available, None, None, entry.presence_priority(), Vec::new()),
                }
            }
            None => (
                conn.presence_available,
                conn.presence_show.clone(),
                conn.presence_status.clone(),
                conn.presence_priority,
                conn.presence_payloads.clone(),
            ),
        };
    AccountResourceSnapshot {
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        presence_payloads,
        carbons_enabled: conn.carbons_enabled,
        roster_interested: conn.roster_interested,
        blocklist_interested: conn.blocklist_interested,
        stream_management_ref: conn.durable_sm.session_binding(),
    }
}

/// Send the connection's resource snapshot to the coordinator when it
/// differs from the one last committed for the binding.
///
/// The durable stream-management generation the snapshot names is bound in
/// the session store before this runs: an enable and a resume commit there
/// before the connection holds them as its link.
pub(super) async fn sync_snapshot(state: &WebSocketState, conn: &mut WsConnState) {
    let Some(authority) = selected_authority(state) else {
        return;
    };
    let Some(jid) = conn
        .account_binding
        .as_ref()
        .map(|link| link.full_jid.clone())
    else {
        return;
    };
    let snapshot = projected_snapshot(state, conn, &jid);
    let Some(link) = conn.account_binding.as_mut() else {
        return;
    };
    if link.committed.as_ref() == Some(&snapshot) {
        return;
    }
    let status = match authority.slot_status(&link.full_jid).await {
        Ok(status) => status,
        Err(error) => {
            warn!(jid = %link.full_jid, %error, "account coordinator did not answer a slot read");
            return;
        }
    };
    if !matches!(
        &status.binding,
        Some((current, AccountResourceBindingState::Active)) if current == &link.published.binding_ref
    ) {
        // Not yet confirmed, or no longer this connection's resource. A
        // superseded connection is retired by its replacement.
        return;
    }
    let operation = AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::UpdateSnapshot {
            claim_identity: status.claim_identity,
            expected_revision: status.revision,
            binding_ref: link.published.binding_ref.clone(),
            snapshot: snapshot.clone(),
        },
    );
    match authority.execute(&link.full_jid, operation).await {
        Ok(reply) if committed(&reply) => link.committed = Some(snapshot),
        Ok(reply) => {
            warn!(jid = %link.full_jid, ?reply, "account coordinator refused a resource snapshot");
        }
        Err(error) => {
            warn!(jid = %link.full_jid, %error, "a resource snapshot has an unknown outcome");
        }
    }
}

/// What the coordinator needs to move a resource's slot to the binding of
/// a committed resume: the detached binding the slot holds and the
/// stream-management generations before and after the resume.
///
/// `None` when the slot holds no detached binding that references the
/// resumed session. The caller then publishes the binding as a new one if
/// the slot is free, and is refused otherwise.
pub(super) async fn resume_evidence(
    authority: &RoutedAccountAuthority,
    conn: &WsConnState,
    jid: &FullJid,
    binding: &BindingRef,
) -> Option<CoordinatorResume> {
    let resumed_sm = conn.durable_sm.session_binding()?;
    let stored = match authority.store().load(jid).await {
        Ok(stored) => stored?,
        Err(error) => {
            warn!(%jid, %error, "the resumed resource's binding row could not be read");
            return None;
        }
    };
    let previous = stored.image.binding?;
    let previous_sm = previous.snapshot.stream_management_ref?;
    if previous.state != AccountResourceBindingState::Detached
        || previous_sm.session != resumed_sm.session
    {
        return None;
    }
    Some(CoordinatorResume {
        receipt: SuccessfulResumeReceipt::observed(
            BindingEvidence::for_binding(&previous.binding_ref),
            BindingEvidence::for_binding(binding),
            previous_sm,
            resumed_sm,
            previous.lifetime,
            previous
                .detach_custody
                .unwrap_or(BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner),
        ),
        previous: previous.binding_ref,
    })
}

/// Whether `jid`'s slot still holds `binding`.
async fn slot_holds(
    authority: &RoutedAccountAuthority,
    jid: &FullJid,
    binding: &BindingRef,
) -> Result<bool, crate::account_coordinator::AccountCoordinatorError> {
    Ok(authority
        .slot_status(jid)
        .await?
        .binding
        .is_some_and(|(current, _)| &current == binding))
}

/// Retire a binding whose connection is gone, as the coordinator's
/// liveness reports it, without the connection's evidence: the recovery
/// paths own the work it accepted. Returns whether the slot no longer
/// holds it.
async fn retire_expired_binding(
    authority: &Arc<RoutedAccountAuthority>,
    jid: &FullJid,
    binding: &BindingRef,
) -> bool {
    let publisher =
        CoordinatorBindingPublisher::new(Arc::clone(authority) as Arc<dyn AccountBindingAuthority>);
    match publisher
        .retire_binding(jid, binding, RetireEvidence::recovery_owned())
        .await
    {
        Ok(
            RetireOutcome::Retired { .. } | RetireOutcome::Stale(_) | RetireOutcome::Superseded(_),
        ) => true,
        Ok(RetireOutcome::Refused(error)) => {
            warn!(%jid, ?error, "an expired binding's retirement was refused");
            false
        }
        Err(error) => {
            warn!(%jid, %error, "an expired binding's retirement has an unknown outcome");
            false
        }
    }
}

/// Release every account binding that names a terminal stream-management
/// session, so its row can be deleted: the resource row releases first,
/// the session row is deleted last.
///
/// Each binding is retired through its account's coordinator with the
/// recovery paths as the owner of what it accepted. A binding that still
/// has a connection is left to that connection. Returns whether no
/// resource names the session any more; with the connection registry as
/// the account authority nothing names one and this is `true`.
pub(super) async fn release_session_bindings(
    state: &WebSocketState,
    session: waddle_xmpp::ownership::SmSessionKey,
) -> bool {
    let Some(authority) = selected_authority(state) else {
        return true;
    };
    let resources = match authority.store().resources_for_session(session).await {
        Ok(resources) => resources,
        Err(error) => {
            warn!(?session, %error, "the bindings of a terminal session could not be read");
            return false;
        }
    };
    let mut released = true;
    for full_jid in resources {
        let evidence = RetireEvidence::recovery_owned();
        let request = ReleaseSessionRequest {
            begin: evidence.begin,
            complete: evidence.complete,
            session,
            custody_disposition: evidence.custody_disposition,
            directed_presence: evidence.directed_presence,
            presentation_flush: evidence.presentation_flush,
        };
        match authority.release_session(&full_jid, request).await {
            Ok(SessionRelease::Released { .. } | SessionRelease::NotReferenced) => {}
            Ok(SessionRelease::HeldByConnection(..)) => {
                debug!(jid = %full_jid, ?session, "a terminal session is still held by a connection");
                released = false;
            }
            Ok(SessionRelease::Rejected(error)) => {
                warn!(jid = %full_jid, ?session, ?error, "a terminal session's binding was not released");
                released = false;
            }
            Err(error) => {
                warn!(jid = %full_jid, ?session, %error, "a session release has an unknown outcome");
                released = false;
            }
        }
    }
    released
}

/// Have the binding a bind displaces retired, and wait until the slot no
/// longer holds it or `deadline` passes.
///
/// The coordinator reserves the resource for the replacing binding and
/// never retires the displaced one itself. A displaced binding on a live
/// connection is force-detached as a fresh-bind replacement, and retires
/// through its own teardown: on this node directly, and on another node by
/// asking the node that hosts the connection. One whose connection is gone
/// is retired here once the coordinator reports it expired. Returns whether
/// the slot was released in time.
pub(super) async fn await_displaced_retirement(
    state: &WebSocketState,
    authority: &Arc<RoutedAccountAuthority>,
    jid: &FullJid,
    displaced: &BindingRef,
    deadline: tokio::time::Instant,
) -> bool {
    let live: Option<ConnectionEntry> = state
        .deps
        .protocol
        .connection_registry
        .entry(jid)
        .filter(|entry| entry.binding_ref() == Some(displaced));
    // The acknowledgement is not awaited: the slot is the evidence that the
    // displaced binding retired.
    let _ack = live.as_ref().map(|entry| {
        let (ack, ack_rx) = tokio::sync::oneshot::channel();
        let _ = entry.try_force_detach_and_interrupt(ForceDetachRequest {
            origin: ForceDetachOrigin::FreshBindReplacement,
            target_binding: Some(displaced.clone()),
            requester_bare_jid: jid.to_bare(),
            ack,
        });
        ack_rx
    });
    if live.is_none() {
        authority.displace_remote(jid, displaced).await;
    }
    let account = jid.to_bare();
    loop {
        match slot_holds(authority, jid, displaced).await {
            Ok(false) => return true,
            Ok(true) => {}
            Err(error) => {
                debug!(%jid, %error, "slot read failed while a displaced binding retires");
            }
        }
        if let Ok(expired) = authority.expired_bindings(&account).await {
            if expired
                .iter()
                .any(|(full, binding)| full == jid && binding == displaced)
                && retire_expired_binding(authority, jid, displaced).await
            {
                return true;
            }
        }
        if tokio::time::Instant::now() + DISPLACED_RETIREMENT_POLL > deadline {
            return false;
        }
        tokio::time::sleep(DISPLACED_RETIREMENT_POLL).await;
    }
}

/// Record a one-time presentation for the binding `entry` carries, before
/// it is made. `true` means the caller presents.
///
/// With the connection registry as the account authority the entry's own
/// once-per-session claim is the whole decision and this returns `true`.
/// With the coordinator, the presentation is recorded against the
/// resource's stream-management session where it has one, so a resumed
/// session is not presented to again; a failed or refused record means the
/// caller does not present.
pub(super) async fn mark_presented(
    state: &WebSocketState,
    jid: &FullJid,
    entry: &ConnectionEntry,
    presentation: Presentation,
) -> bool {
    let Some(authority) = selected_authority(state) else {
        return true;
    };
    let Some(binding) = entry.binding_ref() else {
        return false;
    };
    match authority.mark_presented(jid, binding, presentation).await {
        Ok(first) => first,
        Err(error) => {
            warn!(%jid, %error, ?presentation, "a presentation could not be recorded");
            false
        }
    }
}

/// Retire the connection's binding at the coordinator, if it holds one.
/// Used where the connection gives a binding up before it ends: a failed
/// resume, and a new bind on the same socket.
pub(super) async fn retire_current(state: &WebSocketState, conn: &mut WsConnState) {
    let Some(link) = conn.account_binding.take() else {
        return;
    };
    let Some(authority) = selected_authority(state) else {
        return;
    };
    retire(&authority, &link).await;
}

async fn retire(authority: &Arc<RoutedAccountAuthority>, link: &AccountBindingLink) {
    let publisher =
        CoordinatorBindingPublisher::new(Arc::clone(authority) as Arc<dyn AccountBindingAuthority>);
    match BindingPublisher::retire(
        &publisher,
        &link.full_jid,
        &link.published,
        recovery_owned_retirement(),
    )
    .await
    {
        Ok(BindingRetirement::Retired | BindingRetirement::NotCurrent { .. }) => {}
        Ok(BindingRetirement::Refused(refusal)) => {
            warn!(jid = %link.full_jid, ?refusal, "account coordinator refused a binding retirement");
        }
        Err(error) => {
            warn!(jid = %link.full_jid, %error, "a binding retirement has an unknown outcome");
        }
    }
}

/// Report the end of the connection to the coordinator, after its teardown
/// ran: a binding whose session was detached for resumption is detached,
/// and any other binding is retired, which frees the resource.
///
/// A detach the slot refuses (the binding never recorded a
/// stream-management reference, or is not active) falls back to
/// retirement, so the slot never keeps a binding without a connection or a
/// resumable session.
pub(super) async fn finish_teardown(
    state: &WebSocketState,
    link: Option<AccountBindingLink>,
    outcome: ConnectionShutdownOutcome,
) {
    let (Some(link), Some(authority)) = (link, selected_authority(state)) else {
        return;
    };
    if outcome == ConnectionShutdownOutcome::Detached && detach(&authority, &link).await {
        return;
    }
    retire(&authority, &link).await;
}

/// Record that the binding lost its transport and its session stays
/// resumable. Returns whether the slot holds it as detached.
async fn detach(authority: &RoutedAccountAuthority, link: &AccountBindingLink) -> bool {
    let binding = &link.published.binding_ref;
    let status = match authority.slot_status(&link.full_jid).await {
        Ok(status) => status,
        Err(error) => {
            warn!(jid = %link.full_jid, %error, "slot read failed before a detach");
            return false;
        }
    };
    match &status.binding {
        Some((current, AccountResourceBindingState::Detached)) if current == binding => {
            return true
        }
        Some((current, AccountResourceBindingState::Active)) if current == binding => {}
        _ => return false,
    }
    let evidence = BindingEvidence::for_binding(binding);
    let operation = AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::Detach {
            claim_identity: status.claim_identity,
            expected_revision: status.revision,
            binding_ref: binding.clone(),
            endpoint: EndpointRetirementEvidence::Unpublished {
                publication: EndpointPublicationReceipt::observed(evidence.clone()),
                removed: evidence,
            },
            custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        },
    );
    match authority.execute(&link.full_jid, operation).await {
        Ok(reply) if committed(&reply) => true,
        Ok(reply) => {
            warn!(jid = %link.full_jid, ?reply, "account coordinator refused a detach");
            false
        }
        Err(error) => {
            warn!(jid = %link.full_jid, %error, "a detach has an unknown outcome");
            false
        }
    }
}
