use super::subscription::{
    recipient_blocks_sender, record_stanza_for_detached_available_resources_excluding,
    roster_storage,
};
use super::*;
use crate::server::caps_resolution::{build_caps_disco_info_query, extract_caps_payload};
use tracing::Instrument as _;
use waddle_xmpp::Stanza;

/// Handle a broadcast (undirected) presence update from a live connection.
///
/// `owner` is the connection's registry ownership token
/// (`WsConnState::registry_owner`, the carbons handle returned by
/// `register_*`). Every JID-keyed registry write below is owner-gated
/// (issue #1208): a superseded same-full-JID connection processing a late
/// presence frame must neither stamp stale presence onto the replacement's
/// entry nor consume the replacement's once-per-session claims. `None`
/// means the connection never completed registration (or its registration
/// was rolled back), so it owns no registry slot and is treated as a
/// non-owner: registry writes are skipped entirely.
pub(super) async fn handle_regular_presence_update(
    state: &WebSocketState,
    sender_jid: &FullJid,
    owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    presence: xmpp_parsers::presence::Presence,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
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
    // Issue #1208: each write re-verifies ownership INSIDE the registry call
    // (the owner-gated variants hold the connections entry guard across
    // check and write, see `update_presence_state_if_owner`). The gated
    // `update_presence_if_owner` doubles as the ownership probe for the
    // sibling writes in each branch: a refusal means this connection's slot
    // is gone or belongs to a same-JID replacement, whose own presence
    // supersedes ours — so every registry write (including the bare-JID
    // last-activity bookkeeping and the once-per-session claims) is skipped.
    let owned_availability_written = |available: bool| {
        owner.filter(|owner| {
            state
                .deps
                .protocol
                .connection_registry
                .update_presence_if_owner(sender_jid, owner, available, priority)
        })
    };
    if available {
        if let Some(owner) = owned_availability_written(true) {
            state
                .deps
                .protocol
                .connection_registry
                .clear_last_activity(&sender_jid.to_bare());
            resolve_caps_for_presence(state, sender_jid, &presence).await;
            // XEP-0160 §3 step 5 (locked Q7a/Q7d): on the first non-negative-
            // priority presence of a fresh session, drain pending_delivery
            // for the recipient. `claim_offline_flush` (a once-per-session
            // CAS) is consumed HERE, on the connection task, so the
            // claim-race semantics are unchanged; the actual row pushing is
            // handed to a spawned task below so a backlog larger than the
            // outbound mpsc can't self-deadlock this connection (issue #1220).
            let flush_plan = if priority >= 0 {
                plan_offline_flush(state, sender_jid, owner)
            } else {
                None
            };
            let presence_state_written = state
                .deps
                .protocol
                .connection_registry
                .update_presence_state_if_owner(
                    sender_jid,
                    owner,
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
            mirror_remote_presence_update(
                state,
                sender_jid,
                owner,
                true,
                priority,
                presence_state_written
                    .then(|| remote_presence_state_from_presence(&presence, priority)),
            )
            .await;
            // RFC 6121 §3.1.3: deliver queued inbound subscription requests on
            // this resource's INITIAL available presence only. The per-connection
            // CAS mirrors `claim_offline_flush` so auto-away/available flips
            // within a session never re-prompt the user (issue #1104). A subscribe
            // arriving while the user is online is still delivered directly by
            // `handle_subscription_presence`'s live path; the queue exists for the
            // next fresh session until the request is answered. Owner-gated so a
            // superseded connection cannot consume the replacement's claim.
            let first_available = state
                .deps
                .protocol
                .connection_registry
                .entry_if_owner(sender_jid, owner)
                .is_some_and(|entry| entry.claim_pending_subscribes_flush());
            let subscribe_stanzas = if first_available {
                state
                    .deps
                    .protocol
                    .connection_registry
                    .pending_subscription_stanzas(&sender_jid.to_bare())
            } else {
                Vec::new()
            };
            // The pending-subscribe delivery is the same self-send deadlock
            // class as the offline flush (it pushes into this connection's own
            // outbound mpsc), so it moves off-task too. Both CASes above were
            // consumed on the connection task; only the pushing is deferred.
            spawn_session_recovery_delivery(
                state,
                sender_jid,
                owner,
                flush_plan,
                subscribe_stanzas,
            );
        } else {
            debug!(
                jid = %sender_jid,
                "Skipping presence registry writes: connection does not own its registry slot"
            );
        }
    } else if let Some(owner) = owned_availability_written(false) {
        state
            .deps
            .protocol
            .connection_registry
            .clear_presence_state_if_owner(sender_jid, owner);
        mirror_remote_presence_update(state, sender_jid, owner, false, priority, None).await;
        state
            .deps
            .protocol
            .connection_registry
            .record_last_activity(
                &sender_jid.to_bare(),
                presence.statuses.values().next().cloned(),
            );
    } else {
        debug!(
            jid = %sender_jid,
            "Skipping unavailable-presence registry writes: connection does not own its registry slot"
        );
    }
    broadcast_presence_to_subscribers(state, sender_jid, &presence, ordered_relay_origin).await;
}

#[cfg(feature = "clustering")]
async fn mirror_remote_presence_update(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    available: bool,
    priority: i8,
    presence_state: Option<crate::clustering::route_bridge::RemotePresenceStateSnapshot>,
) {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge
            .update_remote_user_resource_if_owner(
                jid,
                owner,
                crate::clustering::route_bridge::RemoteResourceStateUpdate::Presence {
                    available,
                    priority,
                    state: presence_state,
                },
            )
            .await;
    }
}

