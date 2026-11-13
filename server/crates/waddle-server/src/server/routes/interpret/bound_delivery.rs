//! Full-JID delivery to the binding the account coordinator names.
//!
//! The coordinator names the exact binding that holds a resource. Delivery
//! hands a stanza only to the endpoint that carries that binding:
//!
//! - **This node hosts it.** The connection registry is the transport
//!   index: the stanza goes to the local entry whose `BindingRef` is the
//!   coordinator's. An entry that carries any other binding of the same
//!   full JID is refused, so a stanza selected for one binding can never
//!   reach its replacement.
//! - **Another node hosts it.** The stanza travels as a bound direct
//!   request addressed to that node (`IngressAuthority::commit_bound_request`),
//!   which hands it to the same exact binding.
//! - **No active binding.** The resource may be detached: the stanza takes
//!   the detached stream-management path, as it does when a registry entry
//!   is missing.
//!
//! Nothing here reads the `UserActor` resource map or the remote channel
//! mirrors. Interpreter dependencies without the WebSocket route state
//! reach no coordinator: every function then returns `None` and the caller
//! keeps its transport path.

use waddle_xmpp::ownership::{BindingRef, NodeIdentity};
use waddle_xmpp::registry::{BroadcastOutcome, DeliveryKind, OutboundStanza};

use super::routing::{
    deliver_to_detached_at, detached_after_routing_failure, DROPPED_FULL_RETRY_DELAYS,
};
use super::{debug, warn, Deps, FullJidDeliveryOutcome, Stanza};

/// Where the coordinator's binding of a resource is hosted.
enum BindingHost {
    /// This node, or a process without a cluster identity.
    Local,
    /// Another node incarnation.
    Remote(NodeIdentity),
}

fn binding_host(deps: &Deps<'_>, target: &jid::FullJid, binding: &BindingRef) -> BindingHost {
    if deps.connection_registry.hosts_binding(target, binding) {
        return BindingHost::Local;
    }
    let Some(node) = binding.connection_ref().node_lease().clustered_node() else {
        return BindingHost::Local;
    };
    let me = deps.web_socket_state.and_then(|state| {
        state
            .deps
            .app_state
            .clustering_claims
            .node_identity
            .as_ref()
            .map(|identity| identity.current())
    });
    match me {
        Some(me) if me != node => BindingHost::Remote(node),
        _ => BindingHost::Local,
    }
}

/// How far one bound delivery may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoundReach {
    /// A binding another node hosts is reached by a bound request. Without
    /// it such a binding is `Unavailable` here: the caller has already
    /// offered the stanza to the remote path.
    pub(crate) remote: bool,
    /// A stanza no live endpoint took goes to the resource's detached
    /// session. Without it the caller handles a missing or detached
    /// resource itself and sees `Unavailable`.
    pub(crate) detached: bool,
}

impl BoundReach {
    /// Any binding, then the detached session.
    pub(crate) const ANY: Self = Self {
        remote: true,
        detached: true,
    };
    /// Any live binding.
    pub(crate) const LIVE: Self = Self {
        remote: true,
        detached: false,
    };
    /// A binding this node hosts, then the detached session.
    pub(crate) const LOCAL: Self = Self {
        remote: false,
        detached: true,
    };
    /// A live binding this node hosts.
    pub(crate) const LOCAL_LIVE: Self = Self {
        remote: false,
        detached: false,
    };
}

/// Deliver one stanza to one resource through the coordinator's binding.
///
/// `None` only for interpreter dependencies without the WebSocket route
/// state, which reach no coordinator; the caller then keeps its transport
/// path.
pub(crate) async fn deliver_to_bound_resource(
    deps: &Deps<'_>,
    target: &jid::FullJid,
    stanza: &Stanza,
    kind: DeliveryKind,
    received_at: Option<chrono::DateTime<chrono::Utc>>,
    reach: BoundReach,
) -> Option<FullJidDeliveryOutcome> {
    let state = deps.web_socket_state?;
    let read = state
        .deps
        .app_state
        .account_authority()
        .bound_resource(target)
        .await;
    let context = deps.ingress_append_context.as_ref();
    let detached = || async {
        if reach.detached {
            deliver_to_detached_at(
                deps.sm_session_registry,
                target,
                stanza,
                context,
                received_at,
            )
            .await
            .into()
        } else {
            FullJidDeliveryOutcome::Unavailable
        }
    };
    Some(match read {
        Err(error) => {
            warn!(%error, jid = %target, "account coordinator did not name a binding; no live delivery");
            if reach.detached {
                detached_after_routing_failure(
                    deliver_to_detached_at(
                        deps.sm_session_registry,
                        target,
                        stanza,
                        context,
                        received_at,
                    )
                    .await,
                )
            } else {
                FullJidDeliveryOutcome::Unavailable
            }
        }
        Ok(None) => detached().await,
        Ok(Some(binding)) => match binding_host(deps, target, &binding) {
            BindingHost::Local => {
                deliver_to_local_binding(
                    deps,
                    target,
                    &binding,
                    stanza,
                    kind,
                    received_at,
                    !reach.detached,
                )
                .await
            }
            BindingHost::Remote(node) if reach.remote => {
                deliver_to_remote_binding(deps, target, binding, node, stanza, kind, received_at)
                    .await
            }
            BindingHost::Remote(_) => detached().await,
        },
    })
}

