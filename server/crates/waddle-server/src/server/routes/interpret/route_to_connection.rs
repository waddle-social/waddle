use super::*;
use std::{future::Future, pin::Pin};
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::telemetry::call::PendingCallSetupRoute;
use xmpp_parsers::iq::Iq;

type OrderedRelayDeliveryFuture<'a> =
    Pin<Box<dyn Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a>>;

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
    // the connected resources that have NOT advertised a negative priority.
    // This keeps delivering to clients that bind but defer `<presence/>`
    // (the intended legacy case: no presence yet, so no priority to honor)
    // while conforming to RFC 6121 §8.5.2.1.1's "MUST NOT deliver the
    // stanza to available resources with a negative priority" — a resource
    // that explicitly asked to be skipped for bare-JID delivery stays
    // skipped, and a user whose only resources are negative-priority falls
    // through to the offline/headless path (§8.5.2.1.1 "SHOULD store
    // offline"), closing the deliberate pre-cutover divergence (#1266
    // item 4).
    let all_connected = waddle_xmpp::registry::get_resources_for_user(user_registry, bare).await;
    if all_connected.is_empty() {
        return all_connected;
    }
    let negative: std::collections::HashSet<jid::FullJid> =
        waddle_xmpp::registry::available_resources_for_user(user_registry, bare)
            .await
            .into_iter()
            .filter(|(_, priority)| *priority < 0)
            .map(|(full, _)| full)
            .collect();
    all_connected
        .into_iter()
        .filter(|full| !negative.contains(full))
        .collect()
}

pub(crate) async fn route_to_connection(
    deps: &Deps<'_>,
    jid: Jid,
    stanza: Box<Stanza>,
    recursion_depth: u8,
    call_setup: Option<PendingCallSetupRoute>,
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
            message_id = stanza_message_id(stanza.as_ref()),
            recursion_depth,
            "RouteToConnection: headless recipient-pass already running; \
             dropping nested route (full or bare) to prevent duplicate \
             delivery / persistence"
        );
        // #1488: a dropped route never reached the peer. Unreachable
        // for real invites (the Jingle handler emits at depth 0), but
        // closing here keeps the exactly-once accounting invariant.
        if let Some(ticket) = call_setup {
            ticket.undeliverable();
        }
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
            Ok(full) => route_to_full_jid(deps, full, stanza, recursion_depth, call_setup).await,
            Err(bare) => {
                // #1488: 1:1 invites are full-JID by construction (the
                // Jingle handler rejects bare peers with bad-request),
                // so a bare-JID route cannot carry a ticket. Close
                // defensively as undeliverable — the invite verifiably
                // did not take the full-JID delivery path.
                if let Some(ticket) = call_setup {
                    ticket.undeliverable();
                }
                route_to_bare_jid(deps, bare, stanza, recursion_depth).await
            }
        }
    }
}

