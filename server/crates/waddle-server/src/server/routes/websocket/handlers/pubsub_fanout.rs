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
use waddle_xmpp_core::build_pubsub_retract_event;

use super::super::WebSocketState;

mod caps;

use caps::{owner_self_caps_fan_out, roster_caps_fan_out, CapsFanOutCtx};

/// Typed bundle of `fan_out_publish` inputs — keeps the public
/// signature compact (one `state` + one struct) while still letting
/// the helper consume each field by reference.
pub struct FanOutRequest<'a> {
    pub owner: &'a BareJid,
    pub node: &'a str,
    pub published_item: &'a PubSubItem,
    pub item_id: &'a str,
    pub publisher: Option<&'a BareJid>,
    /// The publishing resource's full JID, when known. Used to
    /// suppress the §3.4 owner-self echo back to the originator.
    pub publisher_full: Option<&'a FullJid>,
    /// `true` when this is a XEP-0163 PEP publish (owner is a user
    /// JID); `false` for generic PubSub / Spaces publishes where the
    /// §3 roster + owner-self passes would be a leak.
    pub is_pep: bool,
}

pub struct FanOutRetractRequest<'a> {
    pub owner: &'a BareJid,
    pub node: &'a str,
    pub item_id: &'a str,
}

/// Dispatch a §7.1 event notification to every deliverable subscriber.
///
/// `req.is_pep` distinguishes XEP-0163 PEP self-publishes (where
/// `owner` is a user JID and the §3 roster + owner-self passes apply)
/// from generic PubSub or Spaces publishes (where `owner` is a service
/// component domain and the §3 passes are skipped). Without this
/// guard, a non-PEP publish would leak to the publisher's roster
/// contacts that happen to advertise `<node>+notify` for the
/// (unrelated) generic node — a real authorization bypass.
pub async fn fan_out_publish(state: &WebSocketState, req: FanOutRequest<'_>) {
    let owner = req.owner;
    let node = req.node;
    let published_item = req.published_item;
    let item_id = req.item_id;
    let publisher = req.publisher;
    let publisher_full = req.publisher_full;
    let is_pep = req.is_pep;
    // Private PEP nodes (whitelist-access) carve out of roster + CAPS
    // fan-out even though the published payload is broadcast at the
    // pubsub layer. Whitelist `access_model` only gates the items-fetch
    // path; without this check a contact with `+notify` for the node's
    // namespace would still receive headline events. New entries here
    // require an integration test asserting roster contacts do NOT
    // receive the event (see `xep0402_bookmarks_*` and the
    // `urn:waddle:story:reads:0` fan-out test).
    let is_private_bookmarks_node = is_pep && node == waddle_xmpp::xep::xep0402::PEP_NODE;
    let is_private_story_reads_node =
        is_pep && node == waddle_xmpp_core::waddle_story_reads::PEP_NODE_WADDLE_STORY_READS;
    let is_private_pep_node = is_private_bookmarks_node || is_private_story_reads_node;
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
    // We also pre-seed the publishing resource itself (when known) so
    // the §3.4 owner-self pass doesn't echo a headline event back to
    // the same FullJid that just received the publish IQ result —
    // matches Prosody/ejabberd behavior.
    let mut already_delivered: HashSet<FullJid> = HashSet::new();
    if let Some(publisher_full) = publisher_full {
        already_delivered.insert(publisher_full.clone());
    }

    for sub in subscribers {
        if is_private_pep_node && sub.subscriber.to_bare() != *owner {
            continue;
        }

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

    // XEP-0163 §3: presence-driven fan-out. Two passes follow the
    // explicit-subscribers loop:
    //
    // - Roster pass: every roster from/both contact of the publisher,
    //   filtered by per-resource cached CAPS for `<node>+notify`. This
    //   is the §3 mainline. XEP-0191 blocking is honored both directions
    //   (publisher blocking contact, contact blocking publisher).
    //
    // - Self pass: the publisher's own *other* resources (PEP §3.4 —
    //   account owner is among the appropriate subscribers). Same +notify
    //   filter. The publishing resource itself is pre-seeded into
    //   `already_delivered` above so it does NOT receive a headline
    //   echo of the item it just authored.
    //
    // Resources mid-resolution (presence with `<c/>` arrived but the
    // disco#info round-trip hasn't completed) carry no ver mapping and
    // are skipped here. Replaying the last item to such a resource on
    // resolution-complete is `send_last_published_item` territory — not
    // yet wired through the runtime; tracked for a follow-up PR.
    let notify_filter = format!("{node}+notify");
    let ctx = CapsFanOutCtx {
        from: &from,
        event: &event,
        notify_filter: &notify_filter,
    };
    // Codex P1 review (PR #439): the §3 roster + owner-self passes
    // are XEP-0163 PEP-specific. For non-PEP publishes (generic
    // PubSub on a service component, Spaces) `owner` is a service
    // domain and the publisher's roster bears no authorization
    // relationship to the published node — running the passes there
    // would leak the event to roster contacts that advertise
    // `<node>+notify` for an unrelated node URI.
    let (roster_metrics, self_metrics) = if is_pep {
        let roster = if is_private_pep_node {
            FanOutMetrics::default()
        } else {
            roster_caps_fan_out(state, owner, &ctx, &mut already_delivered).await
        };
        let self_ = owner_self_caps_fan_out(state, owner, &ctx, &mut already_delivered).await;
        (roster, self_)
    } else {
        (FanOutMetrics::default(), FanOutMetrics::default())
    };
    intended += roster_metrics.intended + self_metrics.intended;
    delivered += roster_metrics.delivered + self_metrics.delivered;
    dropped_full += roster_metrics.dropped_full + self_metrics.dropped_full;
    dropped_closed += roster_metrics.dropped_closed + self_metrics.dropped_closed;
    not_connected += roster_metrics.not_connected + self_metrics.not_connected;

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
        self_caps_intended = self_metrics.intended,
        self_caps_delivered = self_metrics.delivered,
        "PubSub publish fan-out complete"
    );
}

