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
//!   live channel is closed or full.
//! - Best-effort: storage errors are logged and swallowed; subscribers
//!   catch up via `<items/>` on reconnect (RFC 6121 §8.5.2.1.4 forbids
//!   offline storage of headline messages, which is the type used by
//!   `build_pubsub_event`).
//!
//! Race window: a subscriber may unsubscribe between the deliverable-list
//! query and per-resource dispatch; the trailing event is permitted by
//! §7.1 and not worth holding a lock across fan-out to prevent.

use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, warn};
use waddle_xmpp::pubsub::{build_pubsub_event, PubSubEvent, PubSubItem};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Stanza;

use super::super::WebSocketState;

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

    if subscribers.is_empty() {
        debug!(owner = %owner, node, "No deliverable subscribers; nothing to fan out");
        return;
    }

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

    for sub in subscribers {
        let target_resources: Vec<FullJid> = match sub.subscriber.try_as_full() {
            Ok(full) => vec![full.clone()],
            Err(bare) => state
                .deps
                .protocol
                .connection_registry
                .get_resources_for_user(bare),
        };

        if target_resources.is_empty() {
            // Bare-JID subscriber with no live resources, or a full-JID
            // we never see live. Headline messages MUST NOT go to offline
            // storage (RFC 6121 §8.5.2.1.4), so drop on the floor;
            // subscriber catches up via `<items/>` on reconnect.
            intended += 1;
            not_connected += 1;
            continue;
        }

        for resource in target_resources {
            intended += 1;
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
                    .record_stanza_for_detached_bound_resource(&resource, &stanza)
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
                    .record_stanza_for_detached_bound_resource(&resource, &stanza)
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
        "PubSub publish fan-out complete"
    );
}