/// RFC 6121 §8.5.3 full-JID delivery.
///
/// DM messages (`type='chat'`/`'normal'`) get the conformant
/// no-matching-resource treatment (§8.5.3.2.1) via
/// [`route_dm_to_full_jid`]; every other stanza keeps the legacy
/// single-resource delivery (live channel → detached XEP-0198 buffer →
/// drop, with a synthesized reply for undeliverable request IQs,
/// #1130). Two message classes deliberately stay on the legacy path
/// instead of taking the §8.5.3.2.1 bare-JID fallback:
///
/// - **`groupchat`** (MUC reflections addressed to occupant full
///   JIDs): falling back to bare-JID semantics would leak room
///   traffic to resources that never joined the room.
/// - **`headline`**: RFC 6121 §8.5.2.1.1 says headline messages to a
///   bare JID with no available resources are silently ignored, and
///   Waddle's headline traffic (PEP/notification fan-out) is
///   deliberately per-resource (caps-gated), so redistributing a
///   resource-targeted headline to sibling resources would deliver
///   notifications the target never opted into.
///
/// RFC 6121 §8.5.1 still applies on this path: an undeliverable
/// message (non-error) addressed to a full JID whose LOCAL account
/// does not exist is bounced with `<service-unavailable/>` — the
/// no-account rule is unconditional on message type.
async fn route_to_full_jid(
    deps: &Deps<'_>,
    full: jid::FullJid,
    stanza: Box<Stanza>,
    recursion_depth: u8,
    call_setup: Option<PendingCallSetupRoute>,
) -> Vec<Stanza> {
    let is_dm_message = matches!(
        stanza.as_ref(),
        Stanza::Message(message)
            if matches!(
                message.type_,
                xmpp_parsers::message::MessageType::Chat
                    | xmpp_parsers::message::MessageType::Normal,
            )
    );
    if is_dm_message {
        // #1488: an invite is an IQ, never a DM message, so a ticket
        // cannot reach this branch; close defensively as delivered to
        // keep the exactly-once accounting invariant (the DM path has
        // its own fan-out/fallback dispositions).
        if let Some(ticket) = call_setup {
            ticket.delivered();
        }
        return route_dm_to_full_jid(deps, full, stanza, recursion_depth).await;
    }
    // #1488 ticket ownership: the ordered-relay path owns and closes
    // the call-setup ticket whenever it handles the delivery
    // (`Some`) — its deferred-handoff branch returns a synthetic
    // `Delivered` immediately and only learns the real disposition in
    // a spawned completion task, so the close must happen there, not
    // here. When the relay declines (`None`), the local delivery path
    // closes the ticket from its own outcome via the shared
    // [`close_call_setup_from_outcome`] mapping.
    // The clone is sound: the ticket is a shared one-shot guard, so
    // the relay path's copy and the local fallback's copy share the
    // same closed bit and at most one of them counts.
    let delivery =
        match deliver_full_jid_via_ordered_relay(deps, &full, stanza.as_ref(), call_setup.clone())
            .await
        {
            Some(outcome) => outcome,
            None => {
                let outcome =
                    deliver_peer_to_full_with_registered_remote(deps, &full, &stanza).await;
                close_call_setup_from_outcome(call_setup, outcome);
                outcome
            }
        };
    if delivery == FullJidDeliveryOutcome::Unavailable {
        // RFC 6121 §8.5.1: a message to a nonexistent LOCAL account is
        // bounced regardless of type (`groupchat` excluded — reflection
        // targets are authenticated occupants, so §8.5.1 cannot apply
        // and the existence lookup would land on the reflection
        // hot path).
        if matches!(
            stanza.as_ref(),
            Stanza::Message(message)
                if !matches!(
                    message.type_,
                    xmpp_parsers::message::MessageType::Error
                        | xmpp_parsers::message::MessageType::Groupchat,
                )
        ) {
            let bare = full.to_bare();
            if bare.domain().as_str() == deps.local_domain
                && !local_account_exists_for(deps, &bare).await
            {
                return bounce_for_nonexistent_account(stanza.as_ref(), deps.sfu);
            }
            return Vec::new();
        }
        bounce_undeliverable_iq(stanza.as_ref(), deps.sfu)
            .into_iter()
            .collect()
    } else {
        Vec::new()
    }
}

