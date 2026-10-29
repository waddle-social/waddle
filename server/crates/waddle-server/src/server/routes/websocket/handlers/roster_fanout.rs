//! Roster IQ fanout shared by subscription changes and roster mutations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

use futures::stream::FuturesUnordered;
use futures::StreamExt;

use jid::{BareJid, FullJid};
use tokio::sync::Semaphore;
use tracing::{info, warn};
use waddle_xmpp::registry::{BroadcastOutcome, ConnectionEntry, OutboundStanza};
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

fn try_queue_entry(
    state: &WebSocketState,
    resource: &FullJid,
    push: &Iq,
    entry: ConnectionEntry,
) -> Option<Delivery> {
    if !entry.roster_interested.load(Ordering::Relaxed) {
        return None;
    }
    let stanza = Stanza::Iq(Box::new(push.clone()));
    (state
        .deps
        .protocol
        .connection_registry
        .try_send_outbound_if_owner(
            resource,
            &entry.carbons_handle(),
            OutboundStanza::new(stanza),
        )
        == BroadcastOutcome::Delivered)
        .then_some(Delivery::LocalQueueAccepted)
}

async fn try_live_resource(
    state: &WebSocketState,
    resource: &FullJid,
    push: &Iq,
) -> Option<Delivery> {
    let registry = &state.deps.protocol.connection_registry;
    let entry = registry.entry(resource)?;
    if !entry.roster_interested.load(Ordering::Relaxed) {
        return None;
    }
    // A completed resume can leave an old remote registration behind. Prefer
    // the current real socket, including when its queue reports backpressure.
    if entry.is_locally_hosted() {
        return try_queue_entry(state, resource, push, entry);
    }
    #[cfg(feature = "clustering")]
    if let Some(bridge) = &state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
    {
        let stanza = Stanza::Iq(Box::new(push.clone()));
        if let Some(outcome) = bridge
            .try_deliver_registered_remote_resource(
                resource,
                &stanza,
                waddle_xmpp::registry::DeliveryKind::DirectFrame,
                None,
            )
            .await
        {
            // Some concludes this attempt, not necessarily acceptance. Never
            // allocate another SM copy after an uncertain remote commitment.
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
    // Cleanup may have exposed a new, uninterested same-JID replacement. Read
    // that entry's own interest flag and enqueue only under its owner token.
    try_queue_entry(state, resource, push, registry.entry(resource)?)
}

enum Attempt {
    Finished(FullJid),
    Pending,
    Rejected(FullJid),
}

async fn attempt_resource(
    state: &WebSocketState,
    resource: FullJid,
    push: Iq,
    version: &RosterVersion,
    initially_live: bool,
    append_gate: &Semaphore,
) -> Attempt {
    if initially_live {
        if let Some(outcome) = try_live_resource(state, &resource, &push).await {
            if outcome != Delivery::LocalQueueAccepted {
                report_outcome(&resource, &push, version, outcome, false);
            }
            return Attempt::Finished(resource);
        }
    }
    // A live resource may have detached during the remote await. Consult the
    // current SM boundary even when it was absent from detached discovery.
    let recorded = {
        let Ok(_permit) = append_gate.acquire().await else {
            warn!(%resource, "Roster append gate closed before delivery");
            return Attempt::Pending;
        };
        state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(
                &resource,
                &Stanza::Iq(Box::new(push.clone())),
                chrono::Utc::now(),
            )
            .await
    }; // Do not hold the DB admission permit across a remote fallback await.
    let rejected = match recorded {
        Ok(true) => return Attempt::Finished(resource),
        Ok(false) => false,
        Err(error) => {
            warn!(%error, resource = %resource, push_id = push.id(), roster_version = %version,
                disposition = "pending_fallback", "Failed to record detached roster push");
            true
        }
    };
    // Complete a resumed resource's live handoff immediately; an unrelated
    // blocked recipient must not postpone its accepted queue entry.
    if let Some(outcome) = try_live_resource(state, &resource, &push).await {
        if rejected || outcome != Delivery::LocalQueueAccepted {
            report_outcome(&resource, &push, version, outcome, rejected);
        }
        return Attempt::Finished(resource);
    }
    if rejected {
        Attempt::Rejected(resource)
    } else {
        Attempt::Pending
    }
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
    let live = registry.roster_interested_resources_for_user(user);
    let live_set: BTreeSet<_> = live.iter().cloned().collect();
    let mut pushes = BTreeMap::new();
    // Finished means no further attempt in this pass, not confirmed acceptance.
    let mut finished = BTreeSet::new();
    // Preserve the subscription path's nonblocking local delivery before any
    // remote or storage await. A forwarding mirror is not a local socket.
    for resource in &live {
        if !included(resource) {
            continue;
        }
        if let Some(entry) = registry
            .entry(resource)
            .filter(|entry| entry.is_locally_hosted())
        {
            let push = push_for_resource(&mut pushes, resource, user, item, version);
            if try_queue_entry(state, resource, push, entry).is_some() {
                finished.insert(resource.clone());
            }
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
    // Separate discovery maps can theoretically expose a JID twice during
    // resume. One pipeline per JID prevents duplicate remote/SM allocations.
    let targets: BTreeSet<_> = live
        .into_iter()
        .chain(detached)
        .filter(|resource| included(resource) && !finished.contains(resource))
        .collect();
    let append_gate = Semaphore::new(1);
    let mut attempts = FuturesUnordered::new();
    for resource in targets {
        let push = push_for_resource(&mut pushes, &resource, user, item, version).clone();
        let initially_live = live_set.contains(&resource);
        attempts.push(attempt_resource(
            state,
            resource,
            push,
            version,
            initially_live,
            &append_gate,
        ));
    }
    let mut rejected = BTreeSet::new();
    while let Some(attempt) = attempts.next().await {
        match attempt {
            Attempt::Finished(resource) => {
                finished.insert(resource);
            }
            Attempt::Rejected(resource) => {
                rejected.insert(resource);
            }
            Attempt::Pending => {}
        }
    }
    // Pick up connections published during handoff. Pure futures stay owned by
    // this fanout; cancellation drops them rather than spawning recipient tasks.
    let fallback: BTreeSet<_> = registry
        .roster_interested_resources_for_user(user)
        .into_iter()
        .chain(rejected.iter().cloned())
        .filter(|resource| included(resource) && !finished.contains(resource))
        .collect();
    let mut fallbacks = FuturesUnordered::new();
    for resource in fallback {
        let push = push_for_resource(&mut pushes, &resource, user, item, version).clone();
        fallbacks.push(async move {
            let outcome = try_live_resource(state, &resource, &push).await;
            (resource, push, outcome)
        });
    }
    while let Some((resource, push, outcome)) = fallbacks.next().await {
        let Some(outcome) = outcome else {
            continue;
        };
        let rejected_append = rejected.remove(&resource);
        if rejected_append || outcome != Delivery::LocalQueueAccepted {
            report_outcome(&resource, &push, version, outcome, rejected_append);
        }
    }
    for resource in rejected {
        let push = push_for_resource(&mut pushes, &resource, user, item, version);
        report_outcome(&resource, push, version, Delivery::Unresolved, true);
    }
}
