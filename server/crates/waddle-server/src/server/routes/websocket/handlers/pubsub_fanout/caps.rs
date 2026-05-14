use jid::{BareJid, FullJid, Jid};
use std::collections::HashSet;
use tracing::warn;
use waddle_xmpp::pubsub::{build_pubsub_event, PubSubEvent};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Stanza;

use super::{FanOutMetrics, WebSocketState};
use crate::db::actor::GetDatabase;
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::DatabaseRosterStorage;

/// Constants threaded through both §3 fan-out passes.
pub(super) struct CapsFanOutCtx<'a> {
    pub(super) from: &'a Jid,
    pub(super) event: &'a PubSubEvent,
    pub(super) notify_filter: &'a str,
}

/// XEP-0163 §3 — iterate the publisher's roster from/both contacts and
/// deliver `<message><event>` to each available resource whose cached
/// CAPS include `<node>+notify`. Skips resources already reached via
/// the explicit-subscribers loop (deduped through `already_delivered`).
///
/// Honors XEP-0191 blocking in both directions, fail-closed: if the
/// shared DB handle is unavailable the entire roster pass is aborted
/// (failing OPEN would risk leaking PEP items to blocked contacts
/// during a transient DB outage, which §3.3 forbids).
///
/// The DB handle is acquired ONCE for both the roster query and the
/// blocking lookups (PR #439 review: avoid two `GetDatabase` actor
/// round-trips and the fail-open window between them). The
/// publisher's own blocklist is loaded ONCE and consulted via a
/// `HashSet` membership test (PR #439 review: cuts the per-roster-
/// contact query count in half on the publisher→contact direction).
pub(super) async fn roster_caps_fan_out(
    state: &WebSocketState,
    owner: &BareJid,
    ctx: &CapsFanOutCtx<'_>,
    already_delivered: &mut HashSet<FullJid>,
) -> FanOutMetrics {
    let mut metrics = FanOutMetrics::default();

    let db = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
    {
        Ok(db) => db,
        Err(error) => {
            warn!(
                error = %error,
                owner = %owner,
                notify_filter = %ctx.notify_filter,
                "Aborting §3 roster fan-out: cannot acquire DB handle; \
                 cannot honor XEP-0191 — failing closed"
            );
            return metrics;
        }
    };
    let roster = DatabaseRosterStorage::new(db.clone());
    let blocking = DatabaseBlockingStorage::new(db);

    let presence_subscribers = match roster.get_presence_subscribers(owner).await {
        Ok(subs) => subs,
        Err(error) => {
            warn!(
                error = %error,
                owner = %owner,
                notify_filter = %ctx.notify_filter,
                "Failed to load roster from/both contacts for §3 fan-out"
            );
            return metrics;
        }
    };

    if presence_subscribers.is_empty() {
        return metrics;
    }

    // Pre-load the publisher's blocklist as a typed set so the
    // owner→contact direction is checked in O(1) per roster entry,
    // not via a round-trip per check (PR #439 review). The
    // contact→owner direction still needs a per-contact query
    // because we don't have the contacts' blocklists pre-loaded.
    let owner_blocklist: HashSet<BareJid> = match blocking.list_blocked_jids(owner).await {
        Ok(list) => list.into_iter().collect(),
        Err(error) => {
            warn!(
                error = %error,
                owner = %owner,
                notify_filter = %ctx.notify_filter,
                "Aborting §3 roster fan-out: failed to load publisher's blocklist; \
                 cannot honor XEP-0191 — failing closed"
            );
            return metrics;
        }
    };

    for contact_bare in presence_subscribers {
        // XEP-0191 §2: do not deliver if either party blocked the other.
        if owner_blocklist.contains(&contact_bare)
            || is_blocked(&blocking, &contact_bare, owner).await
        {
            continue;
        }

        deliver_to_user_resources(state, &contact_bare, ctx, already_delivered, &mut metrics).await;
    }

    metrics
}

/// XEP-0163 §3.4 / PEP §4.2 — the account owner is among the
/// "appropriate subscribers" of their own publishes. Mirror to every
/// online resource of `owner` whose cached CAPS include `<node>+notify`,
/// skipping resources already reached. The publishing resource itself
/// is pre-seeded into `already_delivered` by the caller, so the owner's
/// other resources receive the headline event but the originator does
/// not see an echo of the item it just authored — matching Prosody's
/// `mod_pep` and ejabberd's `mod_pubsub`.
pub(super) async fn owner_self_caps_fan_out(
    state: &WebSocketState,
    owner: &BareJid,
    ctx: &CapsFanOutCtx<'_>,
    already_delivered: &mut HashSet<FullJid>,
) -> FanOutMetrics {
    let mut metrics = FanOutMetrics::default();
    deliver_to_user_resources(state, owner, ctx, already_delivered, &mut metrics).await;
    metrics
}

