use super::*;

/// Upper bound on each actor `ask` issued from the routing hot path — bounds
/// both mailbox enqueue and the handler reply so a wedged `UserRegistryActor`
/// or `UserActor` degrades bare-JID selection to the offline/headless path
/// quickly rather than stalling the interpreter loop. `UserActor` selection
/// handlers never await I/O, so a real stall is rare; the bound is the
/// backstop.
const ACTOR_ROUTE_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// RFC 6121 §8.5.2.1.1 bare-JID destination selection: the candidate set and
/// priority ranking come from the actor-authoritative `UserActor` (ADR-0017
/// Phase 1 Slice 1), filtered by DashMap liveness (ADR-0017 Phase 1 Slice 1
/// completion — see below).
///
/// # Candidate + ranking source: the authoritative actor
///
/// Reads BOTH tiers from the SAME actor: tier 1 is the available resources with
/// their priorities (`GetAvailableResources`), tier 2 — only when no available
/// non-negative resource exists — is every connected resource (`GetResources`),
/// reproducing the legacy two-call behaviour for clients that bind but defer
/// `<presence/>`. There is NO DashMap *fallback* for the candidate set: if the
/// actor is empty/errors we do not substitute a DashMap lookup.
///
/// # Liveness filter: intersect with the DashMap, then rank
///
/// Each tier's result is intersected with DashMap membership (`is_connected`).
/// Slice 2 cut delivery over to the authoritative `UserActor`
/// (`deliver_peer_to_full` / `deliver_direct_to_full`), but this filter is
/// deliberately KEPT: the best-effort, owner-gated unregister mirror can leave
/// a *stale extra* in the actor — a resource whose DashMap entry was already
/// removed at teardown but whose actor entry the mirror `tell` has not yet
/// reaped. Such a resource is still presence-available in the actor (teardown
/// does not flip the shared atomic) and, until the empty-actor reaper runs, its
/// sender channel may still be open, so the actor would happily queue to a
/// resource the legacy selection considered gone. Filtering it out here keeps
/// selection byte-for-byte identical to the legacy DashMap behaviour and keeps
/// the offline/headless pass reachable when a bare JID has no live resource
/// (council review on PR #1177).
///
/// The intersection is SOUND and introduces no false-negative: Slice 0 made
/// registration authoritative (a live resource is in the actor *and* the
/// DashMap before its own bind returns, with rollback on mirror failure), so
/// for live resources `DashMap ⊆ actor`. Intersecting the actor's per-tier
/// result with DashMap membership therefore yields exactly the DashMap live
/// set. Crucially, tier 1 takes the RFC max-priority collapse *after* the
/// liveness filter (over `GetAvailableResources`, not the actor's pre-collapsed
/// `SelectRoutableResources`), so a stale extra holding a unique top priority is
/// dropped before the max is computed — it can neither mask a live
/// lower-priority resource nor promote other lower-priority resources above it.
/// The result is byte-for-byte identical to the legacy DashMap selection, with
/// the candidate/ranking source moved to the actor. A later slice retires this
/// filter — once teardown flips the shared ownership atomic (or the empty-actor
/// reaper eagerly reaps stale extras) selection can return to the actor-side
/// `SelectRoutableResources` without it.
///
/// On any actor error (registry / user-actor busy, wedged, or state-lost) the
/// selection degrades to empty, so the caller runs the offline/headless
/// recipient pass (archive + inbox projection): the message is persisted, not
/// lost, and the recipient catches up via MAM. `deps.user_registry` is `None`
/// only in unit-test deps that do not exercise live bare-JID delivery.
async fn select_bare_jid_live_targets(
    registry: &waddle_xmpp::registry::ConnectionRegistry,
    deps: &Deps<'_>,
    bare: &BareJid,
) -> Vec<jid::FullJid> {
    let Some(user_registry) = deps.user_registry else {
        return Vec::new();
    };

    let user_actor = match user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: bare.clone(),
        })
        .mailbox_timeout(ACTOR_ROUTE_ASK_TIMEOUT)
        .reply_timeout(ACTOR_ROUTE_ASK_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => return Vec::new(),
        Err(error) => {
            warn!(
                bare_jid = %bare,
                %error,
                "route_to_connection: GetUser failed; degrading bare-JID \
                 selection to the offline/headless path"
            );
            return Vec::new();
        }
    };

    // Tier 1: RFC 6121 §8.5.2.1.1 — available, non-negative, max-priority ties.
    // We read the available resources WITH their priorities
    // (`GetAvailableResources`), intersect with DashMap liveness, and THEN
    // collapse to the max priority — the ranking is computed over the *live*
    // set, not pre-collapsed inside the actor. This is what makes the result
    // byte-for-byte identical to the legacy DashMap `select_routable_resources`
    // even in the stale-extra window: a stale extra holding a unique top
    // priority is dropped by the liveness filter BEFORE the max is taken, so it
    // can neither mask a live lower-priority resource nor promote other
    // lower-priority resources above it (council review on PR #1177). Using the
    // actor's own pre-collapsed `SelectRoutableResources` here would reintroduce
    // that distortion; it returns to use in Slice 2 once delivery (and thus the
    // liveness filter) moves to the actor.
    let available: Vec<(jid::FullJid, i8)> = match user_actor
        .ask(waddle_xmpp::registry::user_actor::GetAvailableResources)
        .mailbox_timeout(ACTOR_ROUTE_ASK_TIMEOUT)
        .reply_timeout(ACTOR_ROUTE_ASK_TIMEOUT)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(
                bare_jid = %bare,
                %error,
                "route_to_connection: GetAvailableResources failed; \
                 degrading bare-JID selection to the offline/headless path"
            );
            return Vec::new();
        }
    };
    let deliverable: Vec<(jid::FullJid, i8)> = available
        .into_iter()
        .filter(|(jid, _)| registry.is_connected(jid))
        .filter(|(_, priority)| *priority >= 0)
        .collect();
    if let Some(max_priority) = deliverable.iter().map(|(_, priority)| *priority).max() {
        return deliverable
            .into_iter()
            .filter(|(_, priority)| *priority == max_priority)
            .map(|(jid, _)| jid)
            .collect();
    }

    // Tier 2: no presence-available (non-negative) resource — fall back to
    // every connected resource (matching the legacy `get_resources_for_user`
    // behaviour), still from the SAME authoritative actor and filtered by the
    // same DashMap liveness gate. This fires both for clients that bind but
    // defer `<presence/>` (the intended case) and, as a deliberate legacy
    // divergence from strict RFC 6121 §8.5.2.1.1, for a user whose only
    // resources advertise negative priority — those are delivered rather than
    // treated as offline, preserving pre-cutover behaviour.
    match user_actor
        .ask(waddle_xmpp::registry::user_actor::GetResources)
        .mailbox_timeout(ACTOR_ROUTE_ASK_TIMEOUT)
        .reply_timeout(ACTOR_ROUTE_ASK_TIMEOUT)
        .await
    {
        Ok(resources) => filter_deliverable(registry, resources),
        Err(error) => {
            warn!(
                bare_jid = %bare,
                %error,
                "route_to_connection: GetResources failed; degrading bare-JID \
                 selection to the offline/headless path"
            );
            Vec::new()
        }
    }
}

