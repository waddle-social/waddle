use std::time::Duration;

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
    GrantOperationId, GrantSubject, HostGrantGate, ScopeKey, SpaceDeletionAttempt,
    SpaceDeletionAttemptOutcome, SpaceDeletionDeliveryKind,
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

/// A missing observation leaves settlement to the persistent staging phase.
pub(crate) async fn offer(
    state: &WebSocketState,
    attempt: &SpaceDeletionAttempt,
) -> Option<SpaceDeletionAttemptOutcome> {
    use SpaceDeletionAttemptOutcome::{Ambiguous, Custody, Refused, Retry};

    if attempt.kind != SpaceDeletionDeliveryKind::Local {
        return Some(Refused);
    }
    let account = attempt.subject.jid();
    let selection = state
        .deps
        .app_state
        .account_authority()
        .headline_binding(account, None)
        .await;
    let (resource, binding) = match selection {
        Ok(Some(selection)) => selection,
        Ok(None) => return Some(Refused),
        Err(_) => return Some(Retry),
    };
    let target = LocalSpaceDeletionTarget {
        service: attempt.key.service().clone(),
        node: attempt.key.node().to_owned(),
        operation: attempt.record.operation.as_uuid(),
        resource,
        binding,
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
        .load_local_space_deletion_attempt(&key, operation, &subject, target.token)
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
        HostGrantGate::local_space_deletion_event(&key, &target.resource, &attempt.record)
    else {
        return retry;
    };
    let mut notice = build_pubsub_delete_event(
        &Jid::from(key.service().clone()),
        &Jid::from(target.resource.clone()),
        key.node(),
    );
    notice.id = Some(Id(target.operation.simple().to_string()));
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
