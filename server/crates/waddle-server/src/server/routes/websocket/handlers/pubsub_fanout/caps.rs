use jid::{BareJid, FullJid, Jid};
use std::collections::HashSet;
use tracing::warn;
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

use super::{FanOutMetrics, WebSocketState};

/// Constants threaded through both §3 fan-out passes.
pub(super) struct CapsFanOutCtx<'a> {
    pub(super) from: &'a Jid,
    pub(super) event: &'a Message,
    pub(super) notify_filter: &'a str,
}

/// XEP-0163 §3 — iterate the publisher's roster from/both contacts and
/// deliver `<message><event>` to each available resource whose cached
/// CAPS include `<node>+notify`. Skips resources already reached via
/// the explicit-subscribers loop (deduped through `already_delivered`).
///
/// Honors XEP-0191 blocking in both directions, fail-closed: if either the
/// publisher's blocklist or the batched contact→publisher blocklist check
/// fails, the roster pass is aborted rather than leaking PEP items.
pub(super) async fn roster_caps_fan_out(
    state: &WebSocketState,
    owner: &BareJid,
    ctx: &CapsFanOutCtx<'_>,
    already_delivered: &mut HashSet<FullJid>,
) -> FanOutMetrics {
    let mut metrics = FanOutMetrics::default();

    let roster = state.deps.app_state.db_pool.roster_storage();
    let blocking = state.deps.app_state.db_pool.blocking_storage();

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

    // Pre-load the publisher's blocklist once so the owner→contact direction
    // is checked in O(1) per roster entry.
    let owner_blocklist = match blocking.load_blocklist(owner).await {
        Ok(list) => list,
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

    let unblocked_by_owner: Vec<BareJid> = presence_subscribers
        .into_iter()
        .filter(|contact_bare| !owner_blocklist.contains(contact_bare))
        .collect();
    if unblocked_by_owner.is_empty() {
        return metrics;
    }

    // Batch-evaluate the contact→publisher blocklist direction in chunked SQL
    // queries (fail-closed on DB error to honor XEP-0191 §3.3).
    let blocked_by_contact = match blocking
        .filter_blocking_recipients(&unblocked_by_owner, ctx.from)
        .await
    {
        Ok(blocked) => blocked,
        Err(error) => {
            warn!(
                error = %error,
                owner = %owner,
                notify_filter = %ctx.notify_filter,
                "Aborting §3 roster fan-out: failed to check contacts' blocklists; \
                 cannot honor XEP-0191 — failing closed"
            );
            return metrics;
        }
    };

    for contact_bare in unblocked_by_owner {
        if blocked_by_contact.contains(&contact_bare) {
            continue;
        }

        deliver_to_user_resources(
            state,
            &contact_bare,
            ctx,
            Some(&owner_blocklist),
            already_delivered,
            &mut metrics,
        )
        .await;
    }

    metrics
}

/// XEP-0163 §4 — "A service automatically sends notifications to all of
/// the account owner's connected resources (subject to notification
/// filtering)". Mirror to every presence-available resource of `owner`
/// whose cached CAPS include `<node>+notify`, skipping resources already
/// reached. The publishing resource is not excluded: it is notified like
/// any other owner resource.
pub(super) async fn owner_self_caps_fan_out(
    state: &WebSocketState,
    owner: &BareJid,
    ctx: &CapsFanOutCtx<'_>,
    already_delivered: &mut HashSet<FullJid>,
) -> FanOutMetrics {
    let mut metrics = FanOutMetrics::default();
    deliver_to_user_resources(state, owner, ctx, None, already_delivered, &mut metrics).await;
    metrics
}

/// Deliver the event to every presence-AVAILABLE resource of `target`
/// whose cached CAPS include `notify_filter`, skipping anything in
/// `already_delivered`.
///
/// XEP-0163 §3 is presence-driven: a resource that has explicitly gone
/// `<presence type="unavailable"/>` MUST NOT receive PEP notifications
/// even if its CAPS still advertise `+notify` (PR #439 review issue
/// Qodo #2). We use the `available_resources_for_user` helper
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
    blocked_by_owner: Option<&waddle_xmpp::protocol::Blocklist>,
    already_delivered: &mut HashSet<FullJid>,
    metrics: &mut FanOutMetrics,
) {
    let resources: Vec<FullJid> =
        crate::server::routes::websocket::handlers::account_reads::available_resources(
            state, target,
        )
        .await
        .into_iter()
        .map(|(jid, _priority)| jid)
        .collect();

    for resource in resources {
        if blocked_by_owner
            .is_some_and(|blocklist| blocklist.contains_jid(&Jid::from(resource.clone())))
        {
            continue;
        }
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
        let mut message = ctx.event.clone();
        message.to = Some(Jid::from(resource.clone()));
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