/// RFC 6121 §8.5.3 for DM messages addressed to a full JID.
///
/// 1. **Live resource** (locally or on a remote cluster node): deliver
///    as `PeerStanza` — the destination connection runs the recipient
///    pass exactly as before.
/// 2. **Detached XEP-0198 resource** (#1245): run the shared fan-out
///    recipient pass (the #1106 machinery) targeted at just this
///    resource, so the queued replay copy carries the recipient
///    `<stanza-id/>` (XEP-0359 §3), the recipient archive captures the
///    message (XEP-0313 §6.1), received-carbons reach the recipient's
///    other live resources (XEP-0280 §7), and the inbox projection
///    updates — then queue the PROCESSED stanza into the replay
///    buffer. This replaces the legacy pre-recipient-pass verbatim
///    queueing for full-JID DMs.
/// 3. **No matching resource** (#1244, RFC 6121 §8.5.3.2.1): treat the
///    stanza as if addressed to the bare JID — deliver to the user's
///    other available resources, or run the offline/headless path
///    (§8.5.2 / §8.5.1) — instead of silently dropping it.
async fn route_dm_to_full_jid(
    deps: &Deps<'_>,
    full: jid::FullJid,
    stanza: Box<Stanza>,
    recursion_depth: u8,
) -> Vec<Stanza> {
    // Cluster relay first (resource owned by a remote node): any
    // outcome other than a confirmed `Unavailable` is terminal here —
    // `Delivered`/`QueuedDetached`/`MaybeCommitted` are handled (or
    // possibly handled, which must suppress local fallback to avoid
    // duplicates), and `Dropped` is the deliberate full-channel /
    // ambiguous-failure drop the legacy path also performed.
    let relay_outcome =
        deliver_full_jid_via_ordered_relay(deps, &full, stanza.as_ref(), None).await;
    match relay_outcome {
        Some(FullJidDeliveryOutcome::Unavailable) => {
            // Confirmed offline on the owning node → §8.5.3.2.1
            // fallback below.
        }
        Some(_) => return Vec::new(),
        None => {
            // Registered-remote-resource path (clustering): same
            // outcome contract as the ordered relay.
            match deliver_registered_remote_resource(
                deps,
                &full,
                stanza.as_ref(),
                waddle_xmpp::registry::DeliveryKind::PeerStanza,
            )
            .await
            {
                Some(FullJidDeliveryOutcome::Unavailable) => {}
                Some(_) => return Vec::new(),
                None => {
                    // Local live-channel attempt with the detached
                    // fallback SUPPRESSED: a detached hit must go
                    // through the recipient-pass path below (#1245),
                    // never queue the raw pre-pass stanza.
                    // [`deliver_peer_to_live_only`] maps provably
                    // never-delivered failures (no actor, GetUser ask
                    // error, never-enqueued TrySend failures) to
                    // `Unavailable` so the detached / §8.5.3.2.1
                    // fallback below still runs for them — the legacy
                    // path routed exactly those classes to the
                    // detached buffer for the same losslessness
                    // argument. `Dropped` (full channel /
                    // maybe-enqueued failure) stays terminal to avoid
                    // double delivery.
                    let live_outcome =
                        deliver_peer_to_live_only(deps.user_registry, &full, stanza.as_ref()).await;
                    if live_outcome != FullJidDeliveryOutcome::Unavailable {
                        return Vec::new();
                    }
                }
            }
        }
    }

    let bare = full.to_bare();

    // #1245: the addressed resource is detached-but-resumable — run
    // the shared recipient pass targeted at exactly this resource and
    // queue its processed output for XEP-0198 replay.
    let is_detached = match deps.sm_session_registry {
        Some(sm) => sm
            .detached_resources_for_user(&bare)
            .await
            .unwrap_or_else(|error| {
                warn!(
                    jid = %full,
                    message_id = stanza_message_id(stanza.as_ref()),
                    %error,
                    "RouteToConnection: failed to enumerate detached \
                     resources for full-JID DM delivery"
                );
                Vec::new()
            })
            .contains(&full),
        None => false,
    };
    if is_detached {
        match run_fanout_recipient_pass(
            deps,
            &bare,
            vec![full.clone()],
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
                    let not_queued = queue_processed_for_detached(
                        deps.sm_session_registry,
                        deps.ingress_effect_capture.as_ref(),
                        vec![full.clone()],
                        &std::collections::HashSet::new(),
                        &processed,
                    )
                    .await;
                    retry_unqueued_detached_as_live(deps, not_queued, &processed).await;
                } else {
                    debug!(
                        jid = %full,
                        message_id = stanza_message_id(stanza.as_ref()),
                        "RouteToConnection: shared recipient pass produced no \
                         wire copy for detached full-JID DM (blocked or \
                         halted); dropping delivery"
                    );
                }
                route_side_stanzas(deps, side_routes, recursion_depth).await;
                return Vec::new();
            }
            FanoutPassResult::Unavailable { blocklist_failed } => {
                if blocklist_failed {
                    // Fail-closed: the XEP-0191 blocklist could not be
                    // loaded and detached replay writes the stored XML
                    // verbatim with no recipient pass, so raw queueing
                    // would let a possibly-blocked sender's message
                    // through on resume. Drop instead — mirroring the
                    // headless pass's fail-closed rule.
                    warn!(
                        jid = %full,
                        message_id = stanza_message_id(stanza.as_ref()),
                        "RouteToConnection: blocklist load failed for detached \
                         full-JID DM; dropping instead of queueing the \
                         unfiltered stanza (XEP-0191 fail-closed)"
                    );
                    return Vec::new();
                }
                // Shared pass unavailable (no dispatcher in test
                // fixtures): fall back to the legacy verbatim queueing
                // so the message is not lost while resumable.
                deliver_to_detached(deps.sm_session_registry, &full, stanza.as_ref()).await;
                return Vec::new();
            }
        }
    }

    // #1244 — RFC 6121 §8.5.3.2.1: no connected resource matches the
    // full JID, so process the stanza as if it were addressed to the
    // bare JID (§8.5.2 resource selection, offline storage via the
    // headless pass, or the §8.5.1 nonexistent-account bounce).
    debug!(
        jid = %full,
        message_id = stanza_message_id(stanza.as_ref()),
        "RouteToConnection: full-JID DM has no matching live or detached \
         resource; falling back to bare-JID delivery per RFC 6121 \
         §8.5.3.2.1"
    );
    route_to_bare_jid(deps, bare, stanza, recursion_depth).await
}

