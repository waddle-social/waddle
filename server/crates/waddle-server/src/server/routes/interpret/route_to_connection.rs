use super::*;
use xmpp_parsers::iq::Iq;

/// RFC 6121 §8.5.2.1.1 bare-JID destination selection: the candidate set and
/// priority ranking come from the actor-authoritative `UserActor` alone
/// (ADR-0017 Phase 3 Slice 9 retires the transitional Slice-1 DashMap
/// -liveness intersection filter).
///
/// # Candidate + ranking source: the authoritative actor, alone
///
/// Tier 1 is the actor's own RFC-ranked `SelectRoutableResources` (available,
/// non-negative-priority resources, collapsed to the max-priority tie set).
/// Tier 2 — only when tier 1 selects nothing — is every connected resource
/// (`GetResources`), reproducing the legacy two-call behaviour for clients
/// that bind but defer `<presence/>`. There is no DashMap read here at all:
/// the actor is the sole source, and the Slice-1 liveness intersection
/// (`is_connected` filter over each tier before/after ranking) is gone.
///
/// # Self-healing replaces the liveness filter
///
/// The liveness filter existed to protect against a *stale extra*: a resource
/// whose DashMap entry was removed at teardown but whose actor entry a
/// lagging best-effort unregister `tell` had not yet reaped. Selecting it
/// would have hand it to delivery, which would find the entry stale.
/// Now that delivery (`deliver_peer_to_full` / `deliver_direct_to_full`,
/// Slice 2) already evicts a closed-channel entry on first touch
/// (`BroadcastOutcome::DroppedClosed` → `try_deliver`'s `remove_resource`)
/// and routes to the detached XEP-0198 buffer, a stale extra self-heals at
/// delivery time instead of being filtered out at selection time — the
/// eviction the Slice-1 filter existed to pre-empt now happens naturally on
/// the very next send attempt. The one gap self-healing does NOT close by
/// itself is a bare-JID delivery whose *entire* selected set turns out
/// stale: with the filter, that case selected empty and fell through to the
/// offline/headless recipient pass; without it, `route_to_connection` must
/// explicitly detect "nothing actually landed" after the delivery loop runs
/// and run the headless pass itself — see the disposition tracking in
/// [`route_to_connection`] below.
///
/// On any actor error (registry / user-actor busy, wedged, or state-lost) the
/// selection degrades to empty, so the caller runs the offline/headless
/// recipient pass (archive + inbox projection): the message is persisted, not
/// lost, and the recipient catches up via MAM. `deps.user_registry` is `None`
/// only in unit-test deps that do not exercise live bare-JID delivery.
async fn select_bare_jid_live_targets(deps: &Deps<'_>, bare: &BareJid) -> Vec<jid::FullJid> {
    let Some(user_registry) = deps.user_registry else {
        return Vec::new();
    };
    let routable =
        waddle_xmpp::registry::select_routable_resources_for_user(user_registry, bare).await;
    if !routable.is_empty() {
        return routable;
    }
    // Tier 2: no presence-available (non-negative) resource — fall back to
    // every connected resource (matching the legacy `get_resources_for_user`
    // behaviour). This fires both for clients that bind but defer
    // `<presence/>` (the intended case) and, as a deliberate legacy
    // divergence from strict RFC 6121 §8.5.2.1.1, for a user whose only
    // resources advertise negative priority — those are delivered rather than
    // treated as offline, preserving pre-cutover behaviour.
    waddle_xmpp::registry::get_resources_for_user(user_registry, bare).await
}