/// Deliver the event to every presence-AVAILABLE resource of `target`
/// whose cached CAPS include `notify_filter`, skipping anything in
/// `already_delivered`.
///
/// XEP-0163 §3 is presence-driven: a resource that has explicitly gone
/// `<presence type="unavailable"/>` MUST NOT receive PEP notifications
/// even if its CAPS still advertise `+notify` (PR #439 review issue
/// Qodo #2). We use the `get_available_resources_for_user` helper
/// rather than `get_resources_for_user`, which would return all
/// connected sockets including unavailable/invisible ones.
///
/// Detached XEP-0198 resumable resources are NOT enumerated here:
/// `caps_resolver.drop_resource` is called on every disconnect/expiry
/// path (including SM detach) so a detached resource never has a caps
/// mapping when the event is published, and the §3 filter would skip
/// it regardless. Once the caps lifetime is extended to span SM detach
/// windows (a separate PR), the iteration here should re-include
/// `sm_session_registry.detached_resources_for_user(target)` and route
/// through `record_stanza_for_detached_bound_resource` on
/// `DroppedClosed`/`NotConnected` outcomes.
async fn deliver_to_user_resources(
    state: &WebSocketState,
    target: &BareJid,
    ctx: &CapsFanOutCtx<'_>,
    already_delivered: &mut HashSet<FullJid>,
    metrics: &mut FanOutMetrics,
) {
    let resources: Vec<FullJid> = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(target)
        .into_iter()
        .map(|(jid, _priority)| jid)
        .collect();

    for resource in resources {
        if already_delivered.contains(&resource) {
            continue;
        }
        let Some(caps_key) = state
            .deps
            .protocol
            .caps_resolver
            .key_for_resource(&resource)
        else {
            continue;
        };
        let Some(cached) = state.deps.protocol.caps_resolver.cached(&caps_key) else {
            continue;
        };
        if !cached
            .features
            .iter()
            .any(|feature| feature.0 == ctx.notify_filter)
        {
            continue;
        }

        metrics.intended += 1;
        already_delivered.insert(resource.clone());
        let message = build_pubsub_event(ctx.from, &Jid::from(resource.clone()), ctx.event);
        let stanza = Stanza::Message(message);
        match state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, stanza.clone())
        {
            BroadcastOutcome::Delivered => metrics.delivered += 1,
            BroadcastOutcome::DroppedFull => metrics.dropped_full += 1,
            BroadcastOutcome::DroppedClosed => match state
                .deps
                .protocol
                .sm_session_registry
                .record_stanza_for_detached_bound_resource(&resource, &stanza, chrono::Utc::now())
                .await
            {
                Ok(true) => metrics.delivered += 1,
                Ok(false) => metrics.dropped_closed += 1,
                Err(error) => {
                    warn!(
                        resource = %resource,
                        error = %error,
                        "Failed to stash §3 fan-out for detached resource after closed live send"
                    );
                    metrics.dropped_closed += 1;
                }
            },
            BroadcastOutcome::NotConnected => match state
                .deps
                .protocol
                .sm_session_registry
                .record_stanza_for_detached_bound_resource(&resource, &stanza, chrono::Utc::now())
                .await
            {
                Ok(true) => metrics.delivered += 1,
                Ok(false) => metrics.not_connected += 1,
                Err(error) => {
                    warn!(
                        resource = %resource,
                        error = %error,
                        "Failed to stash §3 fan-out for not-connected detached resource"
                    );
                    metrics.not_connected += 1;
                }
            },
        }
    }
}

async fn is_blocked(
    storage: &DatabaseBlockingStorage,
    recipient: &BareJid,
    sender: &BareJid,
) -> bool {
    match storage.is_blocked(recipient, sender).await {
        Ok(blocked) => blocked,
        Err(error) => {
            warn!(
                error = %error,
                recipient = %recipient,
                sender = %sender,
                "Blocking check failed during §3 fan-out; treating as blocked to fail closed"
            );
            true
        }
    }
}