/// Handler-generated side stanzas (e.g. a XEP-0191 bounce back to the
/// sender) from a shared recipient pass route at the OUTER depth — the
/// old per-connection pass routed them from the recipient's own
/// interpret loop at depth 0. They always target the peer's JID, so
/// they cannot re-enter the fan-out that produced them.
async fn route_side_stanzas(
    deps: &Deps<'_>,
    side_routes: Vec<(Jid, Box<Stanza>)>,
    recursion_depth: u8,
) {
    if side_routes.is_empty() {
        return;
    }
    let side_events: Vec<OutboundEvent> = side_routes
        .into_iter()
        .map(|(jid, stanza)| OutboundEvent::RouteToConnection {
            jid,
            stanza,
            call_setup: None,
        })
        .collect();
    let _ = Box::pin(interpret_with_depth(side_events, deps, recursion_depth)).await;
}

/// RFC 6121 §8.5.2 bare-JID delivery (plus the §8.5.1 no-such-account
/// bounce and the offline/headless recipient pass).
async fn route_to_bare_jid(
    deps: &Deps<'_>,
    bare: BareJid,
    stanza: Box<Stanza>,
    recursion_depth: u8,
) -> Vec<Stanza> {
    {
        if let Some(delivery) =
            deliver_bare_jid_via_ordered_relay(deps, &bare, stanza.as_ref()).await
        {
            return if delivery == FullJidDeliveryOutcome::Unavailable {
                bounce_undeliverable_iq(stanza.as_ref(), deps.sfu)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
        }

        // Enumerate XEP-0198 detached-but-resumable
        // resources for the bare JID. The legacy
        // `handle_message` direct-route path queued
        // bare-JID DMs onto detached resources via
        // `record_stanza_for_detached_bound_resource`
        // so a recipient mid-resume didn't lose
        // messages; we preserve that here.
        let detached_targets: Vec<jid::FullJid> = match deps.sm_session_registry {
            Some(sm) => sm
                .detached_resources_for_user(&bare)
                .await
                .unwrap_or_else(|error| {
                    warn!(
                        bare_jid = %bare,
                        message_id = stanza_message_id(stanza.as_ref()),
                        %error,
                        "RouteToConnection: failed to enumerate \
                         detached resources for bare-JID delivery"
                    );
                    Vec::new()
                }),
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
        let mut route_fanout = live_targets.clone();
        let detached_only = detached_targets
            .iter()
            .filter(|target| !route_fanout.contains(*target))
            .cloned()
            .collect::<Vec<_>>();
        route_fanout.extend(detached_only);
        if !route_fanout.is_empty() {
            deps.capture_intent(IngressEffectIntent::RouteDirect {
                recipient: bare.clone(),
                fanout: route_fanout,
            });
        }
        if live_targets.is_empty() && detached_targets.is_empty() {
            if bare.domain().as_str() != deps.local_domain {
                debug!(
                    bare_jid = %bare,
                    local_domain = %deps.local_domain,
                    "RouteToConnection: cross-domain bare JID with no \
                     local resources; dropping (s2s out of scope)"
                );
            } else if !local_account_exists_for(deps, &bare).await {
                // #1246 — RFC 6121 §8.5.1: the domainpart
                // matches but no local account exists. A
                // message MUST be bounced with
                // <service-unavailable/> (never persisted —
                // no MAM/pending/inbox rows for arbitrary
                // never-to-exist JIDs), a request IQ gets the
                // same typed error, and presence is silently
                // ignored.
                debug!(
                    bare_jid = %bare,
                    "RouteToConnection: no local account for bare JID; \
                     bouncing with service-unavailable instead of \
                     persisting (RFC 6121 §8.5.1)"
                );
                return bounce_for_nonexistent_account(stanza.as_ref(), deps.sfu);
            } else {
                run_headless_recipient_pass(deps, &bare, *stanza, recursion_depth + 1).await;
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

            // XEP-0191 fail-closed marker for the legacy raw detached
            // queueing below: set when the shared pass was skipped
            // because the blocklist could not be loaded (replay has no
            // per-connection snapshot to fall back on).
            let mut skip_raw_detached_queueing = false;
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
            // The pass runs whenever ANY deliverable target exists —
            // including the detached-only case (live empty, detached
            // non-empty): queueing the raw pre-pass stanza there would
            // bypass recipient-side XEP-0191 filtering (and the
            // blocked-sender bounce), MAM/stanza-id stamping, and the
            // inbox projection until resume (Qodo review on this PR;
            // same rule the full-JID detached path follows).
            if is_dm_message && !(live_targets.is_empty() && detached_targets.is_empty()) {
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
                                deliver_direct_to_full_with_registered_remote(
                                    deps, full, &processed,
                                )
                                .await;
                            }
                            // Detached XEP-0198 targets get the
                            // PROCESSED stanza too, so resume
                            // replay carries the recipient
                            // <stanza-id/> (closes the
                            // stanza-id-parity gap documented on
                            // the legacy path).
                            let not_queued = queue_processed_for_detached(
                                deps.sm_session_registry,
                                deps.ingress_effect_capture.as_ref(),
                                detached_targets,
                                &live_set,
                                &processed,
                            )
                            .await;
                            retry_unqueued_detached_as_live(deps, not_queued, &processed).await;
                        } else {
                            debug!(
                                bare_jid = %bare,
                                message_id = stanza_message_id(stanza.as_ref()),
                                "RouteToConnection: shared recipient pass \
                                 produced no wire copy (blocked or halted); \
                                 dropping delivery"
                            );
                        }
                        // Handler-generated side stanzas (the
                        // XEP-0191 bounce back to a blocked
                        // sender) route at the OUTER depth — the
                        // old per-connection pass routed them
                        // from the recipient's own interpret
                        // loop at depth 0. Side routes target
                        // the peer, and error stanzas are inert
                        // in every persistence handler, so this
                        // cannot re-enter the bare-JID fan-out.
                        route_side_stanzas(deps, side_routes, recursion_depth).await;
                        return Vec::new();
                    }
                    FanoutPassResult::Unavailable { blocklist_failed } => {
                        // Shared pass unavailable (no dispatcher
                        // in test fixtures, or blocklist load
                        // failed): fall through to the legacy
                        // per-resource PeerStanza path below —
                        // each recipient connection's bind-time
                        // blocklist snapshot keeps XEP-0191
                        // enforcement for LIVE delivery. Raw
                        // detached queueing has no snapshot, so
                        // it is skipped when the blocklist load
                        // failed (fail-closed) — see below.
                        skip_raw_detached_queueing = blocklist_failed;
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
                let disposition =
                    deliver_peer_to_full_with_registered_remote(deps, &full, &stanza).await;
                if disposition.suppresses_fallback() {
                    any_landed = true;
                }
            }
            if skip_raw_detached_queueing {
                debug!(
                    bare_jid = %bare,
                    message_id = stanza_message_id(stanza.as_ref()),
                    "RouteToConnection: blocklist load failed; skipping raw \
                     detached XEP-0198 queueing (fail-closed) — live \
                     per-resource delivery above keeps its own snapshot"
                );
            } else if let Some(sm) = deps.sm_session_registry {
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
                        .record_stanza_for_detached_bound_resource_with_stream(
                            &full,
                            &stanza_typed,
                            chrono::Utc::now(),
                        )
                        .await
                    {
                        Ok(Some(stream)) => {
                            any_landed = true;
                            deps.capture_intent(IngressEffectIntent::RecipientSmAppend { stream });
                            debug!(
                                jid = %full,
                                message_id = stanza_message_id(stanza.as_ref()),
                                "RouteToConnection: bare-JID stanza queued \
                                 for detached XEP-0198 replay"
                            );
                        }
                        Ok(None) => {
                            debug!(
                                jid = %full,
                                message_id = stanza_message_id(stanza.as_ref()),
                                "RouteToConnection: detached session expired \
                                 between enumeration and queue; dropping"
                            );
                        }
                        Err(error) => {
                            warn!(
                                jid = %full,
                                message_id = stanza_message_id(stanza.as_ref()),
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
                } else if !local_account_exists_for(deps, &bare).await {
                    // #1246 residual: every selected target was a stale
                    // registry/SM leftover for an account that does not
                    // (or no longer does) exist — bounce instead of
                    // creating archive/inbox rows for it (RFC 6121
                    // §8.5.1).
                    return bounce_for_nonexistent_account(stanza.as_ref(), deps.sfu);
                } else {
                    debug!(
                        bare_jid = %bare,
                        message_id = stanza_message_id(stanza.as_ref()),
                        "RouteToConnection: every selected/detached target \
                         turned out stale (self-healed via DroppedClosed \
                         eviction); running the headless recipient pass so \
                         the message is not silently lost"
                    );
                    run_headless_recipient_pass(deps, &bare, *stanza, recursion_depth + 1).await;
                }
            }
        }
        Vec::new()
    }
}

/// #1246 — does `bare` resolve to a registered local account?
///
/// Uses the OIDC + native aware [`crate::auth::local_account_exists`]
/// (two-table identity: `users` for OIDC accounts, `native_users` for
/// SCRAM accounts) — a native-only check would wrongly bounce every
/// OIDC user. Fails OPEN: with no `web_socket_state` (unit-test
/// fixtures) or on a transient DB error the message proceeds to the
/// headless pass rather than bouncing a possibly-valid user; a
/// domain-only bare JID (no localpart) is not a user account and is
/// left to the existing routing behavior.
async fn local_account_exists_for(deps: &Deps<'_>, bare: &BareJid) -> bool {
    let Some(state) = deps.web_socket_state else {
        return true;
    };
    let Some(node) = bare.node() else {
        return true;
    };
    let actor = state.deps.app_state.db_pool.global_actor();
    match crate::auth::local_account_exists(actor, node.as_str(), bare.domain().as_str()).await {
        Ok(exists) => exists,
        Err(error) => {
            warn!(
                bare_jid = %bare,
                %error,
                "RouteToConnection: local_account_exists lookup failed; \
                 failing open (message proceeds to the offline pass)"
            );
            true
        }
    }
}

/// RFC 6121 §8.5.1 reply for a stanza addressed to a local bare JID
/// with no registered account:
///
/// - **message** (non-error): `<service-unavailable/>` bounce — the
///   same condition XEP-0191 blocked-sender bounces use, so the two
///   are indistinguishable to the sender.
/// - **IQ** `get`/`set`: `<service-unavailable/>` (via the shared
///   undeliverable-IQ builder; `result`/`error` IQs get nothing).
/// - **presence**: nothing is returned here — §8.5.1 silently ignores
///   available/unavailable presence to a nonexistent account, and
///   subscription stanzas are owned by the subscription module, not
///   this routing path.
/// - **error-typed messages**: silently ignored (RFC 6121 §8.3
///   forbids replying to an error with another error).
///
/// Returned stanzas are written back to the originating connection by
/// the interpret loop, exactly like the undeliverable-IQ fallback.
fn bounce_for_nonexistent_account(
    stanza: &Stanza,
    sfu: Option<&dyn waddle_sfu::SfuService>,
) -> Vec<Stanza> {
    match stanza {
        Stanza::Message(message) => {
            if matches!(message.type_, xmpp_parsers::message::MessageType::Error) {
                return Vec::new();
            }
            let reply = waddle_xmpp::protocol::handlers::errors::message_error_reply(
                message,
                StanzaError::new(
                    ErrorType::Cancel,
                    DefinedCondition::ServiceUnavailable,
                    "en",
                    "Service unavailable at this address.",
                ),
            );
            vec![Stanza::Message(reply)]
        }
        Stanza::Iq(_) => bounce_undeliverable_iq(stanza, sfu).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// `call_setup` (#1488): when the relay handles the delivery
/// (returns `Some`), it also owns closing the call-setup ticket —
/// see [`RouteBridge::try_deliver_full_jid_remote`]'s deferred
/// handoff. On `None` the ticket is untouched and the caller closes
/// it from the local delivery outcome.
fn deliver_full_jid_via_ordered_relay<'a>(
    deps: &'a Deps<'_>,
    target: &'a jid::FullJid,
    stanza: &'a Stanza,
    call_setup: Option<PendingCallSetupRoute>,
) -> OrderedRelayDeliveryFuture<'a> {
    Box::pin(async move {
        #[cfg(feature = "clustering")]
        {
            let origin = deps.ordered_relay_origin.as_ref()?;
            let state = deps.web_socket_state?;
            let bridge = state
                .deps
                .app_state
                .clustering_claims
                .ordered_relay_delivery_bridge
                .as_ref()?;
            bridge
                .try_deliver_full_jid_remote(target, stanza, origin, call_setup)
                .await
        }
        #[cfg(not(feature = "clustering"))]
        {
            let _ = (deps, target, stanza, call_setup);
            None
        }
    })
}

async fn deliver_peer_to_full_with_registered_remote(
    deps: &Deps<'_>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    if let Some(outcome) = deliver_registered_remote_resource(
        deps,
        target,
        stanza,
        waddle_xmpp::registry::DeliveryKind::PeerStanza,
    )
    .await
    {
        return outcome;
    }
    deliver_peer_to_full(deps.user_registry, deps.sm_session_registry, target, stanza).await
}

async fn deliver_direct_to_full_with_registered_remote(
    deps: &Deps<'_>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    if let Some(outcome) = deliver_registered_remote_resource(
        deps,
        target,
        stanza,
        waddle_xmpp::registry::DeliveryKind::DirectFrame,
    )
    .await
    {
        return outcome;
    }
    deliver_direct_to_full(deps.user_registry, deps.sm_session_registry, target, stanza).await
}

async fn deliver_registered_remote_resource(
    deps: &Deps<'_>,
    target: &jid::FullJid,
    stanza: &Stanza,
    kind: waddle_xmpp::registry::DeliveryKind,
) -> Option<FullJidDeliveryOutcome> {
    #[cfg(feature = "clustering")]
    {
        let state = deps.web_socket_state?;
        let bridge = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()?;
        bridge
            .try_deliver_registered_remote_resource(target, stanza, kind)
            .await
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (deps, target, stanza, kind);
        None
    }
}

fn deliver_bare_jid_via_ordered_relay<'a>(
    deps: &'a Deps<'_>,
    target: &'a jid::BareJid,
    stanza: &'a Stanza,
) -> OrderedRelayDeliveryFuture<'a> {
    Box::pin(async move {
        #[cfg(feature = "clustering")]
        {
            let origin = deps.ordered_relay_origin.as_ref()?;
            let state = deps.web_socket_state?;
            let bridge = state
                .deps
                .app_state
                .clustering_claims
                .ordered_relay_delivery_bridge
                .as_ref()?;
            bridge
                .try_deliver_bare_jid_remote(target, stanza, origin)
                .await
        }
        #[cfg(not(feature = "clustering"))]
        {
            let _ = (deps, target, stanza);
            None
        }
    })
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
/// `<iq type='result'/>` instead.
///
/// Every OTHER Jingle payload gets a SANITIZED §8.3.1 echo (#1444):
/// the Jingle handler injects the addressee's freshly minted LiveKit
/// join token into forwarded negotiation stanzas, so the raw echo
/// would hand the callee's credential to the caller. The sender gets
/// their own request back minus the server-injected
/// `urn:waddle:transports:livekit:0` transport, and the bounce also
/// revokes exactly the minted issuance via `sfu` (when supplied) so
/// the credential does not outlive the failed delivery. Non-Jingle
/// request IQs keep the verbatim echo.
pub(crate) fn bounce_undeliverable_iq(
    stanza: &Stanza,
    sfu: Option<&dyn waddle_sfu::SfuService>,
) -> Option<Stanza> {
    revoke_credential_minted_into_undeliverable_iq(stanza, sfu);
    undeliverable_iq_reply(stanza)
}

