//! Publish-time fan-out of XEP-0060 §7.1 event notifications.
//!
//! Called from both the generic `PubSubRequest::Publish` arm and
//! `handle_spaces_publish` after `publish_item` succeeds, so §7.1
//! emission is a single seam regardless of node type (PEP, regular
//! PubSub, Spaces).
//!
//! Behavior:
//!
//! - Reads `NodeConfig` once via `pubsub_storage::get_node` to honor
//!   `deliver_payloads` (XEP-0060 §7.1.3.5) at item-construction time.
//! - Queries `pubsub_storage::list_deliverable_subscribers` — outcasts
//!   and non-`Subscribed` entries are filtered by storage.
//! - Builds the event item with `<item id='X' [publisher='Y']>...</item>`
//!   per §7.1.5 (publisher attribute only when ≠ owner).
//! - Expands subscribers per §6.1.6 (bare JID → all live resources) and
//!   §6.1.7 (full JID → that resource).
//! - Falls back to the SM detached-resource stash (XEP-0198) when the
//!   live channel is closed or the recipient is not currently
//!   connected. Bare-JID subscribers with no live resources also have
//!   their detached resources enumerated explicitly so resumable
//!   sessions still receive the event on resume.
//! - Best-effort: storage errors are logged and swallowed; subscribers
//!   catch up via `<items/>` on reconnect (RFC 6121 §8.5.2.1.4 forbids
//!   offline storage of headline messages, which is the type used by
//!   `build_pubsub_event`).
//!
//! Race window: a subscriber may unsubscribe between the deliverable-list
//! query and per-resource dispatch; the trailing event is permitted by
//! §7.1 and not worth holding a lock across fan-out to prevent.

use jid::{BareJid, FullJid, Jid};
use std::collections::HashSet;
use tracing::{info, warn};
use waddle_xmpp::pubsub::{build_pubsub_event, PubSubEvent, PubSubItem};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Stanza;

use super::super::WebSocketState;
use crate::db::actor::GetDatabase;
use crate::db::roster::DatabaseRosterStorage;

/// Dispatch a §7.1 event notification to every deliverable subscriber.
pub async fn fan_out_publish(
    state: &WebSocketState,
    owner: &BareJid,
    node: &str,
    published_item: &PubSubItem,
    item_id: &str,
    publisher: Option<&BareJid>,
) {
    let storage = &state.deps.protocol.pubsub_storage;

    let node_cfg = match storage.get_node(owner, node).await {
        Ok(Some(record)) => record.config,
        Ok(None) => {
            warn!(
                owner = %owner,
                node,
                "Fan-out skipped: node disappeared between publish and config fetch"
            );
            return;
        }
        Err(error) => {
            warn!(
                owner = %owner,
                node,
                error = %error,
                "Fan-out skipped: node config fetch failed"
            );
            return;
        }
    };

    let subscribers = match storage.list_deliverable_subscribers(owner, node).await {
        Ok(subs) => subs,
        Err(error) => {
            warn!(
                owner = %owner,
                node,
                error = %error,
                "Fan-out skipped: subscriber list fetch failed"
            );
            return;
        }
    };

    // XEP-0163 §3 fan-out is presence-driven, not subscription-driven —
    // an empty deliverable-subscriber list does NOT terminate fan-out.
    // We still need to iterate the publisher's roster for from/both
    // contacts whose CAPS advertise `<node>+notify`.

    // §7.1.5: include `publisher` only when it differs from the owner.
    let event_publisher = publisher.filter(|p| *p != owner).cloned();

    // §7.1.3.5: payload included only when deliver_payloads is true.
    let event_payload = if node_cfg.deliver_payloads {
        published_item.payload.clone()
    } else {
        None
    };

    let event_item = PubSubItem {
        id: Some(item_id.to_string()),
        publisher: event_publisher,
        payload: event_payload,
    };

    let event = PubSubEvent::new(node.to_string(), vec![event_item]);
    let from = Jid::from(owner.clone());

    let mut intended: u32 = 0;
    let mut delivered: u32 = 0;
    let mut dropped_full: u32 = 0;
    let mut dropped_closed: u32 = 0;
    let mut not_connected: u32 = 0;

    // Track resources reached via the explicit-subscribers loop so the
    // roster + CAPS phase doesn't double-deliver to the same client.
    let mut already_delivered: HashSet<FullJid> = HashSet::new();

    for sub in subscribers {
        let target_resources: Vec<FullJid> = match sub.subscriber.try_as_full() {
            Ok(full) => vec![full.clone()],
            Err(bare) => {
                // §6.1.6: notify all resources of the bare JID. Live
                // resources go through the connection registry; detached
                // (XEP-0198 resumable) sessions are enumerated explicitly
                // so events still reach them via the per-resource SM
                // stash even when the user has no live socket at publish
                // time.
                let mut all = state
                    .deps
                    .protocol
                    .connection_registry
                    .get_resources_for_user(bare);
                match state
                    .deps
                    .protocol
                    .sm_session_registry
                    .detached_resources_for_user(bare)
                    .await
                {
                    Ok(detached) => all.extend(detached),
                    Err(error) => warn!(
                        bare = %bare,
                        error = %error,
                        "Failed to enumerate detached resources for bare-JID fan-out"
                    ),
                }
                all
            }
        };

        if target_resources.is_empty() {
            // Bare-JID subscriber fully offline (no live, no resumable),
            // or a full-JID we never see live. Headline messages MUST
            // NOT go to offline storage (RFC 6121 §8.5.2.1.4); the
            // subscriber catches up via `<items/>` on reconnect.
            intended += 1;
            not_connected += 1;
            continue;
        }

        for resource in target_resources {
            intended += 1;
            already_delivered.insert(resource.clone());
            let message = build_pubsub_event(&from, &Jid::from(resource.clone()), &event);
            let stanza = Stanza::Message(message);

            match state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&resource, stanza.clone())
            {
                BroadcastOutcome::Delivered => delivered += 1,
                BroadcastOutcome::DroppedFull => dropped_full += 1,
                BroadcastOutcome::DroppedClosed => match state
                    .deps
                    .protocol
                    .sm_session_registry
                    .record_stanza_for_detached_bound_resource(
                        &resource,
                        &stanza,
                        chrono::Utc::now(),
                    )
                    .await
                {
                    Ok(true) => delivered += 1,
                    Ok(false) => dropped_closed += 1,
                    Err(error) => {
                        warn!(
                            resource = %resource,
                            error = %error,
                            "Failed to record fan-out stanza for detached resource after closed live send"
                        );
                        dropped_closed += 1;
                    }
                },
                BroadcastOutcome::NotConnected => match state
                    .deps
                    .protocol
                    .sm_session_registry
                    .record_stanza_for_detached_bound_resource(
                        &resource,
                        &stanza,
                        chrono::Utc::now(),
                    )
                    .await
                {
                    Ok(true) => delivered += 1,
                    Ok(false) => not_connected += 1,
                    Err(error) => {
                        warn!(
                            resource = %resource,
                            error = %error,
                            "Failed to record fan-out stanza for detached resource"
                        );
                        not_connected += 1;
                    }
                },
            }
        }
    }

    // XEP-0163 §3: presence-driven fan-out to roster from/both contacts
    // whose cached CAPS include `<node>+notify`. Per-resource semantics —
    // we deliver only to resources whose ver is mapped (set by PR 1's
    // caps resolver) AND whose cached features include the notify filter.
    // Resources mid-resolution (no ver mapping yet) are skipped here and
    // pick up via `send_last_published_item` on their next presence
    // broadcast per XEP-0060.
    let roster_metrics =
        roster_caps_fan_out(state, owner, node, &from, &event, &mut already_delivered).await;
    intended += roster_metrics.intended;
    delivered += roster_metrics.delivered;
    dropped_full += roster_metrics.dropped_full;
    dropped_closed += roster_metrics.dropped_closed;
    not_connected += roster_metrics.not_connected;

    debug_assert_eq!(
        intended,
        delivered + dropped_full + dropped_closed + not_connected,
        "fan-out accounting must cover every dispatch attempt exactly once"
    );

    info!(
        owner = %owner,
        node,
        intended,
        delivered,
        dropped_full,
        dropped_closed,
        not_connected,
        roster_caps_intended = roster_metrics.intended,
        roster_caps_delivered = roster_metrics.delivered,
        "PubSub publish fan-out complete"
    );
}

