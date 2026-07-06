use super::subscription::{
    recipient_blocks_sender, record_stanza_for_detached_available_resources_excluding,
    roster_storage,
};
use super::*;
use crate::server::caps_resolution::{build_caps_disco_info_query, extract_caps_payload};
use waddle_xmpp::Stanza;

pub(super) async fn handle_regular_presence_update(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: xmpp_parsers::presence::Presence,
) {
    // Defense in depth behind `parse_subscription_presence`: only the normal
    // available/unavailable forms may update state and be relayed. The
    // broadcast below forwards the stanza verbatim, so any other type leaking
    // in here would reach subscribers.
    if !matches!(
        presence.type_,
        xmpp_parsers::presence::Type::None | xmpp_parsers::presence::Type::Unavailable
    ) {
        warn!(
            jid = %sender_jid,
            presence_type = ?presence.type_,
            "Dropping non-broadcastable presence type from regular update path"
        );
        return;
    }
    let available = presence.type_ != xmpp_parsers::presence::Type::Unavailable;
    let priority: i8 = priority_to_i8(&presence.priority);
    if available {
        state
            .deps
            .protocol
            .connection_registry
            .clear_last_activity(&sender_jid.to_bare());
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(sender_jid, true, priority);
        resolve_caps_for_presence(state, sender_jid, &presence).await;
        // XEP-0160 §3 step 5 (locked Q7a/Q7d): on the first non-negative-
        // priority presence of a fresh session, drain pending_delivery
        // for the recipient. `claim_offline_flush` ensures this fires at
        // most once per session even across priority transitions.
        if priority >= 0 {
            maybe_flush_pending_delivery(state, sender_jid).await;
        }
        state
            .deps
            .protocol
            .connection_registry
            .update_presence_state(
                sender_jid,
                presence
                    .show
                    .as_ref()
                    .map(|show| show_name(show).to_string()),
                presence.statuses.values().next().cloned(),
                priority,
                // The client's extension payloads (XEP-0115 caps, XEP-0319
                // idle, ...) are stored verbatim so probe/subscription
                // delivery relays the real advertisements (issue #1101).
                presence.payloads.clone(),
            );
        for stanza in state
            .deps
            .protocol
            .connection_registry
            .pending_subscription_stanzas(&sender_jid.to_bare())
        {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(sender_jid, stanza)
                .await;
        }
    } else {
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(sender_jid, false, priority);
        state
            .deps
            .protocol
            .connection_registry
            .clear_presence_state(sender_jid);
        state
            .deps
            .protocol
            .connection_registry
            .record_last_activity(
                &sender_jid.to_bare(),
                presence.statuses.values().next().cloned(),
            );
    }
    broadcast_presence_to_subscribers(state, sender_jid, &presence).await;
}