pub(super) async fn route_to_connection(
    deps: &Deps<'_>,
    jid: Jid,
    stanza: Box<Stanza>,
    recursion_depth: u8,
) -> Vec<Stanza> {
    // #229 PR12 cutover: the destination's main loop is
    // now wired (PR11) to dispatch on `DeliveryKind` and
    // run the recipient-pass pipeline for `PeerStanza`
    // values, so we deliver as `PeerStanza`. The
    // recipient's `XmppStateMachine::on_peer_stanza`
    // takes it from there: XEP-0191 incoming block,
    // XEP-0359 recipient stamp, XEP-0313 recipient-side
    // archive, XEP-0280 received-carbons, inbox
    // projection, then `SendStanza` to the wire.
    //
    // `jid` is a typed `Jid` — full or bare. Full-JID
    // targets deliver to that single resource. Bare-JID
    // targets go through RFC 6121 §8.5.2.1 resource
    // selection (highest-priority available resources;
    // tie-broken by delivering to all of them).
    //
    // Offline-recipient persistence (#229 PR15): when the
    // bare-JID target has no available resources but the
    // domain is local, run a headless recipient pass so
    // archive + inbox + incoming-blocking still execute
    // — see [`run_headless_recipient_pass`]. Cross-domain
    // bare JIDs (future s2s) drop without a recipient pass.
    //
    // Recursion guard (Codex P1 on PR #275): the depth check
    // gates the *entire* arm, not just the empty-targets
    // branch. At `recursion_depth >= MAX_RECIPIENT_PASS_DEPTH`
    // we are already inside a headless pass; any nested
    // `RouteToConnection` — full-JID or bare-JID, with or
    // without live targets — must drop, otherwise live
    // delivery would re-trigger a second recipient pass and
    // duplicate persistence. Persistence and incoming-block
    // for the offline recipient are owned by the OUTER
    // headless pass; nothing else.
    if recursion_depth >= MAX_RECIPIENT_PASS_DEPTH {
        debug!(
            target_jid = %jid,
            recursion_depth,
            "RouteToConnection: headless recipient-pass already running; \
             dropping nested route (full or bare) to prevent duplicate \
             delivery / persistence"
        );
        Vec::new()
    } else {
        // Notification activity ingest (slice 2b): when the routed
        // stanza is a typed XEP-0085 chat-state on a DM Message (type
        // chat/normal), record the sender's `(sender_bare,
        // recipient_bare)` activity. The sender is the acting party;
        // we DO NOT bump the recipient's projection here — the
        // recipient's activity column is updated only when the
        // recipient herself emits typed activity (chat-state, read
        // marker, outbound commit, presence). The recipient bare JID
        // is derived from the routing target `jid` (full or bare),
        // not from `message.to`, because the routing arm is invoked
        // for each resolved resource and the typed conversation key
        // MUST always be the bare form.
        //
        // MUC reflection also uses `RouteToConnection` (per-occupant
        // delivery from the room reflector), so we must scope to DM
        // message types — for groupchat the activity is already
        // recorded at the sender-pass `DispatchToRoom` entry, and
        // recording the per-occupant reflection here would store
        // `(room_bare, occupant_bare)` rows which is the wrong key
        // shape.
        //
        // The ingest call is gated by the recursion-depth check
        // above: nested `RouteToConnection` events that hit the
        // headless-pass guard are intentionally dropped, so they
        // MUST NOT mutate the activity projection either (Codex
        // review on PR #731).
        // Borrow the boxed Stanza via `as_ref()` so the inner Message
        // is matched by reference — `stanza` MUST remain owned for the
        // delivery branches below. Match ergonomics binds `message`
        // as `&Message` automatically.
        if let Stanza::Message(message) = stanza.as_ref() {
            if matches!(
                message.type_,
                xmpp_parsers::message::MessageType::Chat
                    | xmpp_parsers::message::MessageType::Normal,
            ) {
                if let Some(sender) = message.from.as_ref().map(|jid| jid.to_bare()) {
                    super::notification_activity_ingest::record_chat_state_activity(
                        deps,
                        &sender,
                        &jid.to_bare(),
                        message,
                    )
                    .await;
                }
            }
        }

        match jid.clone().try_into_full() {
            Ok(full) => {
                let delivery = deliver_peer_to_full(
                    deps.user_registry,
                    deps.sm_session_registry,
                    &full,
                    &stanza,
                )
                .await;
                if delivery == FullJidDeliveryOutcome::Unavailable {
                    fallback_reply_for_undeliverable_iq(stanza.as_ref())
                        .into_iter()
                        .collect()
                } else {
                    Vec::new()
                }
            }
            Err(bare) => {
                // Enumerate XEP-0198 detached-but-resumable
                // resources for the bare JID. The legacy
                // `handle_message` direct-route path queued
                // bare-JID DMs onto detached resources via
                // `record_stanza_for_detached_bound_resource`
                // so a recipient mid-resume didn't lose
                // messages; we preserve that here.
                let detached_targets: Vec<jid::FullJid> = match deps.sm_session_registry {
                    Some(sm) => {
                        sm.detached_resources_for_user(&bare)
                            .await
                            .unwrap_or_else(|error| {
                                warn!(
                                    bare_jid = %bare,
                                    %error,
                                    "RouteToConnection: failed to enumerate \
                                     detached resources for bare-JID delivery"
                                );
                                Vec::new()
                            })
                    }
                    None => Vec::new(),
                };
                // RFC 6121 §8.5.2.1.1 prefers presence-available resources for
                // bare-JID delivery; falls back to any connected resource when
                // none have emitted `<presence/>` yet (many clients defer
                // presence until after bind, and the legacy direct-route path
                // delivered without consulting presence).
                //
                // ADR-0017 Phase 3 Slice 9: the candidate set + RFC priority
                // ranking are read from the actor-authoritative `UserActor`
                // alone (tier-1 `SelectRoutableResources`, then tier-2
                // `GetResources`) — the transitional Slice-1 DashMap-liveness
                // intersection is retired. A stale extra self-heals via
                // `TrySendPeer`/`TrySendDirect` → `DroppedClosed` eviction at
                // delivery time instead of being filtered out here; see
                // `select_bare_jid_live_targets`'s doc comment. The headless
                // -persistence gate below covers the residual case where
                // self-healing alone would otherwise lose the message: every
                // selected target turning out stale.
                let live_targets = select_bare_jid_live_targets(deps, &bare).await;
                if live_targets.is_empty() && detached_targets.is_empty() {
                    if bare.domain().as_str() != deps.local_domain {
                        debug!(
                            bare_jid = %bare,
                            local_domain = %deps.local_domain,
                            "RouteToConnection: cross-domain bare JID with no \
                             local resources; dropping (s2s out of scope)"
                        );
                    } else {
                        run_headless_recipient_pass(deps, &bare, *stanza, recursion_depth + 1)
                            .await;
                    }
                } else {
                    // Build a set from the cached `live_targets`
                    // before iterating so we can both consume
                    // the targets for delivery and re-check
                    // membership when filtering the detached
                    // list — avoids re-querying the registry
                    // per detached resource (Copilot review on
                    // PR #276).
                    let live_set: std::collections::HashSet<jid::FullJid> =
                        live_targets.iter().cloned().collect();

                    // #1106: a bare-JID DM with live targets runs the
                    // recipient pass ONCE (shared, headless-style) and
                    // fans the single processed stanza out to every
                    // same-priority resource — instead of queueing a
                    // `PeerStanza` per resource, which ran the full
                    // recipient pass N times (N archive rows with
                    // divergent XEP-0359 stanza-ids, N inbox unread
                    // increments, and cross-resource received-carbons
                    // that XEP-0280 §6.3 forbids).
                    let is_dm_message = matches!(
                        stanza.as_ref(),
                        Stanza::Message(message)
                            if matches!(
                                message.type_,
                                xmpp_parsers::message::MessageType::Chat
                                    | xmpp_parsers::message::MessageType::Normal,
                            )
                    );
                    if is_dm_message && !live_targets.is_empty() {
                        // The carbon exclusion set is every client the
                        // original stanza is addressed to (XEP-0280 §6.3):
                        // the live delivery set PLUS the detached XEP-0198
                        // resources whose replay buffers get the processed
                        // original queued below — a detached sibling must
                        // not ALSO find a received-carbon in its buffer on
                        // resume.
                        let mut delivery_fanout = live_targets.clone();
                        delivery_fanout.extend(
                            detached_targets
                                .iter()
                                .filter(|full| !live_set.contains(*full))
                                .cloned(),
                        );
                        match run_fanout_recipient_pass(
                            deps,
                            &bare,
                            delivery_fanout,
                            (*stanza).clone(),
                            recursion_depth + 1,
                        )
                        .await
                        {
                            FanoutPassResult::Ran {
                                processed,
                                side_routes,
                            } => {
                                if let Some(processed) = processed {
                                    // Wire delivery of the ONE processed stanza
                                    // per resource as a `DeliveryKind::DirectFrame`
                                    // — the destination's main loop keeps XEP-0198
                                    // outbound accounting (`record_outbound`) but
                                    // does NOT re-run the recipient pass. The
                                    // stanza's `to` stays bare (RFC 6121
                                    // §8.5.2.1.1). ADR-0017 Slice 2: this now goes
                                    // through the authoritative actor
                                    // (`TrySendDirect`, non-blocking) via
                                    // `deliver_direct_to_full`.
                                    for full in &live_targets {
                                        deliver_direct_to_full(
                                            deps.user_registry,
                                            deps.sm_session_registry,
                                            full,
                                            &processed,
                                        )
                                        .await;
                                    }
                                    // Detached XEP-0198 targets get the
                                    // PROCESSED stanza too, so resume
                                    // replay carries the recipient
                                    // <stanza-id/> (closes the
                                    // stanza-id-parity gap documented on
                                    // the legacy path).
                                    queue_processed_for_detached(
                                        deps.sm_session_registry,
                                        detached_targets,
                                        &live_set,
                                        &processed,
                                    )
                                    .await;
                                } else {
                                    debug!(
                                        bare_jid = %bare,
                                        "RouteToConnection: shared recipient pass \
                                         produced no wire copy (blocked or halted); \
                                         dropping delivery"
                                    );
                                }
                                // Handler-generated side stanzas (XEP-0184
                                // receipt back to the sender) route at the
                                // OUTER depth — the old per-connection
                                // pass routed them from the recipient's
                                // own interpret loop at depth 0.
                                // Receipts always target the sender's
                                // full JID, so this cannot re-enter the
                                // bare-JID fan-out.
                                if !side_routes.is_empty() {
                                    let side_events: Vec<OutboundEvent> = side_routes
                                        .into_iter()
                                        .map(|(jid, stanza)| OutboundEvent::RouteToConnection {
                                            jid,
                                            stanza,
                                        })
                                        .collect();
                                    let _ = Box::pin(interpret_with_depth(
                                        side_events,
                                        deps,
                                        recursion_depth,
                                    ))
                                    .await;
                                }
                                return Vec::new();
                            }
                            FanoutPassResult::Unavailable => {
                                // Shared pass unavailable (no dispatcher
                                // in test fixtures, or blocklist load
                                // failed): fall through to the legacy
                                // per-resource PeerStanza path below —
                                // each recipient connection's bind-time
                                // blocklist snapshot keeps XEP-0191
                                // enforcement.
                            }
                        }
                    }
                    // ADR-0017 Phase 3 Slice 9 headless-persistence gate: with
                    // the Slice-1 DashMap-liveness filter retired, a target
                    // selected as "live" can still turn out stale (self-heals
                    // via `DroppedClosed` eviction — see
                    // `select_bare_jid_live_targets`'s doc comment). Track
                    // whether ANYTHING in this delivery set actually landed
                    // (a live channel or the detached replay buffer); if
                    // nothing did, the message would otherwise be silently
                    // lost, so fall back to the same offline/headless
                    // recipient pass the "both empty at selection" branch
                    // above already runs.
                    let mut any_landed = false;
                    for full in live_targets {
                        let disposition = deliver_peer_to_full(
                            deps.user_registry,
                            deps.sm_session_registry,
                            &full,
                            &stanza,
                        )
                        .await;
                        if matches!(
                            disposition,
                            FullJidDeliveryOutcome::Delivered
                                | FullJidDeliveryOutcome::QueuedDetached
                        ) {
                            any_landed = true;
                        }
                    }
                    if let Some(sm) = deps.sm_session_registry {
                        for full in detached_targets {
                            // Skip if this resource was just
                            // delivered live (race between
                            // enumeration and live-resource
                            // selection).
                            if live_set.contains(&full) {
                                continue;
                            }
                            // Known limitation: queues the
                            // pre-recipient-pass stanza into
                            // the detached XEP-0198 replay
                            // buffer. When the resource
                            // resumes, replay sends the
                            // stored XML verbatim WITHOUT
                            // running the recipient-pass
                            // chain, so the replayed message
                            // is missing the recipient-side
                            // `<stanza-id by='recipient/>`
                            // (XEP-0359 §5) and recipient-
                            // side filtering / archive /
                            // inbox effects don't fire.
                            // This matches LEGACY behaviour
                            // (which had no recipient pass
                            // at all) and is therefore not a
                            // regression. Closing the gap
                            // properly requires running the
                            // headless recipient pass per
                            // detached target and queueing
                            // its `SendStanza` output —
                            // tracked as a follow-up to
                            // #229 (Copilot review on
                            // PR #276).
                            let stanza_typed = (*stanza).clone();
                            match sm
                                .record_stanza_for_detached_bound_resource(
                                    &full,
                                    &stanza_typed,
                                    chrono::Utc::now(),
                                )
                                .await
                            {
                                Ok(true) => {
                                    any_landed = true;
                                    debug!(
                                        jid = %full,
                                        "RouteToConnection: bare-JID stanza queued \
                                         for detached XEP-0198 replay"
                                    );
                                }
                                Ok(false) => {
                                    debug!(
                                        jid = %full,
                                        "RouteToConnection: detached session expired \
                                         between enumeration and queue; dropping"
                                    );
                                }
                                Err(error) => {
                                    warn!(
                                        jid = %full,
                                        %error,
                                        "RouteToConnection: failed to record bare-JID \
                                         stanza for detached resource"
                                    );
                                }
                            }
                        }
                    }
                    if !any_landed {
                        if bare.domain().as_str() != deps.local_domain {
                            debug!(
                                bare_jid = %bare,
                                local_domain = %deps.local_domain,
                                "RouteToConnection: cross-domain bare JID; every \
                                 selected target turned out stale and none is \
                                 local, dropping (s2s out of scope)"
                            );
                        } else {
                            debug!(
                                bare_jid = %bare,
                                "RouteToConnection: every selected/detached target \
                                 turned out stale (self-healed via DroppedClosed \
                                 eviction); running the headless recipient pass so \
                                 the message is not silently lost"
                            );
                            run_headless_recipient_pass(deps, &bare, *stanza, recursion_depth + 1)
                                .await;
                        }
                    }
                }
                Vec::new()
            }
        }
    }
}

