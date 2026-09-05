use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetachedDeliveryCapture {
    pub(crate) outcome: FullJidDeliveryOutcome,
    pub(crate) recipient_sm_append_stream: Option<waddle_xmpp::pending_delivery::SmSessionId>,
}

pub(super) async fn run_headless_recipient_pass(
    deps: &Deps<'_>,
    recipient_bare: &jid::BareJid,
    stanza: Stanza,
    depth: u8,
) {
    let Some(dispatcher) = deps.message_dispatcher else {
        debug!(
            bare_jid = %recipient_bare,
            "headless recipient-pass: no message_dispatcher in Deps; \
             skipping (test fixture)"
        );
        return;
    };

    // Synthetic FullJid for `transition_to_ready`. The resource value
    // is irrelevant — the recipient pass derives `Locality::Recipient`
    // from bare-as-bare matching when `to` is bare.
    let synthetic_resource =
        match jid::ResourcePart::new(waddle_xmpp::protocol::HEADLESS_RECIPIENT_RESOURCE) {
            Ok(rp) => rp,
            Err(error) => {
                warn!(
                    bare_jid = %recipient_bare,
                    %error,
                    "headless recipient-pass: synthetic resource part rejected; \
                     skipping (should not happen — static literal)"
                );
                return;
            }
        };
    let synthetic_full = recipient_bare.with_resource(&synthetic_resource);

    // Fail-closed on blocklist load error (Copilot review on PR #275).
    // Mirroring `load_blocklist_for_bind`'s fail-closed semantic and
    // PR13's bind-time policy: a transient storage error must not
    // disable XEP-0191 incoming-block enforcement, otherwise a blocked
    // sender could be persisted into the offline recipient's MAM /
    // inbox. We skip the recipient pass entirely; the outer arm has
    // already logged the routing intent, and the sender's archive
    // entry survives independently of the recipient pass.
    let blocklist = match deps.blocking_storage {
        Some(storage) => match storage.list_blocked_jid_entries(recipient_bare).await {
            Ok(jids) => Blocklist::new(jids),
            Err(error) => {
                warn!(
                    bare_jid = %recipient_bare,
                    error = %error,
                    "headless recipient-pass: blocklist load failed; skipping \
                     recipient-side processing to preserve XEP-0191 incoming-block \
                     enforcement (fail-closed)"
                );
                return;
            }
        },
        None => Blocklist::empty(),
    };

    let mut transient = XmppStateMachine::new(deps.local_domain, (**dispatcher).clone());
    transient.set_has_live_transport(false);
    transient.transition_to_ready(synthetic_full, false);
    transient.set_blocklist(blocklist);

    let events = transient.handle(InboundEvent::StanzaFromPeer(Box::new(stanza)));

    // Handler-generated side stanzas addressed to OTHER parties (the
    // XEP-0191 <service-unavailable/> bounce back to a blocked sender)
    // must route at the depth of the interpret loop that invoked this
    // pass: interpreting them at `depth` (the headless depth itself)
    // would trip the recursion guard and silently swallow them.
    // Mirrors the fanout pass's `side_routes` partition. Side routes
    // always target the peer's JID, so they cannot re-enter the pass
    // that produced them.
    let mut side_routes: Vec<OutboundEvent> = Vec::new();
    let mut remaining: Vec<OutboundEvent> = Vec::with_capacity(events.len());
    for event in events {
        match event {
            OutboundEvent::RouteToConnection { .. } => side_routes.push(event),
            other => remaining.push(other),
        }
    }
    if !side_routes.is_empty() {
        let _ = Box::pin(interpret_with_depth(
            side_routes,
            deps,
            depth.saturating_sub(1),
        ))
        .await;
    }

    // Recursively interpret with the depth bumped. The inner outcome
    // is *discarded*: the transient SM is ephemeral so any frames
    // (SendStanza) have no wire to write to and any feedback events
    // (callback completions) belong to a state machine that goes out
    // of scope at function return.
    let nested = Box::pin(interpret_with_depth(remaining, deps, depth)).await;
    let InterpretOutcome {
        frames,
        close,
        feedback,
        // The transient SM has no transport, so it never receives
        // TransportReady/Tick and cannot emit keepalive or timer
        // effects; discarding matches the frames/feedback semantics.
        keepalive_probes: _,
        timer_commands: _,
        // Retry suppression belongs to this discarded transient batch and
        // must not escape into the caller's unrelated recipient work.
        retry_suppression: _,
        // Headless pass emits no wire copy, so there is nothing to
        // rewrite.
        archive_id_rewrites: _,
        route_to_connection_events: _,
    } = nested;
    debug!(
        bare_jid = %recipient_bare,
        discarded_frames = frames.len(),
        discarded_feedback = feedback.len(),
        nested_close = close,
        "headless recipient-pass: completed; transient outcome discarded"
    );
}

/// Result of [`run_fanout_recipient_pass`].
pub(super) enum FanoutPassResult {
    /// The shared recipient pass ran. `processed` is the
    /// recipient-stamped stanza the pipeline emitted for the wire
    /// (`None` when the pass dropped the message, e.g. XEP-0191
    /// incoming block). `side_routes` are handler-generated stanzas
    /// addressed to OTHER parties (the XEP-0191 <service-unavailable/>
    /// bounce back to a blocked sender) that must still be routed by
    /// the caller.
    Ran {
        processed: Option<Box<Stanza>>,
        side_routes: Vec<(Jid, Box<Stanza>)>,
    },
    /// The shared pass could not run — no `message_dispatcher` in
    /// `Deps` (unit-test fixtures), the static synthetic resource
    /// literal was rejected (should not happen), or the XEP-0191
    /// blocklist load failed (`blocklist_failed = true`). The caller
    /// falls back to per-resource `PeerStanza` delivery: each
    /// recipient connection's own state machine carries a bind-time
    /// blocklist snapshot, so XEP-0191 enforcement holds for LIVE
    /// fallback delivery. Detached XEP-0198 raw queueing has no such
    /// snapshot — replay writes the stored XML verbatim — so callers
    /// MUST NOT queue to detached buffers when `blocklist_failed` is
    /// set (fail-closed, mirroring the headless pass).
    Unavailable {
        /// True when the pass was skipped because the XEP-0191
        /// blocklist could not be loaded — recipient-side filtering
        /// is unverified, so only per-connection-snapshot-guarded
        /// delivery may proceed.
        blocklist_failed: bool,
    },
}