/// XEP-0160 §3 step 5 + locked Q7a / Q7c / Q7d: on the recovering
/// session's first non-negative-priority presence, drain
/// `pending_delivery` for the user's bare JID and push each row to
/// this resource.
///
/// `ConnectionEntry::claim_offline_flush()` is a CAS that returns
/// `true` exactly once per fresh session — repeated presence updates
/// (priority transitions, status text changes) do not re-flush an
/// already-drained queue. Exception (issue #1122 follow-up): when the
/// flush defers rows on a transient MAM failure
/// (`deferred_transient > 0`), the CAS is reset so the next presence
/// update retries instead of stranding the rows until reconnect.
async fn maybe_flush_pending_delivery(state: &WebSocketState, sender_jid: &FullJid) {
    let entry = match state
        .deps
        .protocol
        .connection_registry
        .get_entry(sender_jid)
    {
        Some(entry) => entry,
        None => return,
    };
    if !entry.claim_offline_flush() {
        return;
    }
    let recipient_bare = sender_jid.to_bare();
    let resolver = crate::pending_delivery::MamArchiveResolver {
        mam_storage: std::sync::Arc::clone(&state.deps.protocol.mam_storage),
    };
    // Locked Q7b SM-ack lifecycle (issue #209): when the recovering
    // connection has an active XEP-0198 session, key claims by its
    // stream id so a subsequent `<a h>` from the same session deletes
    // exactly its acked rows. Without SM, the flush function falls
    // back to delete-on-push (no ack will ever fire).
    let sm_session_id = entry.sm_stream_id();
    let outcome = crate::pending_delivery::flush_for_resource(
        &state.deps.protocol.pending_delivery_storage,
        &state.deps.protocol.connection_registry,
        &recipient_bare,
        sender_jid,
        crate::pending_delivery::FlushContext {
            server_domain: state.deps.auth_state.xmpp_domain.as_str(),
            sm_session: sm_session_id.as_ref(),
            // XEP-0191 §2 step 4 flush-time block re-evaluation
            // (PR #360): pass live blocking storage so a recipient
            // who blocked a sender AFTER intake doesn't see queued
            // messages from that sender on reconnect.
            blocking_storage: Some(&state.deps.protocol.blocking_storage),
            archive_resolver: &resolver,
        },
    )
    .await;
    if outcome.deferred_transient > 0 {
        // Issue #1122 follow-up (R2): the flush hit a transient MAM
        // failure and released the failing row plus the rest of the
        // batch. `claim_offline_flush` is a once-per-connection CAS,
        // so without a reset those rows would wait for a full
        // reconnect — potentially forever on a long-lived session.
        // Re-open the CAS so this client's next presence update
        // re-attempts the flush (rate-limited by presence traffic —
        // no hot retry loop). Safe: this runs on the same connection
        // task that won the claim above, so no concurrent claimant
        // can race the reset.
        entry.reset_offline_flush();
    }
    if outcome.claimed > 0 {
        debug!(
            jid = %sender_jid,
            claimed = outcome.claimed,
            pushed = outcome.pushed,
            unresolved = outcome.unresolved,
            deferred_transient = outcome.deferred_transient,
            "XEP-0160 pending_delivery flush completed"
        );
    }
}

async fn broadcast_presence_to_subscribers(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: &xmpp_parsers::presence::Presence,
) {
    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let subscribers = match storage
        .get_presence_subscribers(&sender_jid.to_bare())
        .await
    {
        Ok(subscribers) => subscribers,
        Err(error) => {
            warn!(error = %error, jid = %sender_jid, "Failed to load presence subscribers");
            return;
        }
    };
    for subscriber_bare in subscribers {
        if recipient_blocks_sender(state, &sender_jid.to_bare(), &subscriber_bare).await {
            continue;
        }
        if recipient_blocks_sender(state, &subscriber_bare, &sender_jid.to_bare()).await {
            continue;
        }
        // RFC 6121 §4.4.2: relay the user's own stanza (readdressed), never a
        // rebuild — a rebuild drops extensions and would misattribute the
        // server's XEP-0115 caps to the contact (issue #1101).
        let mut relayed = presence.clone();
        relayed.from = Some(Jid::from(sender_jid.clone()));
        relayed.to = Some(Jid::from(subscriber_bare.clone()));
        let stanza = Stanza::Presence(relayed);
        let mut delivered_resources = Vec::new();
        for resource in state
            .deps
            .protocol
            .connection_registry
            .get_available_resources_for_user(&subscriber_bare)
            .into_iter()
            .map(|(jid, _)| jid)
        {
            if state
                .deps
                .protocol
                .connection_registry
                .send_to(&resource, stanza.clone())
                .await
                .is_sent()
            {
                delivered_resources.push(resource);
            }
        }
        record_stanza_for_detached_available_resources_excluding(
            state,
            &subscriber_bare,
            &stanza,
            "presence broadcast",
            &delivered_resources,
        )
        .await;
    }
}