/// Synthesize the reply for a full-JID **request** IQ (`get`/`set`) that
/// could not be delivered because the addressed resource is confirmed
/// offline (#1130). Returns `None` for `result`/`error` IQs (nothing
/// expects a reply) and for non-IQ stanzas.
///
/// A Jingle `session-terminate` is special-cased: hanging up on a peer
/// who is already gone is *success*, not failure. When the terminate
/// handler tore the call down (both sides unregistered, JTIs revoked)
/// and then forwarded the terminate to a peer whose XMPP resource had
/// already vanished, bouncing `<service-unavailable/>` back would make
/// the caller's completed hangup look failed. We ack it with an empty
/// `<iq type='result'/>` instead. Every other undeliverable request IQ
/// gets a typed `cancel`/`<service-unavailable/>` that echoes the
/// original request payload per RFC 6120 §8.3.1.
fn fallback_reply_for_undeliverable_iq(stanza: &Stanza) -> Option<Stanza> {
    let Stanza::Iq(iq) = stanza else {
        return None;
    };
    let (from, to, id, payload) = match iq.as_ref() {
        Iq::Get {
            from,
            to,
            id,
            payload,
            ..
        }
        | Iq::Set {
            from,
            to,
            id,
            payload,
            ..
        } => (from.clone(), to.clone(), id.clone(), payload.clone()),
        Iq::Result { .. } | Iq::Error { .. } => return None,
    };
    if is_jingle_session_terminate(&payload) {
        // The server already completed the teardown; ack the hangup.
        return Some(Stanza::Iq(Box::new(Iq::Result {
            from: to,
            to: from,
            id,
            payload: None,
        })));
    }
    let error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ServiceUnavailable,
        "en",
        "Service unavailable at this address.",
    );
    Some(Stanza::Iq(Box::new(Iq::Error {
        from: to,
        to: from,
        id,
        error,
        // RFC 6120 §8.3.1: echo the offending request so the sender can
        // correlate which stanza failed.
        payload: Some(payload),
    })))
}