/// #1106: run the recipient pass ONCE for a bare-JID DM delivered to
/// multiple same-priority resources (RFC 6121 §8.5.2.1.1).
///
/// Mirrors [`run_headless_recipient_pass`] (synthetic full JID,
/// fail-closed blocklist load, transient [`XmppStateMachine`]) with two
/// differences:
///
/// - `has_live_transport` stays `true`: the recipient IS live, so the
///   XEP-0160 offline intake must not queue pending-delivery rows.
/// - The pass's wire output is NOT discarded: the final
///   [`OutboundEvent::SendStanza`] carries the recipient-stamped
///   message; the caller delivers that one processed stanza to every
///   resource in the delivery set.
///
/// The transient machine is seeded with the delivery-fanout set so the
/// XEP-0280 carbons handler excludes the WHOLE delivery set
/// (XEP-0280 §6.3), not just one resource.
///
/// Persistence side effects (XEP-0313 archive, inbox projection,
/// carbon fan-out) are interpreted exactly once at `depth` (bumped),
/// so the recursion guard in [`route_to_connection`] prevents any
/// nested re-pass, exactly like the headless path.
pub(super) async fn run_fanout_recipient_pass(
    deps: &Deps<'_>,
    recipient_bare: &jid::BareJid,
    delivery_fanout: Vec<jid::FullJid>,
    stanza: Stanza,
    depth: u8,
) -> FanoutPassResult {
    let Some(dispatcher) = deps.message_dispatcher else {
        debug!(
            bare_jid = %recipient_bare,
            "fanout recipient-pass: no message_dispatcher in Deps; \
             falling back to per-resource delivery (test fixture)"
        );
        return FanoutPassResult::Unavailable {
            blocklist_failed: false,
        };
    };

    let synthetic_resource =
        match jid::ResourcePart::new(waddle_xmpp::protocol::HEADLESS_RECIPIENT_RESOURCE) {
            Ok(rp) => rp,
            Err(error) => {
                warn!(
                    bare_jid = %recipient_bare,
                    %error,
                    "fanout recipient-pass: synthetic resource part rejected; \
                     falling back to per-resource delivery (should not happen — \
                     static literal)"
                );
                return FanoutPassResult::Unavailable {
                    blocklist_failed: false,
                };
            }
        };
    let synthetic_full = recipient_bare.with_resource(&synthetic_resource);

    // Fail-closed on blocklist load error, mirroring
    // [`run_headless_recipient_pass`]: a transient storage error must
    // not disable XEP-0191 incoming-block enforcement.
    let blocklist = match deps.blocking_storage {
        Some(storage) => match storage.list_blocked_jid_entries(recipient_bare).await {
            Ok(jids) => Blocklist::new(jids),
            Err(error) => {
                warn!(
                    bare_jid = %recipient_bare,
                    error = %error,
                    "fanout recipient-pass: blocklist load failed; falling back \
                     to legacy per-resource delivery (each recipient \
                     connection's bind-time blocklist snapshot keeps XEP-0191 \
                     enforcement)"
                );
                return FanoutPassResult::Unavailable {
                    blocklist_failed: true,
                };
            }
        },
        None => Blocklist::empty(),
    };

    let mut transient = XmppStateMachine::new(deps.local_domain, (**dispatcher).clone());
    // Deliberately NOT `set_has_live_transport(false)`: unlike the
    // offline headless pass, this pass acts for a recipient with live
    // resources, so delivery-only behaviour must match the old
    // per-connection recipient pass (no XEP-0160 pending rows and no
    // server-fabricated XEP-0184 receipt).
    transient.transition_to_ready(synthetic_full, false);
    transient.set_blocklist(blocklist);
    transient.set_delivery_fanout(delivery_fanout);

    let events = transient.handle(InboundEvent::StanzaFromPeer(Box::new(stanza)));

    // Partition the pass output:
    // - the final `SendStanza(Message)` is the recipient-stamped wire
    //   copy — captured for the caller to deliver per resource;
    // - `RouteToConnection` events are side stanzas addressed to other
    //   parties — returned so the caller can route them at the outer
    //   depth, matching the old per-connection pass where they routed
    //   at interpret depth 0;
    // - everything else (ArchiveDirect / ProjectInbox / SendCarbons /
    //   logs) is interpreted exactly once, depth-bumped.
    let mut processed: Option<Box<Stanza>> = None;
    let mut side_routes: Vec<(Jid, Box<Stanza>)> = Vec::new();
    let mut remaining: Vec<OutboundEvent> = Vec::with_capacity(events.len());
    for event in events {
        match event {
            OutboundEvent::SendStanza(boxed) if matches!(boxed.as_ref(), Stanza::Message(_)) => {
                if processed.is_some() {
                    warn!(
                        bare_jid = %recipient_bare,
                        "fanout recipient-pass: multiple SendStanza(Message) \
                         events; keeping the last (pipeline emits exactly one \
                         wire copy per RouteHandler recipient branch)"
                    );
                }
                processed = Some(boxed);
            }
            OutboundEvent::RouteToConnection { jid, stanza, .. } => {
                side_routes.push((jid, stanza));
            }
            other => remaining.push(other),
        }
    }

    let nested = Box::pin(interpret_with_depth(remaining, deps, depth)).await;
    let InterpretOutcome {
        frames,
        close,
        feedback,
        keepalive_probes: _,
        timer_commands: _,
        // The fanout recipient pass discards its transient outcome; its
        // batch-local retry marker is not meaningful to the outer batch.
        retry_suppression: _,
        archive_id_rewrites,
        route_to_connection_events: _,
    } = nested;
    debug!(
        bare_jid = %recipient_bare,
        discarded_frames = frames.len(),
        discarded_feedback = feedback.len(),
        nested_close = close,
        "fanout recipient-pass: persistence interpreted once; transient \
         outcome discarded"
    );
    // XEP-0359 live/MAM id parity: the archive store may have deduped
    // to an EXISTING row (origin-id retry), reported via
    // ArchiveIdRewrite. The interpreter already rewrote the persistence
    // events in the batch; the wire copy and side routes were extracted
    // BEFORE interpreting, so apply the rewrites here too — otherwise
    // live resources carry a recipient <stanza-id/> no archive row has.
    if !archive_id_rewrites.is_empty() {
        if let Some(processed) = processed.as_mut() {
            rewrite_stanza_archive_ids(processed, &archive_id_rewrites);
        }
        for (_, stanza) in side_routes.iter_mut() {
            rewrite_stanza_archive_ids(stanza, &archive_id_rewrites);
        }
    }

    FanoutPassResult::Ran {
        processed,
        side_routes,
    }
}

