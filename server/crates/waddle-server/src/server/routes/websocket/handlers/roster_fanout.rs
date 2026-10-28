//! Roster IQ fanout shared by subscription changes and roster mutations.

use std::collections::{BTreeMap, BTreeSet};

use jid::{BareJid, FullJid};
use tracing::{info, warn};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::roster::{build_roster_push, RosterItem, RosterVersion};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

#[cfg(feature = "clustering")]
use crate::server::routes::interpret::FullJidDeliveryOutcome;
use crate::server::routes::websocket::WebSocketState;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
    LocalQueueAccepted,
    #[cfg(feature = "clustering")]
    RemoteQueueAccepted,
    #[cfg(feature = "clustering")]
    RemoteBackpressure,
    #[cfg(feature = "clustering")]
    RemoteUnavailable,
    #[cfg(feature = "clustering")]
    RemoteAcceptanceUnknown,
    Unresolved,
}

impl Delivery {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalQueueAccepted => "local_queue_accepted",
            #[cfg(feature = "clustering")]
            Self::RemoteQueueAccepted => "remote_queue_accepted",
            #[cfg(feature = "clustering")]
            Self::RemoteBackpressure => "remote_backpressure",
            #[cfg(feature = "clustering")]
            Self::RemoteUnavailable => "remote_unavailable",
            #[cfg(feature = "clustering")]
            Self::RemoteAcceptanceUnknown => "remote_acceptance_unknown",
            Self::Unresolved => "unresolved",
        }
    }

    fn accepted(self) -> bool {
        match self {
            Self::LocalQueueAccepted => true,
            #[cfg(feature = "clustering")]
            Self::RemoteQueueAccepted => true,
            _ => false,
        }
    }
}

fn push_for_resource<'a>(
    pushes: &'a mut BTreeMap<FullJid, Iq>,
    resource: &FullJid,
    user: &BareJid,
    item: &RosterItem,
    version: &RosterVersion,
) -> &'a Iq {
    pushes.entry(resource.clone()).or_insert_with(|| {
        build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user,
            resource,
            item,
            Some(version),
        )
    })
}

fn report_outcome(
    resource: &FullJid,
    push: &Iq,
    version: &RosterVersion,
    outcome: Delivery,
    rejected_append: bool,
) {
    let disposition = outcome.as_str();
    let push_id = push.id();
    let message = if rejected_append {
        "Detached roster push fallback outcome"
    } else {
        "Roster push remote delivery outcome"
    };
    if outcome.accepted() {
        info!(resource = %resource, push_id, roster_version = %version, disposition, message);
    } else {
        warn!(resource = %resource, push_id, roster_version = %version, disposition, message);
    }
}

async fn try_live_resource(
    state: &WebSocketState,
    resource: &FullJid,
    push: &Iq,
) -> Option<Delivery> {
    let registry = &state.deps.protocol.connection_registry;
    if !registry.is_roster_interested(resource) {
        return None;
    }
    let stanza = Stanza::Iq(Box::new(push.clone()));
    #[cfg(feature = "clustering")]
    if let Some(bridge) = &state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
    {
        if let Some(outcome) = bridge
            .try_deliver_registered_remote_resource(
                resource,
                &stanza,
                waddle_xmpp::registry::DeliveryKind::DirectFrame,
                None,
            )
            .await
        {
            // Preserve the IQ path's bounded-pass policy. A remote reply is
            // conclusive for this attempt, not necessarily acceptance. In
            // particular, an uncertain ask must not allocate another SM copy.
            return Some(match outcome {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    Delivery::RemoteQueueAccepted
                }
                FullJidDeliveryOutcome::Dropped => Delivery::RemoteBackpressure,
                FullJidDeliveryOutcome::Unavailable => Delivery::RemoteUnavailable,
                FullJidDeliveryOutcome::MaybeCommitted => Delivery::RemoteAcceptanceUnknown,
            });
        }
    }
    // Remote cleanup can expose a fresh same-JID connection that has never
    // requested its roster. Revalidate interest after that await.
    (registry.is_roster_interested(resource)
        && registry.try_send_to(resource, stanza) == BroadcastOutcome::Delivered)
        .then_some(Delivery::LocalQueueAccepted)
}

pub(crate) async fn send_roster_push_to_resources(
    state: &WebSocketState,
    user: &BareJid,
    item: &RosterItem,
    version: &RosterVersion,
    excluded: Option<&FullJid>,
) {
    let registry = &state.deps.protocol.connection_registry;
    let included = |resource: &FullJid| excluded != Some(resource);
    let mut pushes = BTreeMap::new();
    // Finished means no further attempt in this pass, not confirmed acceptance.
    let mut finished = BTreeSet::new();
    for resource in registry.roster_interested_resources_for_user(user) {
        if !included(&resource) {
            continue;
        }
        let push = push_for_resource(&mut pushes, &resource, user, item, version);
        if let Some(outcome) = try_live_resource(state, &resource, push).await {
            if outcome != Delivery::LocalQueueAccepted {
                report_outcome(&resource, push, version, outcome, false);
            }
            finished.insert(resource);
        }
    }

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(user)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(%error, %user, "Failed to list detached roster-interested resources");
            Vec::new()
        }
    };
    // The two discovery maps can theoretically expose the same resource during
    // a resume transition. A set prevents duplicate attempts in that case.
    let detached: BTreeSet<_> = detached
        .into_iter()
        .filter(|resource| included(resource) && !finished.contains(resource))
        .collect();
    let mut rejected = BTreeSet::new();
    for resource in detached {
        let push = push_for_resource(&mut pushes, &resource, user, item, version);
        let stanza = Stanza::Iq(Box::new(push.clone()));
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => {
                finished.insert(resource);
            }
            Ok(false) => {}
            Err(error) => {
                // Emit before another resource can block and cancel fanout.
                warn!(%error, resource = %resource, push_id = push.id(), roster_version = %version,
                    disposition = "pending_fallback", "Failed to record detached roster push");
                rejected.insert(resource);
            }
        }
    }

    let fallback: BTreeSet<_> = registry
        .roster_interested_resources_for_user(user)
        .into_iter()
        .chain(rejected.iter().cloned())
        .filter(|resource| included(resource) && !finished.contains(resource))
        .collect();
    let mut outcomes = BTreeMap::new();
    for resource in fallback {
        let push = push_for_resource(&mut pushes, &resource, user, item, version);
        if let Some(outcome) = try_live_resource(state, &resource, push).await {
            if !rejected.contains(&resource) && outcome != Delivery::LocalQueueAccepted {
                report_outcome(&resource, push, version, outcome, false);
            }
            outcomes.insert(resource, outcome);
        }
    }
    for resource in rejected {
        let push = push_for_resource(&mut pushes, &resource, user, item, version);
        report_outcome(
            &resource,
            push,
            version,
            outcomes
                .get(&resource)
                .copied()
                .unwrap_or(Delivery::Unresolved),
            true,
        );
    }
}