/// Deliver one stanza to the local connection that carries exactly
/// `binding`.
///
/// A copy an ingress transaction owns enters only through the socket's
/// acceptance boundary, and only when the named binding holds the
/// resource; without a live endpoint it is appended to the resource's
/// detached session under its obligation. Any other stanza is a
/// non-blocking send to the binding's endpoint, retried briefly while the
/// channel is full. An endpoint that is gone sends the stanza to the
/// detached session; an endpoint that carries another binding refuses it.
pub(crate) async fn deliver_to_local_binding(
    deps: &Deps<'_>,
    target: &jid::FullJid,
    binding: &BindingRef,
    stanza: &Stanza,
    kind: DeliveryKind,
    received_at: Option<chrono::DateTime<chrono::Utc>>,
    live_only: bool,
) -> FullJidDeliveryOutcome {
    let registry = deps.connection_registry;
    if let Some(context) = deps.ingress_append_context.as_ref() {
        if registry.hosts_binding(target, binding) {
            if let Some(outcome) =
                super::route_to_connection::deliver_ordered_local_copy(deps, target, stanza, kind)
                    .await
            {
                return outcome;
            }
        } else if registry.entry(target).is_some() {
            debug!(jid = %target, "keyed copy refused: another binding holds the resource");
            return FullJidDeliveryOutcome::Unavailable;
        }
        if live_only {
            return FullJidDeliveryOutcome::Unavailable;
        }
        return deliver_to_detached_at(
            deps.sm_session_registry,
            target,
            stanza,
            Some(context),
            received_at,
        )
        .await
        .into();
    }
    let mut retry_delays = DROPPED_FULL_RETRY_DELAYS.iter();
    loop {
        let mut outbound = match kind {
            DeliveryKind::PeerStanza => OutboundStanza::peer_stanza(stanza.clone()),
            _ => OutboundStanza::new(stanza.clone()),
        };
        outbound.kind = kind;
        outbound.original_receipt_at = received_at;
        match registry.try_send_outbound_to_binding(target, binding, outbound) {
            Ok(BroadcastOutcome::Delivered) => {
                if let Some(delivered) =
                    waddle_xmpp::telemetry::messages::delivered_message_kind(stanza)
                {
                    waddle_xmpp::telemetry::messages::record_delivered_message(delivered);
                }
                return FullJidDeliveryOutcome::Delivered;
            }
            Ok(BroadcastOutcome::DroppedFull) => {
                if let Some(delay) = retry_delays.next() {
                    tokio::time::sleep(*delay).await;
                    continue;
                }
                waddle_xmpp::telemetry::reliability::increment_delivery_retry_exhausted_drop();
                warn!(
                    jid = %target,
                    retries = DROPPED_FULL_RETRY_DELAYS.len(),
                    "bound delivery: recipient channel still full after bounded retries; dropped"
                );
                return FullJidDeliveryOutcome::Dropped;
            }
            Ok(BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed) => break,
            Err(waddle_xmpp::registry::StaleBinding) => {
                if registry.entry(target).is_some() {
                    debug!(jid = %target, "bound delivery refused: another binding holds the resource");
                    return FullJidDeliveryOutcome::Unavailable;
                }
                break;
            }
        }
    }
    if live_only {
        return FullJidDeliveryOutcome::Unavailable;
    }
    deliver_to_detached_at(deps.sm_session_registry, target, stanza, None, received_at)
        .await
        .into()
}

#[cfg(feature = "clustering")]
async fn deliver_to_remote_binding(
    deps: &Deps<'_>,
    target: &jid::FullJid,
    binding: BindingRef,
    node: NodeIdentity,
    stanza: &Stanza,
    kind: DeliveryKind,
    received_at: Option<chrono::DateTime<chrono::Utc>>,
) -> FullJidDeliveryOutcome {
    let Some(bridge) = deps.web_socket_state.and_then(|state| {
        state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .clone()
    }) else {
        return FullJidDeliveryOutcome::Unavailable;
    };
    bridge
        .deliver_bound(crate::clustering::route_bridge::BoundDelivery {
            target,
            binding,
            node,
            stanza,
            kind,
            ingress_append_context: deps.ingress_append_context.as_ref(),
            received_at,
        })
        .await
}

#[cfg(not(feature = "clustering"))]
async fn deliver_to_remote_binding(
    _deps: &Deps<'_>,
    _target: &jid::FullJid,
    _binding: BindingRef,
    _node: NodeIdentity,
    _stanza: &Stanza,
    _kind: DeliveryKind,
    _received_at: Option<chrono::DateTime<chrono::Utc>>,
) -> FullJidDeliveryOutcome {
    FullJidDeliveryOutcome::Unavailable
}