#[cfg(not(feature = "clustering"))]
async fn mirror_remote_presence_update(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    _available: bool,
    _priority: i8,
    _presence_state: Option<()>,
) {
}

#[cfg(feature = "clustering")]
fn remote_presence_state_from_presence(
    presence: &xmpp_parsers::presence::Presence,
    priority: i8,
) -> crate::clustering::route_bridge::RemotePresenceStateSnapshot {
    crate::clustering::route_bridge::RemotePresenceStateSnapshot {
        show: presence
            .show
            .as_ref()
            .map(|show| show_name(show).to_string()),
        status: presence.statuses.values().next().cloned(),
        priority,
        payloads: presence
            .payloads
            .iter()
            .cloned()
            .map(crate::clustering::codec::RemoteElement)
            .collect(),
    }
}

#[cfg(not(feature = "clustering"))]
fn remote_presence_state_from_presence(
    _presence: &xmpp_parsers::presence::Presence,
    _priority: i8,
) {
}

/// Captured intent to run the XEP-0160 offline flush for a recovering
/// session, produced on the connection task and consumed by the spawned
/// [`spawn_session_recovery_delivery`] task (issue #1220).
struct OfflineFlushPlan {
    /// The recovering connection's XEP-0198 stream id (locked Q7b SM-ack
    /// lifecycle). `None` → the flush uses the non-SM delete-on-push path.
    sm_session: Option<waddle_xmpp::pending_delivery::SmSessionId>,
}