/// Compensation half of the bounce (#1444): when the undeliverable
/// stanza is a forwarded Jingle negotiation carrying a freshly minted
/// LiveKit credential, revoke exactly that issuance. Targeted on the
/// stanza's own jti — never `unregister_call_participant` — because
/// the `(call, identity)` pair may be live in the call through an
/// independent, successful negotiation (e.g. racing same-sid
/// initiates), and one failed delivery must not evict it or invalidate
/// its other tokens. Revocation is process-local JTI bookkeeping; the
/// mint happened on this node's dispatcher, so this is the right
/// registry to compensate.
fn revoke_credential_minted_into_undeliverable_iq(
    stanza: &Stanza,
    sfu: Option<&dyn waddle_sfu::SfuService>,
) {
    let (Stanza::Iq(iq), Some(sfu)) = (stanza, sfu) else {
        return;
    };
    let Some(rollback) =
        waddle_xmpp::protocol::handlers::jingle::undeliverable_negotiation_rollback(iq)
    else {
        return;
    };
    if let Some(jti) = &rollback.minted_jti {
        sfu.revoke_issued_token(&rollback.call_id, &rollback.identity, jti);
    }
}

/// Pure reply half of the bounce — no side effects. See the module
/// doc above [`bounce_undeliverable_iq`] for the terminate-ack and
/// credential-scrub rules.
pub(crate) fn undeliverable_iq_reply(stanza: &Stanza) -> Option<Stanza> {
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
    let is_jingle = payload.is("jingle", waddle_xmpp::xep::xep0166::NS_JINGLE);
    if is_jingle && jingle_action(&payload) == Some(xmpp_parsers::jingle::Action::SessionTerminate)
    {
        // The server already completed the teardown; ack the hangup.
        return Some(Stanza::Iq(Box::new(Iq::Result {
            from: to,
            to: from,
            id,
            payload: None,
        })));
    }
    // RFC 6120 §8.3.1: echo the offending request so the sender can
    // correlate which stanza failed. For Jingle payloads the echo is
    // SANITIZED first (#1444): the sender gets their own request back
    // minus the server-injected LiveKit transport — the one element
    // that can carry credentials they were never meant to hold.
    let echoed = if is_jingle {
        waddle_xmpp::protocol::handlers::jingle::credential_free_jingle_echo(&payload)
    } else {
        payload
    };
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
        payload: Some(echoed),
    })))
}