/// Whether an IQ request payload is a Jingle `session-terminate` action.
fn is_jingle_session_terminate(payload: &minidom::Element) -> bool {
    payload.is("jingle", waddle_xmpp::xep::xep0166::NS_JINGLE)
        && payload.attr("action") == Some("session-terminate")
}

/// #1106: queue the PROCESSED (recipient-stamped) stanza into the
/// detached XEP-0198 replay buffers of `detached_targets`, skipping any
/// resource that was just delivered live. Because the queued form is
/// the shared recipient pass's wire output, resume replay carries the
/// recipient-side `<stanza-id by='recipient'/>` (XEP-0359 §5) — the
/// persistence side effects already ran exactly once in the shared
/// pass, so replay is delivery-only.
async fn queue_processed_for_detached(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    detached_targets: Vec<jid::FullJid>,
    live_set: &std::collections::HashSet<jid::FullJid>,
    stanza: &Stanza,
) {
    let Some(sm) = sm_session_registry else {
        return;
    };
    for full in detached_targets {
        if live_set.contains(&full) {
            continue;
        }
        match sm
            .record_stanza_for_detached_bound_resource(&full, stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => {
                debug!(
                    jid = %full,
                    "RouteToConnection: processed DM queued for detached \
                     XEP-0198 replay"
                );
            }
            Ok(false) => {
                debug!(
                    jid = %full,
                    "RouteToConnection: detached session expired between \
                     enumeration and queue; dropping"
                );
            }
            Err(error) => {
                warn!(
                    jid = %full,
                    %error,
                    "RouteToConnection: failed to record processed DM for \
                     detached resource"
                );
            }
        }
    }
}