/// XEP-0160 §3 step 5 + locked Q7a / Q7c / Q7d: consume the recovering
/// session's once-per-session offline-flush CAS on the CONNECTION task and
/// capture the intent to flush.
///
/// `ConnectionEntry::claim_offline_flush()` is a CAS that returns `true`
/// exactly once per fresh session — repeated presence updates (priority
/// transitions, status text changes) do not re-flush an already-drained
/// queue. Consuming it here (not in the spawned task) keeps the claim-race
/// semantics identical to the pre-#1220 inline flush.
///
/// Owner-gated (issue #1208): the entry lookup uses `entry_if_owner` so a
/// superseded same-JID connection cannot consume the replacement's
/// once-per-session `claim_offline_flush` CAS.
fn plan_offline_flush(
    state: &WebSocketState,
    sender_jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Option<OfflineFlushPlan> {
    let entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(sender_jid, owner)?;
    if !entry.claim_offline_flush() {
        return None;
    }
    Some(OfflineFlushPlan {
        sm_session: entry.sm_stream_id(),
    })
}

/// Push a recovering session's offline backlog and queued subscription
/// requests to its resource, OFF the connection task (issue #1220).
///
/// Both the XEP-0160 flush and the RFC 6121 §3.1.3 pending-subscribe
/// delivery `.await`-push into the recipient's own 256-slot outbound mpsc,
/// whose only consumer is the connection task. Running them inline (the
/// pre-#1220 behaviour) self-deadlocks that task the moment the backlog
/// exceeds the channel capacity. Spawning with owned handles lets the
/// connection task keep draining its mpsc while this task backpressures on
/// it — and it composes with the #1219 send-window gate, which pauses that
/// drain under load without ever wedging the producer.
///
/// The CASes gating both actions were already consumed on the connection
/// task; only the pushing is deferred here. Flush precedes subscribe
/// delivery, preserving the pre-#1220 ordering.
fn spawn_session_recovery_delivery(
    state: &WebSocketState,
    resource: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    flush_plan: Option<OfflineFlushPlan>,
    subscribe_stanzas: Vec<Stanza>,
) {
    if flush_plan.is_none() && subscribe_stanzas.is_empty() {
        return;
    }
    let storage = std::sync::Arc::clone(&state.deps.protocol.pending_delivery_storage);
    let registry = std::sync::Arc::clone(&state.deps.protocol.connection_registry);
    let blocking_storage = std::sync::Arc::clone(&state.deps.protocol.blocking_storage);
    let mam_storage = std::sync::Arc::clone(&state.deps.protocol.mam_storage);
    let server_domain = state.deps.auth_state.xmpp_domain.clone();
    let recipient = resource.to_bare();
    let resource = resource.clone();
    let owner = std::sync::Arc::clone(owner);
    // Carry the current tracing span into the spawned task (issue #1220): the
    // flush is logically part of this presence-handling flow, but `tokio::spawn`
    // resets the task-local span context, which would orphan the flush's own
    // `#[instrument]` span (and its OTLP logs/trace) from the connection's
    // trace. Instrumenting the spawned future with `Span::current()` re-parents
    // it — the same idiom `server::http` uses for off-task request work.
    let flush_span = tracing::Span::current();
    tokio::spawn(
        async move {
            // If this session was superseded (same full JID re-registered) before
            // the task ran, skip the flush: the replacement runs its OWN flush via
            // its own once-per-session CAS, and pushing SM-claimed rows to a
            // replacement whose stream id differs would leave them claimed by the
            // now-dead session (self-healed later by the claim-expiry janitor, but
            // redundant and a duplicate-delivery risk). The rows stay unclaimed
            // for the replacement. This narrows the window the off-task spawn
            // opened; a replacement racing in mid-flush is still janitor-healed.
            let still_owner = registry.entry_if_owner(&resource, &owner).is_some();
            if let Some(plan) = flush_plan.filter(|_| still_owner) {
                let resolver = crate::pending_delivery::MamArchiveResolver { mam_storage };
                let outcome = crate::pending_delivery::flush_for_resource(
                    &storage,
                    &registry,
                    &recipient,
                    &resource,
                    crate::pending_delivery::FlushContext {
                        server_domain: &server_domain,
                        sm_session: plan.sm_session.as_ref(),
                        // XEP-0191 §2 step 4 flush-time block re-evaluation
                        // (PR #360): pass live blocking storage so a recipient who
                        // blocked a sender AFTER intake doesn't see queued
                        // messages from that sender on reconnect.
                        blocking_storage: Some(&blocking_storage),
                        // Owner-gate SM pushes so a same-full-JID replacement
                        // racing in mid-flush can't receive this session's
                        // SM-claimed rows (issue #1220 review).
                        owner: Some(&owner),
                        archive_resolver: &resolver,
                    },
                )
                .await;
                if outcome.batches > 0 {
                    waddle_xmpp::telemetry::reliability::add_pending_flush_batches(u64::from(
                        outcome.batches,
                    ));
                }
                if outcome.pushed > 0 {
                    waddle_xmpp::telemetry::reliability::add_pending_flush_rows_pushed(u64::from(
                        outcome.pushed,
                    ));
                }
                if outcome.deferred_transient > 0 {
                    // Issue #1122 follow-up (R2): the flush hit a transient MAM
                    // failure and released the failing row plus the rest of the
                    // batch. `claim_offline_flush` is a once-per-connection CAS,
                    // so without a reset those rows would wait for a full
                    // reconnect. Re-open the CAS (re-acquiring the entry, which
                    // may be gone if the session was superseded) so the client's
                    // next presence update re-attempts the flush.
                    if let Some(entry) = registry.entry_if_owner(&resource, &owner) {
                        entry.reset_offline_flush();
                    }
                }
                if outcome.claimed > 0 {
                    info!(
                        jid = %resource,
                        claimed = outcome.claimed,
                        batches = outcome.batches,
                        pushed = outcome.pushed,
                        unresolved = outcome.unresolved,
                        deferred_transient = outcome.deferred_transient,
                        "XEP-0160 pending_delivery flush completed"
                    );
                }
            }
            // RFC 6121 §3.1.3 queued inbound subscription requests, delivered on
            // this resource's initial available presence. Owner-gated (Qodo
            // review on PR #1234): `pending_subscription_stanzas` is
            // non-draining, so if this session was superseded the
            // replacement's own once-per-session flush delivers them —
            // rerouting them to the replacement here would double-deliver.
            for stanza in subscribe_stanzas {
                let _ = registry.send_to_if_owner(&resource, &owner, stanza).await;
            }
        }
        .instrument(flush_span),
    );
}

async fn broadcast_presence_to_subscribers(
    state: &WebSocketState,
    sender_jid: &FullJid,
    presence: &xmpp_parsers::presence::Presence,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
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
        if super::subscription::try_route_presence_to_bare_remote(
            state,
            &subscriber_bare,
            &stanza,
            ordered_relay_origin,
        )
        .await
        {
            continue;
        }
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