pub async fn fan_out_retract(state: &WebSocketState, req: FanOutRetractRequest<'_>) {
    let storage = &state.deps.protocol.pubsub_storage;
    let node_cfg = match storage.get_node(req.owner, req.node).await {
        Ok(Some(record)) => record.config,
        Ok(None) => {
            warn!(
                owner = %req.owner,
                node = req.node,
                "Retract fan-out skipped: node disappeared between retract and config fetch"
            );
            return;
        }
        Err(error) => {
            warn!(
                owner = %req.owner,
                node = req.node,
                error = %error,
                "Retract fan-out skipped: node config fetch failed"
            );
            return;
        }
    };
    if !node_cfg.notify_retract {
        return;
    }

    let subscribers = match storage
        .list_deliverable_subscribers(req.owner, req.node)
        .await
    {
        Ok(subs) => subs,
        Err(error) => {
            warn!(
                owner = %req.owner,
                node = req.node,
                error = %error,
                "Retract fan-out skipped: subscriber list fetch failed"
            );
            return;
        }
    };

    let from = Jid::from(req.owner.clone());
    for sub in subscribers {
        let target_resources: Vec<FullJid> = match sub.subscriber.try_as_full() {
            Ok(full) => vec![full.clone()],
            Err(bare) => {
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
                        "Failed to enumerate detached resources for bare-JID retract fan-out"
                    ),
                }
                all
            }
        };

        for resource in target_resources {
            let message = build_pubsub_retract_event(
                &from,
                &Jid::from(resource.clone()),
                req.node,
                req.item_id,
            );
            let stanza = Stanza::Message(message);
            match state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&resource, stanza.clone())
            {
                BroadcastOutcome::Delivered => {}
                BroadcastOutcome::DroppedClosed | BroadcastOutcome::NotConnected => {
                    if let Err(error) = state
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
                        warn!(
                            resource = %resource,
                            error = %error,
                            "Failed to record retract fan-out stanza for detached resource"
                        );
                    }
                }
                BroadcastOutcome::DroppedFull => {}
            }
        }
    }
}

#[derive(Default)]
struct FanOutMetrics {
    intended: u32,
    delivered: u32,
    dropped_full: u32,
    dropped_closed: u32,
    not_connected: u32,
}