/// Typed XEP-0166 action of a jingle-namespaced payload, `None` when
/// the payload does not parse as Jingle.
fn jingle_action(payload: &minidom::Element) -> Option<xmpp_parsers::jingle::Action> {
    xmpp_parsers::jingle::Jingle::try_from(payload.clone())
        .ok()
        .map(|jingle| jingle.action)
}

/// #1106: queue the PROCESSED (recipient-stamped) stanza into the
/// detached XEP-0198 replay buffers of `detached_targets`, skipping any
/// resource that was just delivered live. Because the queued form is
/// the shared recipient pass's wire output, resume replay carries the
/// recipient-side `<stanza-id by='recipient'/>` (XEP-0359 §3) — the
/// persistence side effects already ran exactly once in the shared
/// pass, so replay is delivery-only.
///
/// Returns the targets whose queueing did NOT land (session expired or
/// resumed between enumeration and record, or a storage error) so the
/// caller can make a second-chance LIVE delivery attempt — the common
/// cause of `Ok(false)` is the resource resuming mid-route, in which
/// case a direct send reaches it (the persistence already happened, so
/// the retry is delivery-only and cannot duplicate rows).
async fn queue_processed_for_detached(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    ingress_effect_capture: Option<&crate::ingress_shadow::IngressEffectCapture>,
    detached_targets: Vec<jid::FullJid>,
    live_set: &std::collections::HashSet<jid::FullJid>,
    stanza: &Stanza,
) -> Vec<jid::FullJid> {
    let Some(sm) = sm_session_registry else {
        return Vec::new();
    };
    let mut not_queued = Vec::new();
    for full in detached_targets {
        if live_set.contains(&full) {
            continue;
        }
        match sm
            .record_stanza_for_detached_bound_resource_with_stream(
                &full,
                stanza,
                chrono::Utc::now(),
            )
            .await
        {
            Ok(Some(stream)) => {
                if let Some(capture) = ingress_effect_capture {
                    capture.record_intent(IngressEffectIntent::RecipientSmAppend { stream });
                }
                debug!(
                    jid = %full,
                    message_id = stanza_message_id(stanza),
                    "RouteToConnection: processed DM queued for detached \
                     XEP-0198 replay"
                );
            }
            Ok(None) => {
                debug!(
                    jid = %full,
                    message_id = stanza_message_id(stanza),
                    "RouteToConnection: detached session gone between \
                     enumeration and queue (resumed or expired); retrying \
                     as live delivery"
                );
                not_queued.push(full);
            }
            Err(error) => {
                warn!(
                    jid = %full,
                    message_id = stanza_message_id(stanza),
                    %error,
                    "RouteToConnection: failed to record processed DM for \
                     detached resource; retrying as live delivery"
                );
                not_queued.push(full);
            }
        }
    }
    not_queued
}

/// Second-chance delivery for detached targets whose replay-buffer
/// queueing did not land (see [`queue_processed_for_detached`]): try
/// the live channel with the already-processed stanza. Best-effort —
/// the shared pass already persisted archive/inbox, so a miss here
/// degrades to MAM catch-up rather than loss.
async fn retry_unqueued_detached_as_live(
    deps: &Deps<'_>,
    not_queued: Vec<jid::FullJid>,
    processed: &Stanza,
) {
    for full in not_queued {
        let outcome = deliver_direct_to_full_with_registered_remote(deps, &full, processed).await;
        debug!(
            jid = %full,
            message_id = stanza_message_id(processed),
            ?outcome,
            "RouteToConnection: second-chance live delivery after failed \
             detached queueing"
        );
    }
}

fn stanza_message_id(stanza: &Stanza) -> &str {
    match stanza {
        Stanza::Message(message) => message.id.as_ref().map_or("", |id| id.0.as_str()),
        Stanza::Iq(_) | Stanza::Presence(_) => "",
    }
}
