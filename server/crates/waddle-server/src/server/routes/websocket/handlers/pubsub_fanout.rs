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
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::DatabaseRosterStorage;

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
        let roster = roster_caps_fan_out(state, owner, &ctx, &mut already_delivered).await;
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

#[derive(Default)]
struct FanOutMetrics {
    intended: u32,
    delivered: u32,
    dropped_full: u32,
    dropped_closed: u32,
    not_connected: u32,
}

/// Constants threaded through both §3 fan-out passes.
struct CapsFanOutCtx<'a> {
    from: &'a Jid,
    event: &'a PubSubEvent,
    notify_filter: &'a str,
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
async fn roster_caps_fan_out(
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
async fn owner_self_caps_fan_out(
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
