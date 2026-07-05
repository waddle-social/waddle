use super::*;

pub(super) async fn route_to_connection(
    registry: &ConnectionRegistry,
    deps: &Deps<'_>,
    jid: Jid,
    stanza: Box<Stanza>,
    recursion_depth: u8,
) {
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
                deliver_peer_to_full(registry, deps.sm_session_registry, &full, &stanza).await
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
                // RFC 6121 §8.5.2.1.1 prefers presence-available
                // resources for bare-JID delivery; fall back to
                // any connected resource when none have emitted
                // `<presence/>` yet. Many clients defer presence
                // until after resource binding completes, and
                // the legacy `handle_message` direct-route path
                // delivered without consulting presence. This
                // preserves that behaviour without giving up
                // RFC priority routing for clients that do use
                // presence.
                let live_targets = {
                    let priority = registry.select_routable_resources_for_user(&bare);
                    if priority.is_empty() {
                        registry.get_resources_for_user(&bare)
                    } else {
                        priority
                    }
                };
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
                        match run_fanout_recipient_pass(
                            deps,
                            &bare,
                            live_targets.clone(),
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
                                    // Wire delivery of the ONE processed
                                    // stanza per resource. `send_to`
                                    // queues a `DeliveryKind::DirectFrame`
                                    // — the destination's main loop keeps
                                    // XEP-0198 outbound accounting
                                    // (`record_outbound`) but does NOT
                                    // re-run the recipient pass. The
                                    // stanza's `to` stays bare (RFC 6121
                                    // §8.5.2.1.1 delivery of a
                                    // bare-addressed message).
                                    for full in &live_targets {
                                        match registry.send_to(full, (*processed).clone()).await {
                                            waddle_xmpp::registry::SendResult::Sent => {
                                                debug!(
                                                    jid = %full,
                                                    "RouteToConnection: processed DM \
                                                     delivered (shared recipient pass)"
                                                );
                                            }
                                            waddle_xmpp::registry::SendResult::NotConnected
                                            | waddle_xmpp::registry::SendResult::ChannelClosed => {
                                                deliver_to_detached(
                                                    deps.sm_session_registry,
                                                    full,
                                                    &processed,
                                                )
                                                .await;
                                            }
                                        }
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
                                return;
                            }
                            FanoutPassResult::DropFailClosed => {
                                return;
                            }
                            FanoutPassResult::Unavailable => {
                                // Test fixtures without a dispatcher keep
                                // the legacy per-resource PeerStanza path
                                // below.
                            }
                        }
                    }
                    for full in live_targets {
                        deliver_peer_to_full(registry, deps.sm_session_registry, &full, &stanza)
                            .await;
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
                }
            }
        }
    }
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