/// Upper bound on each actor `ask` on the delivery hot path (mailbox enqueue +
/// handler reply). `UserActor` delivery handlers are non-blocking `try_send`s,
/// so a real stall is rare; the bound keeps a wedged actor from stalling the
/// interpreter loop.
const ACTOR_DELIVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// #1263: bounded in-line retry schedule for a `DroppedFull` actor-path
/// delivery — the recipient's outbound channel was full, the frame was
/// provably never enqueued, and a short pause usually lets the consumer
/// drain. Kept deliberately tight (25 ms total worst case): the retries
/// run inside the sender's interpreter, so across a sequential
/// reflection fan-out with several backpressured recipients the added
/// sender-loop latency stays far below the existing 2 s per-ask bound
/// (SM review on PR #1277). The MUC presence fan-out does NOT use this
/// schedule — its join/leave broadcast loops are non-blocking by
/// contract and retry once without sleeping.
const DROPPED_FULL_RETRY_DELAYS: [std::time::Duration; 2] = [
    std::time::Duration::from_millis(5),
    std::time::Duration::from_millis(20),
];

/// Which recipient-pass semantics the actor should stamp on the queued frame.
#[derive(Clone, Copy)]
enum ActorSendKind {
    /// `DeliveryKind::DirectFrame` — the stanza is already recipient-passed
    /// (the #1106 shared fan-out `processed` copy); write it to the wire
    /// as-is.
    Direct,
    /// `DeliveryKind::PeerStanza` — the destination re-runs its recipient pass
    /// (legacy per-resource / groupchat-reflection delivery).
    Peer,
}

/// Classification of a terminal actor `ask` failure on the delivery hot path,
/// deciding whether the frame may safely fall back to the detached XEP-0198
/// replay buffer.
///
/// The decision hinges on whether the message could already be sitting in the
/// actor's mailbox: kameo does **not** cancel an enqueued handler when the
/// caller's `reply_timeout` fires, so a message that may have been enqueued
/// could still be delivered later — routing it to detached as well would
/// double-deliver.
#[derive(Debug, PartialEq, Eq)]
enum ActorSendFailure {
    /// Provably never enqueued (`ActorNotRunning`, `MailboxFull`, and the
    /// mailbox-enqueue `Timeout(Some(_))`, which hands the message back). No
    /// delivery was attempted, so routing to the detached replay buffer is
    /// lossless and cannot duplicate.
    NeverEnqueued,
    /// May have been enqueued (`ActorStopped`, the reply-wait `Timeout(None)`,
    /// `HandlerError`). A post-timeout handler run could still deliver, so we
    /// drop rather than risk a double-delivery via detached.
    MaybeEnqueued,
}

/// Disposition of a single-resource actor delivery attempt. Two callers rely
/// on this:
///
/// - `route_to_connection`'s full-JID path (#1130) needs to distinguish
///   `Unavailable` (confirmed offline, no detached session to hold the
///   stanza) from `Dropped` (an ambiguous/transient failure) so it knows
///   when to synthesize a fallback IQ reply to the sender.
/// - `route_to_connection`'s bare-JID headless-persistence gate (ADR-0017
///   Phase 3 Slice 9) needs to know whether ANY target in a delivery set
///   is handled somewhere durable or maybe-committed in clustered builds
///   (`Delivered`, `QueuedDetached`, or `MaybeCommitted`) — self-healing via
///   `DroppedClosed` eviction means a target selected as "live" can still
///   turn out not to land by the time delivery runs, and if every target
///   fails to land, the message must not be silently lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FullJidDeliveryOutcome {
    /// Delivered onto a live resource's channel.
    Delivered,
    /// Routed to the detached XEP-0198 replay buffer.
    QueuedDetached,
    /// Confirmed offline: no live channel and no detached session to fall
    /// back to.
    Unavailable,
    /// Dropped for an ambiguous/transient reason: full channel, a storage
    /// error while recording to detached, or a terminal ask failure
    /// classified `MaybeEnqueued` (never routed to detached, to avoid
    /// double-delivery).
    Dropped,
    /// The relay ask may have reached the target and committed the delivery,
    /// but the sender did not observe the reply. Callers must suppress local
    /// or headless fallback to avoid duplicate user-visible effects.
    #[cfg(feature = "clustering")]
    MaybeCommitted,
}

impl FullJidDeliveryOutcome {
    pub(crate) fn suppresses_fallback(self) -> bool {
        match self {
            Self::Delivered | Self::QueuedDetached => true,
            Self::Unavailable | Self::Dropped => false,
            #[cfg(feature = "clustering")]
            Self::MaybeCommitted => true,
        }
    }
}

