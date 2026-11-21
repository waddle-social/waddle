use std::time::Duration;

use futures::{stream, StreamExt};
use jid::{FullJid, Jid};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp::ownership::{BindingRef, NodeIdentityStamp, SharedNodeIdentity};
use waddle_xmpp::registry::SpaceDeletionSocketOutcome;
use waddle_xmpp::stream_management::persistence::SmReplayPayload;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::pubsub::build_pubsub_delete_event;
use xmpp_parsers::message::Id;

use super::{transport_xml::stanza_to_xml, WebSocketState};
use crate::permissions::host_grant::{
    GrantOperationId, GrantSubject, HostGrantError, HostGrantGate, HostGrantStore, ScopeKey,
    SpaceDeletionAttempt, SpaceDeletionAttemptOutcome, SpaceDeletionBinding,
    SpaceDeletionBindingAttempt,
};

const SOCKET_COMPLETION_BOUND: Duration = Duration::from_secs(8);

/// Relay input carries identity, without XML, authority observations, or a
/// caller-selected effective audience. The socket host reloads those from
/// the canonical operation and active attempt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LocalSpaceDeletionTarget {
    service: jid::BareJid,
    node: String,
    operation: Uuid,
    resource: FullJid,
    binding: BindingRef,
    token: Uuid,
}

#[cfg(feature = "clustering")]
impl LocalSpaceDeletionTarget {
    pub(crate) fn binding(&self) -> &BindingRef {
        &self.binding
    }
}

/// Process the frozen set of live bindings for an account's deletion notice.
pub(crate) async fn deliver(
    state: &WebSocketState,
    store: &HostGrantStore,
    attempt: &SpaceDeletionAttempt,
) -> Result<(), HostGrantError> {
    if !store.local_space_deletion_bindings_frozen(attempt).await? {
        let targets = match state
            .deps
            .app_state
            .account_authority()
            .headline_bindings(attempt.subject.jid())
            .await
        {
            Ok(targets) => targets
                .into_iter()
                .map(|(resource, binding)| SpaceDeletionBinding { resource, binding })
                .collect::<Vec<_>>(),
            Err(_) => {
                store
                    .finish_space_deletion_attempt(attempt, SpaceDeletionAttemptOutcome::Retry)
                    .await?;
                return Ok(());
            }
        };
        if !store
            .freeze_local_space_deletion_bindings(attempt, &targets)
            .await?
        {
            return Ok(());
        }
    }
    let bindings = store
        .claim_local_space_deletion_bindings(attempt, 32)
        .await?;
    stream::iter(bindings)
        .for_each_concurrent(8, |binding| async move {
            if let Some(outcome) = offer_binding(state, &binding).await {
                if let Err(error) = store
                    .finish_local_space_deletion_binding_attempt(&binding, outcome)
                    .await
                {
                    tracing::warn!(%error, "could not settle Space deletion for a socket");
                }
            }
        })
        .await;
    store.settle_local_space_deletion_parent(attempt).await?;
    Ok(())
}

/// Missing observations settle according to the persisted child staging phase.
async fn offer_binding(
    state: &WebSocketState,
    attempt: &SpaceDeletionBindingAttempt,
) -> Option<SpaceDeletionAttemptOutcome> {
    use SpaceDeletionAttemptOutcome::{Ambiguous, Custody, Retry};

    let target = LocalSpaceDeletionTarget {
        service: attempt.parent.key.service().clone(),
        node: attempt.parent.key.node().to_owned(),
        operation: attempt.parent.record.operation.as_uuid(),
        resource: attempt.target.resource.clone(),
        binding: attempt.target.binding.clone(),
        token: attempt.token,
    };
    #[cfg(feature = "clustering")]
    let completion = if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge.offer_local_space_deletion(target).await
    } else {
        offer_on_socket(state, target, None).await
    };
    #[cfg(not(feature = "clustering"))]
    let completion = offer_on_socket(state, target, None).await;

    completion.map(|outcome| match outcome {
        SpaceDeletionSocketOutcome::Custody => Custody,
        SpaceDeletionSocketOutcome::RetryBeforeStage => Retry,
        SpaceDeletionSocketOutcome::Ambiguous => Ambiguous,
    })
}

pub(crate) async fn offer_on_socket(
    state: &WebSocketState,
    target: LocalSpaceDeletionTarget,
    node_fence: Option<(&SharedNodeIdentity, &NodeIdentityStamp)>,
) -> Option<SpaceDeletionSocketOutcome> {
    use SpaceDeletionSocketOutcome::RetryBeforeStage;
    let retry = Some(RetryBeforeStage);
    if target.service.as_str() != state.deps.service_domains.spaces.as_str() {
        return retry;
    }
    let Ok(key) = ScopeKey::new(target.service, target.node) else {
        return retry;
    };
    let Ok(subject) = GrantSubject::new(target.resource.to_bare()) else {
        return retry;
    };
    let gate = &state.deps.app_state.host_grants;
    let Some(store) = gate.store() else {
        return retry;
    };
    let operation = GrantOperationId::from_uuid(target.operation);
    // Another handoff may already own staging. An absent leased token says
    // nothing about the outcome of this attempt.
    let attempt = store
        .load_local_space_deletion_binding_attempt(
            &key,
            operation,
            &target.resource,
            &target.binding,
            target.token,
        )
        .await
        .ok()??;
    let selection = state
        .deps
        .app_state
        .account_authority()
        .headline_binding(subject.jid(), Some(&target.resource))
        .await;
    if !matches!(selection, Ok(Some((_, binding))) if binding == target.binding) {
        return retry;
    }
    let Some(descriptor) =
        HostGrantGate::local_space_deletion_event(&key, &target.resource, &attempt.parent.record)
    else {
        return retry;
    };
    let mut notice = build_pubsub_delete_event(
        &Jid::from(key.service().clone()),
        &Jid::from(subject.jid().clone()),
        key.node(),
    );
    notice.id = Some(Id(descriptor.id().to_owned()));
    let Ok(payload) = SmReplayPayload::from_protected_space_reply(
        stanza_to_xml(&Stanza::Message(notice)),
        descriptor,
    ) else {
        return retry;
    };
    let registry = &state.deps.protocol.connection_registry;
    let Some(binding_id) = registry
        .entry(&target.resource)
        .filter(|entry| entry.is_locally_hosted() && entry.binding_ref() == Some(&target.binding))
        .map(|entry| entry.binding_id())
    else {
        return retry;
    };
    let enqueue = || {
        registry.try_send_space_deletion_if_owner(
            &target.resource,
            binding_id,
            payload,
            target.token,
        )
    };
    let result = match node_fence {
        Some((identity, stamp)) => identity.with_current_stamp(stamp, |_| enqueue()),
        None => Some(enqueue()),
    };
    let Some(Ok(completion)) = result else {
        return retry;
    };
    tokio::time::timeout(SOCKET_COMPLETION_BOUND, completion)
        .await
        .ok()?
        .ok()
}