/// Keep only the actor-selected resources that are still live in the DashMap
/// (the delivery source in this slice), dropping stale extras a lagging
/// unregister mirror has not yet reaped. See `select_bare_jid_live_targets`.
fn filter_deliverable(
    registry: &waddle_xmpp::registry::ConnectionRegistry,
    resources: Vec<jid::FullJid>,
) -> Vec<jid::FullJid> {
    resources
        .into_iter()
        .filter(|jid| registry.is_connected(jid))
        .collect()
}

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
                deliver_peer_to_full(deps.user_registry, deps.sm_session_registry, &full, &stanza)
                    .await
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
                // ADR-0017 Phase 1 Slice 1: the candidate set + RFC priority
                // ranking are now read from the actor-authoritative `UserActor`
                // (BOTH tiers, tier-1 `GetAvailableResources` with a post-filter
                // max-priority collapse, then tier-2 `GetResources`, from the
                // SAME actor — no DashMap *fallback* for candidates), then
                // intersected with DashMap liveness because delivery still reads
                // the DashMap in this slice. Tier 1 uses `GetAvailableResources`
                // rather than the actor's pre-collapsed `SelectRoutableResources`
                // precisely so the max is taken *after* the liveness filter (see
                // `select_bare_jid_live_targets`). Slice 0's authoritative
                // registration makes this intersection provably equal to the
                // legacy DashMap live set (no false-negative). See
                // docs/adrs/0017-phase1-completion-authoritative-registration.md.
                let live_targets = select_bare_jid_live_targets(registry, deps, &bare).await;
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
                                return;
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
                    for full in live_targets {
                        deliver_peer_to_full(
                            deps.user_registry,
                            deps.sm_session_registry,
                            &full,
                            &stanza,
                        )
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