/// #1488: close a routed 1:1 call-setup ticket from a full-JID
/// delivery disposition — the single mapping shared by the local
/// delivery path and the ordered-relay path (including the deferred
/// handoff, which resolves its real outcome in a spawned task).
///
/// `Delivered`, `QueuedDetached` (XEP-0198 replay hands the invite
/// over on resume) and `MaybeCommitted` (ambiguous cluster relay —
/// may well have reached the peer, so the alert must not over-read)
/// count `ok`. `Unavailable` (confirmed offline — the caller gets the
/// undeliverable bounce) and `Dropped` count
/// `failed{reason=peer_unavailable}`; no bounce is sent for `Dropped`.
///
/// `Dropped` is mostly definite loss (recipient channel still full
/// after bounded retries, storage error recording to detached) but
/// also absorbs `ActorSendFailure::MaybeEnqueued` — an actor ask that
/// timed out after possibly enqueueing, where the invite may still be
/// delivered. Counting that ambiguous sliver as `failed` is
/// deliberate, and deliberately the opposite of the `MaybeCommitted`
/// → `ok` choice: `MaybeCommitted` is a healthy-path cluster
/// ambiguity, while a `MaybeEnqueued` timeout means the local user
/// actor is congested or wedged — a state in which call setup IS
/// degraded, so erring toward the alert firing is the useful reading.
/// A `Dropped`-driven spike therefore means delivery loss or actor
/// congestion, not necessarily an offline peer; the runbook on
/// `CallSetupFailureRate` covers the readings.
pub(crate) fn close_call_setup_from_outcome(
    call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
    outcome: FullJidDeliveryOutcome,
) {
    let Some(ticket) = call_setup else {
        return;
    };
    match outcome {
        FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
            ticket.delivered();
        }
        #[cfg(feature = "clustering")]
        FullJidDeliveryOutcome::MaybeCommitted => ticket.delivered(),
        FullJidDeliveryOutcome::Unavailable | FullJidDeliveryOutcome::Dropped => {
            ticket.undeliverable();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DetachedDeliveryOutcome {
    Queued,
    Unavailable,
    Failed,
}

impl From<DetachedDeliveryOutcome> for FullJidDeliveryOutcome {
    fn from(outcome: DetachedDeliveryOutcome) -> Self {
        match outcome {
            DetachedDeliveryOutcome::Queued => Self::QueuedDetached,
            DetachedDeliveryOutcome::Unavailable => Self::Unavailable,
            DetachedDeliveryOutcome::Failed => Self::Dropped,
        }
    }
}

fn detached_after_routing_failure(outcome: DetachedDeliveryOutcome) -> FullJidDeliveryOutcome {
    match outcome {
        DetachedDeliveryOutcome::Queued => FullJidDeliveryOutcome::QueuedDetached,
        DetachedDeliveryOutcome::Unavailable | DetachedDeliveryOutcome::Failed => {
            FullJidDeliveryOutcome::Dropped
        }
    }
}

/// Classify a terminal `SendError` for the delivery fallback decision.
///
/// `Timeout` discriminates by payload: `.mailbox_timeout()` elapsing hands the
/// message back as `Timeout(Some(_))` (never enqueued), whereas
/// `.reply_timeout()` elapsing after a successful enqueue yields `Timeout(None)`
/// (may still run).
fn classify_send_error<M, E>(error: &kameo::error::SendError<M, E>) -> ActorSendFailure {
    use kameo::error::SendError;
    match error {
        SendError::ActorNotRunning(_) | SendError::MailboxFull(_) | SendError::Timeout(Some(_)) => {
            ActorSendFailure::NeverEnqueued
        }
        SendError::ActorStopped | SendError::Timeout(None) | SendError::HandlerError(_) => {
            ActorSendFailure::MaybeEnqueued
        }
    }
}

/// Deliver one stanza to one resource through the authoritative `UserActor`
/// (ADR-0017 Phase 1 Slice 2), with **no DashMap fallback**.
///
/// The actor delivery is a non-blocking `try_send`: `Delivered` is done;
/// `DroppedFull` drops the frame (the recipient is connected but its 256-slot
/// channel is full — a deliberate behaviour change from the old *blocking* 1:1
/// send, so one wedged/zombie recipient can no longer stall global dispatch,
/// issue #699; `try_deliver` already bumps the Prometheus dropped-full
/// counter); `NotConnected`/`DroppedClosed` routes to the detached XEP-0198
/// replay buffer — this is the self-healing eviction path ADR-0017 Phase 3
/// Slice 9 relies on in place of the retired DashMap-liveness intersection
/// filter: a stale actor entry is caught and evicted the moment delivery
/// touches it, exactly like the DashMap path already did.
///
/// A terminal ask failure is classified by [`classify_send_error`]:
/// provably-never-enqueued failures (`ActorNotRunning`, `MailboxFull`,
/// mailbox-enqueue `Timeout(Some(_))`) route to detached — no delivery was
/// attempted, so replay cannot duplicate; possibly-enqueued failures
/// (`ActorStopped`, reply `Timeout(None)`, `HandlerError`) drop WITHOUT routing
/// to detached, because kameo does not cancel an enqueued handler and a
/// post-timeout run plus a detached replay would double-deliver.
async fn deliver_one_via_actor(
    user_registry: &kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    ingress_effect_capture: Option<&crate::ingress_shadow::IngressEffectCapture>,
    target: &jid::FullJid,
    stanza: &Stanza,
    kind: ActorSendKind,
) -> FullJidDeliveryOutcome {
    deliver_one_via_actor_capturing_detached(
        user_registry,
        sm_session_registry,
        ingress_effect_capture,
        target,
        stanza,
        kind,
    )
    .await
    .outcome
}

async fn deliver_one_via_actor_capturing_detached(
    user_registry: &kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    ingress_effect_capture: Option<&crate::ingress_shadow::IngressEffectCapture>,
    target: &jid::FullJid,
    stanza: &Stanza,
    kind: ActorSendKind,
) -> DetachedDeliveryCapture {
    let message_id = stanza_message_id(stanza);
    // FUTURE CLEANUP (ADR-0017; Greptile review on PR #1177, tracked in #1195):
    // for a bare-JID DM this is the SECOND `GetUser` for the same bare JID —
    // `select_bare_jid_live_targets` already resolved the `UserActor` during
    // selection, then every target here re-resolves it. It's a cheap
    // (HashMap-lookup, bounded) registry round-trip, but the delivery signature
    // could thread the already-resolved `ActorRef` from selection to collapse it
    // to one `GetUser` per bare JID. Deferred: keeping the self-contained
    // GetUser+TrySend encapsulation is simpler than reworking the delivery path
    // ahead of the Phase-2/3 changes that touch it anyway.
    let user_actor = match user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .mailbox_timeout(ACTOR_DELIVER_TIMEOUT)
        .reply_timeout(ACTOR_DELIVER_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => actor,
        // No live actor for this bare JID — no delivery was attempted, so the
        // detached replay buffer is a safe (non-duplicating) fallback.
        Ok(None) => {
            return deliver_to_detached_with_capture_details(
                sm_session_registry,
                ingress_effect_capture,
                target,
                stanza,
            )
            .await;
        }
        Err(error) => {
            warn!(jid = %target, message_id, %error, "actor delivery: GetUser failed; routing to detached");
            return deliver_to_detached_with_capture_details(
                sm_session_registry,
                ingress_effect_capture,
                target,
                stanza,
            )
            .await
            .map_outcome(detached_after_routing_failure);
        }
    };

    // The two `TrySend*` messages are distinct types with distinct `SendError`
    // types; classify each terminal failure into the shared `ActorSendFailure`
    // disposition (plus a `String` rendering for the human-facing log below)
    // before unifying.
    //
    // #1263: a `DroppedFull` outcome means the recipient's 256-slot channel
    // was momentarily full — the frame was provably NEVER enqueued, so an
    // in-line retry cannot double-deliver and preserves per-sender ordering
    // (this call blocks the sender's interpreter until it resolves). Retry
    // on the bounded [`DROPPED_FULL_RETRY_DELAYS`] schedule before
    // declaring the frame lost, so a recipient that is merely catching up
    // (e.g. draining a MAM page) doesn't silently miss a groupchat
    // reflection.
    let mut retry_delays = DROPPED_FULL_RETRY_DELAYS.iter();
    let outcome: Result<waddle_xmpp::registry::BroadcastOutcome, (ActorSendFailure, String)> = loop {
        let attempt: Result<waddle_xmpp::registry::BroadcastOutcome, (ActorSendFailure, String)> =
            match kind {
                ActorSendKind::Direct => user_actor
                    .ask(waddle_xmpp::registry::TrySendDirect {
                        jid: target.clone(),
                        stanza: stanza.clone(),
                    })
                    .mailbox_timeout(ACTOR_DELIVER_TIMEOUT)
                    .reply_timeout(ACTOR_DELIVER_TIMEOUT)
                    .await
                    .map_err(|error| (classify_send_error(&error), error.to_string())),
                ActorSendKind::Peer => user_actor
                    .ask(waddle_xmpp::registry::TrySendPeer {
                        jid: target.clone(),
                        stanza: stanza.clone(),
                    })
                    .mailbox_timeout(ACTOR_DELIVER_TIMEOUT)
                    .reply_timeout(ACTOR_DELIVER_TIMEOUT)
                    .await
                    .map_err(|error| (classify_send_error(&error), error.to_string())),
            };
        if matches!(
            attempt,
            Ok(waddle_xmpp::registry::BroadcastOutcome::DroppedFull)
        ) {
            if let Some(delay) = retry_delays.next() {
                tokio::time::sleep(*delay).await;
                continue;
            }
        }
        break attempt;
    };

    match outcome {
        Ok(waddle_xmpp::registry::BroadcastOutcome::Delivered) => {
            debug!(jid = %target, message_id, "actor delivery: queued for recipient");
            DetachedDeliveryCapture::from_outcome(FullJidDeliveryOutcome::Delivered)
        }
        // Still full after every retry — surface the loss instead of the
        // previous silent debug-level drop (#1263). The recipient stays
        // registered: a wedged consumer is reaped by the send-stall
        // backstop / closed-channel eviction, not by a transient full
        // window.
        Ok(waddle_xmpp::registry::BroadcastOutcome::DroppedFull) => {
            waddle_xmpp::telemetry::reliability::increment_delivery_retry_exhausted_drop();
            warn!(
                jid = %target,
                message_id,
                retries = DROPPED_FULL_RETRY_DELAYS.len(),
                "actor delivery: recipient channel still full after bounded retries; dropped"
            );
            DetachedDeliveryCapture::from_outcome(FullJidDeliveryOutcome::Dropped)
        }
        Ok(waddle_xmpp::registry::BroadcastOutcome::NotConnected)
        | Ok(waddle_xmpp::registry::BroadcastOutcome::DroppedClosed) => {
            deliver_to_detached_with_capture_details(
                sm_session_registry,
                ingress_effect_capture,
                target,
                stanza,
            )
            .await
        }
        // Provably never enqueued — no delivery was attempted, so the detached
        // replay buffer is a lossless, non-duplicating fallback.
        Err((ActorSendFailure::NeverEnqueued, error)) => {
            warn!(
                jid = %target,
                message_id,
                %error,
                "actor delivery: TrySend ask failed before enqueue; routing to detached"
            );
            deliver_to_detached_with_capture_details(
                sm_session_registry,
                ingress_effect_capture,
                target,
                stanza,
            )
            .await
            .map_outcome(detached_after_routing_failure)
        }
        // May have been enqueued — kameo does not cancel the enqueued handler,
        // so a post-timeout run plus a detached replay would double-deliver.
        // Count the drop so this enqueue-uncertain loss is graphable alongside
        // the other broadcast drop reasons (the one drop path `try_deliver`
        // does not itself account for).
        Err((ActorSendFailure::MaybeEnqueued, error)) => {
            waddle_xmpp::telemetry::reliability::increment_delivery_terminal_error_drop();
            warn!(
                jid = %target,
                message_id,
                %error,
                "actor delivery: TrySend ask failed terminally (possibly enqueued); \
                 dropping to avoid double-delivery"
            );
            DetachedDeliveryCapture::from_outcome(FullJidDeliveryOutcome::Dropped)
        }
    }
}

/// Deliver a peer-routed (`PeerStanza`) stanza to one resource through the
/// authoritative `UserActor` (`TrySendPeer`).
///
/// ADR-0017 Phase 1 Slice 3: the legacy DashMap delivery methods
/// (`send_peer_to` / `try_send_peer_to`) are deleted, so `user_registry` is the
/// only delivery path. `None` — test fixtures without an actor tree — can no
/// longer deliver live and falls back to the detached XEP-0198 buffer (the same
/// "no live target" fallback used everywhere), never a DashMap send.
#[cfg(feature = "clustering")]
pub(crate) async fn deliver_peer_to_full(
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    deliver_peer_to_full_capturing_detached(
        user_registry,
        sm_session_registry,
        None,
        target,
        stanza,
    )
    .await
}

/// Variant of [`deliver_peer_to_full`] for ingress interpretation, where a
/// successful detached fallback must identify the replay stream it mutated.
pub(crate) async fn deliver_peer_to_full_capturing_detached(
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    ingress_effect_capture: Option<&crate::ingress_shadow::IngressEffectCapture>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    match user_registry {
        Some(user_registry) => {
            deliver_one_via_actor(
                user_registry,
                sm_session_registry,
                ingress_effect_capture,
                target,
                stanza,
                ActorSendKind::Peer,
            )
            .await
        }
        None => deliver_to_detached_with_capture(
            sm_session_registry,
            ingress_effect_capture,
            target,
            stanza,
        )
        .await
        .into(),
    }
}

#[cfg(feature = "clustering")]
pub(crate) async fn deliver_peer_to_full_with_detached_capture(
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> DetachedDeliveryCapture {
    match user_registry {
        Some(user_registry) => {
            deliver_one_via_actor_capturing_detached(
                user_registry,
                sm_session_registry,
                None,
                target,
                stanza,
                ActorSendKind::Peer,
            )
            .await
        }
        None => {
            deliver_to_detached_with_capture_details(sm_session_registry, None, target, stanza)
                .await
        }
    }
}

/// Deliver an already-recipient-passed (`DirectFrame`) stanza to one resource
/// — the #1106 shared fan-out `processed` copy — through the authoritative
/// `UserActor` (`TrySendDirect`).
///
/// ADR-0017 Phase 1 Slice 3: mirrors [`deliver_peer_to_full`] — the actor is
/// the only delivery path; `None` (actor-less test fixtures) falls back to the
/// detached XEP-0198 buffer instead of a DashMap `send_to`.
pub(crate) async fn deliver_direct_to_full(
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    match user_registry {
        Some(user_registry) => {
            deliver_one_via_actor(
                user_registry,
                sm_session_registry,
                None,
                target,
                stanza,
                ActorSendKind::Direct,
            )
            .await
        }
        None => deliver_to_detached_with_capture(sm_session_registry, None, target, stanza)
            .await
            .into(),
    }
}

/// Live-channel-only `PeerStanza` delivery attempt for the full-JID DM
/// path (#1244/#1245): the detached XEP-0198 fallback is deliberately
/// NOT taken here — a detached hit must go through the shared
/// recipient pass so the replay copy is the processed (stamped)
/// stanza, and a fully-missing resource must fall back to bare-JID
/// semantics. The outcome mapping therefore differs from
/// [`deliver_peer_to_full`] on the failure classes:
///
/// - `Delivered` → delivered; terminal.
/// - `DroppedFull` → the recipient is CONNECTED but its channel is
///   full; terminal drop (issue #699 semantics — queueing to detached
///   would replay out of band next resume).
/// - `NotConnected` / `DroppedClosed` / no actor → `Unavailable`; the
///   caller falls through to the detached / §8.5.3.2.1 fallback.
/// - `GetUser` ask error and never-enqueued `TrySendPeer` failures
///   (`ActorNotRunning`, `MailboxFull`, mailbox `Timeout(Some)`) →
///   `Unavailable` too: provably nothing was delivered, so letting the
///   caller's detached/bare fallback run is lossless and cannot
///   duplicate — the legacy path routed exactly these classes to the
///   detached buffer for the same reason.
/// - Maybe-enqueued failures (`ActorStopped`, reply `Timeout(None)`,
///   `HandlerError`) → `Dropped`; kameo does not cancel an enqueued
///   handler, so any fallback delivery could double-deliver.
pub(super) async fn deliver_peer_to_live_only(
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    let message_id = stanza_message_id(stanza);
    let Some(user_registry) = user_registry else {
        return FullJidDeliveryOutcome::Unavailable;
    };
    let user_actor = match user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .mailbox_timeout(ACTOR_DELIVER_TIMEOUT)
        .reply_timeout(ACTOR_DELIVER_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => return FullJidDeliveryOutcome::Unavailable,
        Err(error) => {
            warn!(
                jid = %target,
                message_id,
                %error,
                "live-only delivery: GetUser failed; treating as unavailable \
                 so the detached/bare fallback runs"
            );
            return FullJidDeliveryOutcome::Unavailable;
        }
    };
    match user_actor
        .ask(waddle_xmpp::registry::TrySendPeer {
            jid: target.clone(),
            stanza: stanza.clone(),
        })
        .mailbox_timeout(ACTOR_DELIVER_TIMEOUT)
        .reply_timeout(ACTOR_DELIVER_TIMEOUT)
        .await
    {
        Ok(waddle_xmpp::registry::BroadcastOutcome::Delivered) => {
            debug!(jid = %target, message_id, "live-only delivery: queued for recipient");
            FullJidDeliveryOutcome::Delivered
        }
        Ok(waddle_xmpp::registry::BroadcastOutcome::DroppedFull) => {
            debug!(jid = %target, message_id, "live-only delivery: recipient channel full; dropped");
            FullJidDeliveryOutcome::Dropped
        }
        Ok(waddle_xmpp::registry::BroadcastOutcome::NotConnected)
        | Ok(waddle_xmpp::registry::BroadcastOutcome::DroppedClosed) => {
            FullJidDeliveryOutcome::Unavailable
        }
        Err(error) => match classify_send_error(&error) {
            ActorSendFailure::NeverEnqueued => {
                warn!(
                    jid = %target,
                    message_id,
                    error = %error,
                    "live-only delivery: TrySendPeer failed before enqueue; \
                     treating as unavailable so the detached/bare fallback runs"
                );
                FullJidDeliveryOutcome::Unavailable
            }
            ActorSendFailure::MaybeEnqueued => {
                waddle_xmpp::telemetry::reliability::increment_delivery_terminal_error_drop();
                warn!(
                    jid = %target,
                    message_id,
                    error = %error,
                    "live-only delivery: TrySendPeer failed terminally (possibly \
                     enqueued); dropping to avoid double-delivery"
                );
                FullJidDeliveryOutcome::Dropped
            }
        },
    }
}

/// Queue a fallback replay stanza and record the *accepted* SM stream when
/// this route belongs to an ingress capture. The registry resolves the stream
/// under its own lock, so we never infer a stale stream from the full JID.
pub(super) async fn deliver_to_detached_with_capture(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    ingress_effect_capture: Option<&crate::ingress_shadow::IngressEffectCapture>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> DetachedDeliveryOutcome {
    deliver_to_detached_with_capture_details(
        sm_session_registry,
        ingress_effect_capture,
        target,
        stanza,
    )
    .await
    .detached_outcome()
}

async fn deliver_to_detached_with_capture_details(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    ingress_effect_capture: Option<&crate::ingress_shadow::IngressEffectCapture>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> DetachedDeliveryCapture {
    let message_id = stanza_message_id(stanza);
    let Some(sm) = sm_session_registry else {
        debug!(jid = %target, message_id, "RouteToConnection: target offline, dropping");
        return DetachedDeliveryCapture::from_detached(DetachedDeliveryOutcome::Unavailable, None);
    };
    match sm
        .record_stanza_for_detached_bound_resource_with_stream(target, stanza, chrono::Utc::now())
        .await
    {
        Ok(Some(stream)) => {
            if let Some(capture) = ingress_effect_capture {
                capture.record_recipient_sm_append(stream.clone());
            }
            debug!(
                jid = %target,
                message_id,
                "RouteToConnection: recipient detached, queued for XEP-0198 replay"
            );
            DetachedDeliveryCapture::from_detached(DetachedDeliveryOutcome::Queued, Some(stream))
        }
        Ok(None) => {
            debug!(
                jid = %target,
                message_id,
                "RouteToConnection: target offline and no detached session, dropping"
            );
            DetachedDeliveryCapture::from_detached(DetachedDeliveryOutcome::Unavailable, None)
        }
        Err(error) => {
            warn!(
                jid = %target,
                message_id,
                %error,
                "RouteToConnection: failed to record stanza for detached resource"
            );
            DetachedDeliveryCapture::from_detached(DetachedDeliveryOutcome::Failed, None)
        }
    }
}

impl DetachedDeliveryCapture {
    pub(crate) fn from_outcome(outcome: FullJidDeliveryOutcome) -> Self {
        Self {
            outcome,
            recipient_sm_append_stream: None,
        }
    }

    fn from_detached(
        outcome: DetachedDeliveryOutcome,
        recipient_sm_append_stream: Option<waddle_xmpp::pending_delivery::SmSessionId>,
    ) -> Self {
        Self {
            outcome: outcome.into(),
            recipient_sm_append_stream,
        }
    }

    fn map_outcome(
        self,
        map: impl FnOnce(DetachedDeliveryOutcome) -> FullJidDeliveryOutcome,
    ) -> Self {
        Self {
            outcome: map(self.detached_outcome()),
            recipient_sm_append_stream: self.recipient_sm_append_stream,
        }
    }

    fn detached_outcome(&self) -> DetachedDeliveryOutcome {
        match self.outcome {
            FullJidDeliveryOutcome::QueuedDetached => DetachedDeliveryOutcome::Queued,
            FullJidDeliveryOutcome::Unavailable => DetachedDeliveryOutcome::Unavailable,
            FullJidDeliveryOutcome::Delivered => DetachedDeliveryOutcome::Unavailable,
            FullJidDeliveryOutcome::Dropped => DetachedDeliveryOutcome::Failed,
            #[cfg(feature = "clustering")]
            FullJidDeliveryOutcome::MaybeCommitted => DetachedDeliveryOutcome::Failed,
        }
    }
}

fn stanza_message_id(stanza: &Stanza) -> &str {
    match stanza {
        Stanza::Message(message) => message.id.as_ref().map_or("", |id| id.0.as_str()),
        Stanza::Iq(_) | Stanza::Presence(_) => "",
    }
}

#[cfg(test)]
mod tests {
    //! ADR-0017 Phase 1 Slice 2 delivery-cutover behaviour: the actor-path
    //! delivery decides between the recipient's live channel, a deliberate
    //! drop, and the detached XEP-0198 replay buffer — and MUST NOT
    //! double-deliver a frame that may still be sitting in the actor's mailbox.
    use super::*;
    use kameo::actor::Spawn;
    use kameo::error::SendError;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use waddle_xmpp::registry::{ConnectionEntry, OutboundStanza, UserRegistryActor};
    use waddle_xmpp::stream_management::{
        DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry,
    };

    fn full(s: &str) -> jid::FullJid {
        s.parse().expect("valid full jid")
    }

    fn sample_message(to: &jid::FullJid) -> Stanza {
        let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(to.clone())));
        msg.type_ = xmpp_parsers::message::MessageType::Chat;
        msg.bodies
            .insert(xmpp_parsers::message::Lang::new(), "hello".to_string());
        Stanza::Message(msg)
    }

    /// A non-expired detached session bound to `jid`, so
    /// `record_stanza_for_detached_bound_resource` matches it and appends to
    /// `unacked_stanzas` — the observable signal that a frame was routed to
    /// the XEP-0198 replay buffer rather than dropped.
    fn detached_session(stream_id: &str, jid: &jid::FullJid) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: jid.to_bare().to_string(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        }
    }

    /// Spawn a fresh registry with `jid` registered against a channel of the
    /// given capacity. The receiver is returned so the caller controls channel
    /// state: keep it to leave the channel open, `drop` it to close it.
    async fn registry_with_resource(
        jid: &jid::FullJid,
        capacity: usize,
    ) -> (
        kameo::actor::ActorRef<UserRegistryActor>,
        mpsc::Receiver<OutboundStanza>,
    ) {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let (tx, rx) = mpsc::channel(capacity);
        let entry = ConnectionEntry::new(tx);
        assert!(
            crate::server::dual_registration::mirror_register(&registry, jid.clone(), entry).await,
            "register mirror should confirm the resource in the actor tree"
        );
        (registry, rx)
    }

    async fn unacked_len(sm: &Arc<InMemorySmSessionRegistry>, stream_id: &str) -> usize {
        sm.peek_session(stream_id)
            .await
            .expect("peek ok")
            .expect("detached session present")
            .unacked_stanzas
            .len()
    }

    /// Finding 1 (PR #1177 council review): the never-enqueued/maybe-enqueued
    /// partition is the whole safety argument for the terminal-error fallback,
    /// so pin every `SendError` variant's classification directly.
    #[test]
    fn classify_send_error_partitions_by_enqueue_certainty() {
        // Provably never enqueued — the actor never saw the message, so
        // detached is a lossless fallback.
        assert_eq!(
            classify_send_error(&SendError::<(), ()>::ActorNotRunning(())),
            ActorSendFailure::NeverEnqueued
        );
        assert_eq!(
            classify_send_error(&SendError::<(), ()>::MailboxFull(())),
            ActorSendFailure::NeverEnqueued
        );
        assert_eq!(
            classify_send_error(&SendError::<(), ()>::Timeout(Some(()))),
            ActorSendFailure::NeverEnqueued
        );
        // May have been enqueued — a post-timeout handler run plus a detached
        // replay would double-deliver, so these drop.
        assert_eq!(
            classify_send_error(&SendError::<(), ()>::Timeout(None)),
            ActorSendFailure::MaybeEnqueued
        );
        assert_eq!(
            classify_send_error(&SendError::<(), ()>::ActorStopped),
            ActorSendFailure::MaybeEnqueued
        );
        assert_eq!(
            classify_send_error(&SendError::<(), ()>::HandlerError(())),
            ActorSendFailure::MaybeEnqueued
        );
    }

    /// A successfully-queued frame must NOT also be buffered for replay.
    #[tokio::test]
    async fn actor_delivery_delivered_does_not_route_to_detached() {
        let target = full("alice@example.com/web");
        let (registry, _rx) = registry_with_resource(&target, 4).await;
        let sm = Arc::new(InMemorySmSessionRegistry::new());
        sm.store_session(detached_session("s-delivered", &target))
            .await
            .expect("store");

        deliver_one_via_actor(
            &registry,
            Some(&sm),
            None,
            &target,
            &sample_message(&target),
            ActorSendKind::Peer,
        )
        .await;

        assert_eq!(
            unacked_len(&sm, "s-delivered").await,
            0,
            "a delivered frame must not also be queued for XEP-0198 replay"
        );
    }

    /// `DroppedFull` is a deliberate behaviour change (issue #699): a connected
    /// recipient whose channel is full has the frame DROPPED, never rerouted to
    /// detached (that would replay it out of band on resume).
    #[tokio::test]
    async fn actor_delivery_dropped_full_drops_without_detached() {
        let target = full("alice@example.com/web");
        let (registry, _rx) = registry_with_resource(&target, 1).await;

        // Fill the single channel slot (Delivered) so the next send is Full.
        // `None` sm so this priming send can never touch the detached buffer.
        deliver_one_via_actor(
            &registry,
            None,
            None,
            &target,
            &sample_message(&target),
            ActorSendKind::Peer,
        )
        .await;

        let sm = Arc::new(InMemorySmSessionRegistry::new());
        sm.store_session(detached_session("s-full", &target))
            .await
            .expect("store");

        deliver_one_via_actor(
            &registry,
            Some(&sm),
            None,
            &target,
            &sample_message(&target),
            ActorSendKind::Peer,
        )
        .await;

        assert_eq!(
            unacked_len(&sm, "s-full").await,
            0,
            "DroppedFull must drop, not route to detached replay"
        );
    }

    /// A live actor with no such resource (`NotConnected`) falls back to the
    /// detached replay buffer.
    #[tokio::test]
    async fn actor_delivery_not_connected_routes_to_detached() {
        let registered = full("alice@example.com/web");
        let (registry, _rx) = registry_with_resource(&registered, 4).await;

        // Same bare JID, different resource: the actor exists but the resource
        // is absent → NotConnected.
        let missing = full("alice@example.com/desktop");
        let sm = Arc::new(InMemorySmSessionRegistry::new());
        sm.store_session(detached_session("s-missing", &missing))
            .await
            .expect("store");

        deliver_one_via_actor(
            &registry,
            Some(&sm),
            None,
            &missing,
            &sample_message(&missing),
            ActorSendKind::Peer,
        )
        .await;

        assert_eq!(
            unacked_len(&sm, "s-missing").await,
            1,
            "NotConnected must route to detached replay"
        );
    }

    /// A closed channel (`DroppedClosed`) falls back to the detached buffer.
    #[tokio::test]
    async fn actor_delivery_dropped_closed_routes_to_detached() {
        let target = full("alice@example.com/web");
        let (registry, rx) = registry_with_resource(&target, 4).await;
        drop(rx); // close the channel → try_send returns Closed

        let sm = Arc::new(InMemorySmSessionRegistry::new());
        sm.store_session(detached_session("s-closed", &target))
            .await
            .expect("store");

        deliver_one_via_actor(
            &registry,
            Some(&sm),
            None,
            &target,
            &sample_message(&target),
            ActorSendKind::Peer,
        )
        .await;

        assert_eq!(
            unacked_len(&sm, "s-closed").await,
            1,
            "DroppedClosed must route to detached replay"
        );
    }

    /// Live-only delivery (#1244/#1245 full-JID DM path): the healthy
    /// outcomes map like the actor path, but every provably
    /// never-delivered failure maps to `Unavailable` so the caller's
    /// detached / bare-JID fallback still runs.
    #[tokio::test]
    async fn live_only_delivery_maps_absent_user_to_unavailable() {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let target = full("ghost@example.com/web");
        let outcome =
            deliver_peer_to_live_only(Some(&registry), &target, &sample_message(&target)).await;
        assert_eq!(outcome, FullJidDeliveryOutcome::Unavailable);
    }

    #[tokio::test]
    async fn live_only_delivery_maps_missing_resource_to_unavailable() {
        let registered = full("alice@example.com/web");
        let (registry, _rx) = registry_with_resource(&registered, 4).await;
        let missing = full("alice@example.com/desktop");
        let outcome =
            deliver_peer_to_live_only(Some(&registry), &missing, &sample_message(&missing)).await;
        assert_eq!(outcome, FullJidDeliveryOutcome::Unavailable);
    }

    #[tokio::test]
    async fn live_only_delivery_delivers_to_live_channel() {
        let target = full("alice@example.com/web");
        let (registry, mut rx) = registry_with_resource(&target, 4).await;
        let outcome =
            deliver_peer_to_live_only(Some(&registry), &target, &sample_message(&target)).await;
        assert_eq!(outcome, FullJidDeliveryOutcome::Delivered);
        assert!(rx.try_recv().is_ok(), "frame reached the live channel");
    }

    #[tokio::test]
    async fn live_only_delivery_maps_full_channel_to_dropped() {
        let target = full("alice@example.com/web");
        let (registry, _rx) = registry_with_resource(&target, 1).await;
        // Fill the single slot.
        deliver_peer_to_live_only(Some(&registry), &target, &sample_message(&target)).await;
        let outcome =
            deliver_peer_to_live_only(Some(&registry), &target, &sample_message(&target)).await;
        assert_eq!(
            outcome,
            FullJidDeliveryOutcome::Dropped,
            "DroppedFull is terminal (issue #699): the recipient is connected, \
             so fallback delivery would double-deliver"
        );
    }

    /// No actor for the bare JID (`GetUser` = `None`) falls back to detached —
    /// nothing was ever attempted, so replay cannot duplicate.
    #[tokio::test]
    async fn actor_delivery_absent_user_routes_to_detached() {
        let registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let target = full("ghost@example.com/web");
        let sm = Arc::new(InMemorySmSessionRegistry::new());
        sm.store_session(detached_session("s-ghost", &target))
            .await
            .expect("store");

        deliver_one_via_actor(
            &registry,
            Some(&sm),
            None,
            &target,
            &sample_message(&target),
            ActorSendKind::Peer,
        )
        .await;

        assert_eq!(
            unacked_len(&sm, "s-ghost").await,
            1,
            "GetUser=None must route to detached replay"
        );
    }
}