#[derive(Default)]
struct FanOutMetrics {
    intended: u32,
    delivered: u32,
    dropped_full: u32,
    dropped_closed: u32,
    not_connected: u32,
}

/// XEP-0163 §3 — iterate the publisher's roster from/both contacts and
/// deliver `<message><event>` to each available resource whose cached
/// CAPS include `<node>+notify`. Skips resources already reached via
/// the explicit-subscribers loop (deduped through `already_delivered`).
async fn roster_caps_fan_out(
    state: &WebSocketState,
    owner: &BareJid,
    node: &str,
    from: &Jid,
    event: &PubSubEvent,
    already_delivered: &mut HashSet<FullJid>,
) -> FanOutMetrics {
    let mut metrics = FanOutMetrics::default();

    let roster = match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
    {
        Ok(db) => DatabaseRosterStorage::new(db),
        Err(error) => {
            warn!(
                error = %error,
                owner = %owner,
                "Failed to access roster database for §3 fan-out"
            );
            return metrics;
        }
    };

    let presence_subscribers = match roster.get_presence_subscribers(owner).await {
        Ok(subs) => subs,
        Err(error) => {
            warn!(
                error = %error,
                owner = %owner,
                node,
                "Failed to load roster from/both contacts for §3 fan-out"
            );
            return metrics;
        }
    };

    if presence_subscribers.is_empty() {
        return metrics;
    }

    let notify_filter = format!("{node}+notify");

    for subscriber in presence_subscribers {
        let Ok(contact_bare) = subscriber.parse::<BareJid>() else {
            continue;
        };

        let resources = state
            .deps
            .protocol
            .connection_registry
            .get_resources_for_user(&contact_bare);

        for resource in resources {
            if already_delivered.contains(&resource) {
                continue;
            }
            // Skip resources mid-resolution — no ver mapping yet — per
            // RFC 363 PR 2 contract. They pick up via send_last_published_item.
            let Some(ver) = state
                .deps
                .protocol
                .caps_resolver
                .ver_for_resource(&resource)
            else {
                continue;
            };
            let Some(cached) = state.deps.protocol.caps_resolver.cached(&ver) else {
                continue;
            };
            let advertises_notify = cached
                .features
                .iter()
                .any(|feature| feature.0 == notify_filter);
            if !advertises_notify {
                continue;
            }

            metrics.intended += 1;
            already_delivered.insert(resource.clone());
            let message = build_pubsub_event(from, &Jid::from(resource.clone()), event);
            let stanza = Stanza::Message(message);
            match state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&resource, stanza.clone())
            {
                BroadcastOutcome::Delivered => metrics.delivered += 1,
                BroadcastOutcome::DroppedFull => metrics.dropped_full += 1,
                BroadcastOutcome::DroppedClosed => metrics.dropped_closed += 1,
                BroadcastOutcome::NotConnected => metrics.not_connected += 1,
            }
        }
    }

    metrics
}