/// XEP-0115: drive caps resolution for an inbound `<presence>`
/// carrying `<c hash node ver/>`.
///
/// - Cache hit on `(hash, ver)`: record the per-resource mapping so
///   the next PEP fan-out (PR 2) can filter by feature list.
/// - Cache miss with supported `hash`: send the resource a typed
///   disco#info IQ get with `node="<NODE>#<VER>"` per §6.2 and remember
///   the in-flight ver. If the outbound write fails (resource just
///   went offline / channel closed) the pending entry is immediately
///   evicted so the in-memory pending map cannot leak.
/// - Cache miss with unsupported `hash` (e.g. `sha-256`): per §5.4
///   step 2 the server MUST NOT cache. We additionally skip the
///   round-trip entirely — fan-out will treat this resource as
///   "caps unknown" until it next advertises with a supported algo.
async fn resolve_caps_for_presence(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: &xmpp_parsers::presence::Presence,
) {
    let Some(caps) = extract_caps_payload(presence) else {
        return;
    };
    let resolver = &state.deps.protocol.caps_resolver;
    let cache_key = waddle_xmpp::xep::xep0115::CapsCacheKey::from_caps(&caps);
    if resolver.cache().contains(&cache_key) {
        resolver.record_resource(sender_jid, cache_key);
        return;
    }
    if !crate::server::caps_resolution::is_supported_hash(&caps.hash) {
        debug!(
            jid = %sender_jid,
            hash = %caps.hash,
            "Skipping caps resolution: advertised hash algorithm not implemented (XEP-0115 §5.4 step 2)"
        );
        return;
    }
    // Dedup in-flight resolutions per (full_jid, hash, ver). Without
    // this guard, a client spamming presence updates with random
    // `ver` values (or re-advertising the same uncached ver before
    // its disco#info reply lands) could grow the pending map
    // unboundedly and amplify outbound disco#info traffic. PR #438
    // review issue (Qodo / Copilot).
    if resolver.has_pending_for(sender_jid, &caps) {
        debug!(
            jid = %sender_jid,
            hash = %caps.hash,
            ver = %caps.ver,
            "Skipping caps disco#info: a resolution is already in flight for this (jid, hash, ver)"
        );
        return;
    }
    let iq_id = format!("waddle-caps-disco-{}", uuid::Uuid::new_v4());
    let iq = build_caps_disco_info_query(
        &state.deps.auth_state.caps_server_domain,
        sender_jid,
        &caps,
        &iq_id,
    );
    resolver.begin_pending(iq_id.clone(), sender_jid.clone(), caps);
    let send_outcome = state
        .deps
        .protocol
        .connection_registry
        .send_to(sender_jid, Stanza::Iq(Box::new(iq)))
        .await;
    if !send_outcome.is_sent() {
        // PR #438 review issue #5: if the recipient already
        // disconnected between the presence-receipt and the
        // disco#info IQ write, evict the orphan pending entry
        // immediately rather than waiting for the next disconnect
        // event.
        debug!(
            jid = %sender_jid,
            iq_id = %iq_id,
            "Caps disco#info send failed; evicting orphaned pending entry"
        );
        let _ = resolver.take_pending(&iq_id);
    }
}

pub(super) fn show_name(show: &xmpp_parsers::presence::Show) -> &'static str {
    match show {
        xmpp_parsers::presence::Show::Away => "away",
        xmpp_parsers::presence::Show::Chat => "chat",
        xmpp_parsers::presence::Show::Dnd => "dnd",
        xmpp_parsers::presence::Show::Xa => "xa",
    }
}

fn priority_to_i8(priority: &xmpp_parsers::presence::Priority) -> i8 {
    // `Priority` wraps an `i8` whose inner field is crate-private; round-trip
    // through the XML element to read the value back as an integer.
    let element: minidom::Element = priority.into();
    element.text().trim().parse::<i8>().unwrap_or(0)
}

pub(super) fn show_from_name(value: &str) -> Option<xmpp_parsers::presence::Show> {
    match value {
        "away" => Some(xmpp_parsers::presence::Show::Away),
        "chat" => Some(xmpp_parsers::presence::Show::Chat),
        "dnd" => Some(xmpp_parsers::presence::Show::Dnd),
        "xa" => Some(xmpp_parsers::presence::Show::Xa),
        _ => None,
    }
}
