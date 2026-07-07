use super::*;

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

    // Recursively interpret with the depth bumped. The inner outcome
    // is *discarded*: the transient SM is ephemeral so any frames
    // (SendStanza) have no wire to write to and any feedback events
    // (callback completions) belong to a state machine that goes out
    // of scope at function return.
    let nested = Box::pin(interpret_with_depth(events, deps, depth)).await;
    let InterpretOutcome {
        frames,
        close,
        feedback,
        // The transient SM has no transport, so it never receives
        // TransportReady/Tick and cannot emit keepalive or timer
        // effects; discarding matches the frames/feedback semantics.
        keepalive_probes: _,
        timer_commands: _,
        // Headless pass emits no wire copy, so there is nothing to
        // rewrite.
        archive_id_rewrites: _,
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
    /// addressed to OTHER parties (XEP-0184 delivery receipt back to
    /// the sender) that must still be routed by the caller.
    Ran {
        processed: Option<Box<Stanza>>,
        side_routes: Vec<(Jid, Box<Stanza>)>,
    },
    /// The shared pass could not run — no `message_dispatcher` in
    /// `Deps` (unit-test fixtures), the static synthetic resource
    /// literal was rejected (should not happen), or the XEP-0191
    /// blocklist load failed. The caller falls back to per-resource
    /// `PeerStanza` delivery: each recipient connection's own state
    /// machine carries a bind-time blocklist snapshot, so XEP-0191
    /// enforcement holds on the fallback path — unlike the OFFLINE
    /// headless pass, which has no per-connection snapshot to fall
    /// back on and must stay fail-closed.
    Unavailable,
}

/// #1106: run the recipient pass ONCE for a bare-JID DM delivered to
/// multiple same-priority resources (RFC 6121 §8.5.2.1.1).
///
/// Mirrors [`run_headless_recipient_pass`] (synthetic full JID,
/// fail-closed blocklist load, transient [`XmppStateMachine`]) with two
/// differences:
///
/// - `has_live_transport` stays `true`: the recipient IS live, so the
///   XEP-0160 offline intake must not queue pending-delivery rows and
///   the XEP-0184 receipt fires (once, instead of once per resource).
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
        return FanoutPassResult::Unavailable;
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
                return FanoutPassResult::Unavailable;
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
                return FanoutPassResult::Unavailable;
            }
        },
        None => Blocklist::empty(),
    };

    let mut transient = XmppStateMachine::new(deps.local_domain, (**dispatcher).clone());
    // Deliberately NOT `set_has_live_transport(false)`: unlike the
    // offline headless pass, this pass acts for a recipient with live
    // resources, so delivery-only behaviour must match the old
    // per-connection recipient pass (no XEP-0160 pending rows, one
    // XEP-0184 receipt).
    transient.transition_to_ready(synthetic_full, false);
    transient.set_blocklist(blocklist);
    transient.set_delivery_fanout(delivery_fanout);

    let events = transient.handle(InboundEvent::StanzaFromPeer(Box::new(stanza)));

    // Partition the pass output:
    // - the final `SendStanza(Message)` is the recipient-stamped wire
    //   copy — captured for the caller to deliver per resource;
    // - `RouteToConnection` events are side stanzas addressed to other
    //   parties (XEP-0184 receipt to the sender) — returned so the
    //   caller can route them at the outer depth, matching the old
    //   per-connection pass where they routed at interpret depth 0;
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
            OutboundEvent::RouteToConnection { jid, stanza } => {
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
        archive_id_rewrites,
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

/// Disposition of a single-resource actor delivery attempt (ADR-0017 Phase 3
/// Slice 9). The bare-JID headless-persistence gate in
/// [`super::route_to_connection::route_to_connection`] needs to know
/// whether ANY target in a delivery set actually landed somewhere durable
/// (`DeliveredLive` or `QueuedDetached`) — self-healing via `DroppedClosed`
/// eviction means a target selected as "live" can still turn out `Dropped`
/// by the time delivery runs, and if every target does, the message must
/// not be silently lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryDisposition {
    /// Delivered onto a live resource's channel.
    DeliveredLive,
    /// Routed to the detached XEP-0198 replay buffer.
    QueuedDetached,
    /// Dropped: full channel, closed/not-connected with no detached
    /// session to fall back to, or a terminal ask failure classified
    /// `MaybeEnqueued` (never routed to detached, to avoid double-delivery).
    Dropped,
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
    target: &jid::FullJid,
    stanza: &Stanza,
    kind: ActorSendKind,
) -> DeliveryDisposition {
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
            return deliver_to_detached(sm_session_registry, target, stanza).await;
        }
        Err(error) => {
            warn!(jid = %target, %error, "actor delivery: GetUser failed; routing to detached");
            return deliver_to_detached(sm_session_registry, target, stanza).await;
        }
    };

    // The two `TrySend*` messages are distinct types with distinct `SendError`
    // types; classify each terminal failure into the shared `ActorSendFailure`
    // disposition (plus a `String` rendering for the human-facing log below)
    // before unifying.
    let outcome: Result<waddle_xmpp::registry::BroadcastOutcome, (ActorSendFailure, String)> =
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

    match outcome {
        Ok(waddle_xmpp::registry::BroadcastOutcome::Delivered) => {
            debug!(jid = %target, "actor delivery: queued for recipient");
            DeliveryDisposition::DeliveredLive
        }
        Ok(waddle_xmpp::registry::BroadcastOutcome::DroppedFull) => {
            debug!(jid = %target, "actor delivery: recipient channel full; dropped");
            DeliveryDisposition::Dropped
        }
        Ok(waddle_xmpp::registry::BroadcastOutcome::NotConnected)
        | Ok(waddle_xmpp::registry::BroadcastOutcome::DroppedClosed) => {
            deliver_to_detached(sm_session_registry, target, stanza).await
        }
        // Provably never enqueued — no delivery was attempted, so the detached
        // replay buffer is a lossless, non-duplicating fallback.
        Err((ActorSendFailure::NeverEnqueued, error)) => {
            warn!(
                jid = %target,
                %error,
                "actor delivery: TrySend ask failed before enqueue; routing to detached"
            );
            deliver_to_detached(sm_session_registry, target, stanza).await
        }
        // May have been enqueued — kameo does not cancel the enqueued handler,
        // so a post-timeout run plus a detached replay would double-deliver.
        // Count the drop so this enqueue-uncertain loss is graphable alongside
        // the other broadcast drop reasons (the one drop path `try_deliver`
        // does not itself account for).
        Err((ActorSendFailure::MaybeEnqueued, error)) => {
            waddle_xmpp::prometheus::increment_delivery_terminal_error_drop();
            warn!(
                jid = %target,
                %error,
                "actor delivery: TrySend ask failed terminally (possibly enqueued); \
                 dropping to avoid double-delivery"
            );
            DeliveryDisposition::Dropped
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
pub(super) async fn deliver_peer_to_full(
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> DeliveryDisposition {
    match user_registry {
        Some(user_registry) => {
            deliver_one_via_actor(
                user_registry,
                sm_session_registry,
                target,
                stanza,
                ActorSendKind::Peer,
            )
            .await
        }
        None => deliver_to_detached(sm_session_registry, target, stanza).await,
    }
}

/// Deliver an already-recipient-passed (`DirectFrame`) stanza to one resource
/// — the #1106 shared fan-out `processed` copy — through the authoritative
/// `UserActor` (`TrySendDirect`).
///
/// ADR-0017 Phase 1 Slice 3: mirrors [`deliver_peer_to_full`] — the actor is
/// the only delivery path; `None` (actor-less test fixtures) falls back to the
/// detached XEP-0198 buffer instead of a DashMap `send_to`.
pub(super) async fn deliver_direct_to_full(
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> DeliveryDisposition {
    match user_registry {
        Some(user_registry) => {
            deliver_one_via_actor(
                user_registry,
                sm_session_registry,
                target,
                stanza,
                ActorSendKind::Direct,
            )
            .await
        }
        None => deliver_to_detached(sm_session_registry, target, stanza).await,
    }
}

/// Shared "live target unavailable" fallback. Queues the stanza
/// into the recipient's detached XEP-0198 replay buffer if a
/// resumable session exists, otherwise drops with a debug log.
///
/// Known limitation (Copilot review on PR #276) — applies to the
/// LEGACY call sites only (full-JID targets, non-DM stanzas); the
/// #1106 shared fan-out pass hands this function the PROCESSED
/// stanza instead: the buffered XML here
/// is the pre-recipient-pass form, so replay on resume sends it
/// verbatim WITHOUT running the recipient-pass chain. The replayed
/// message is missing the recipient-side `<stanza-id by='recipient'/>`
/// (XEP-0359 §5) and recipient-side filtering / archive / inbox
/// effects don't fire. Matches LEGACY behaviour (which had no
/// recipient pass at all) and is therefore not a regression. Closing
/// the gap properly requires running the headless recipient pass per
/// detached target and queueing its `SendStanza` output — tracked as
/// a follow-up to #229.
pub(super) async fn deliver_to_detached(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> DeliveryDisposition {
    let Some(sm) = sm_session_registry else {
        debug!(jid = %target, "RouteToConnection: target offline, dropping");
        return DeliveryDisposition::Dropped;
    };
    match sm
        .record_stanza_for_detached_bound_resource(target, stanza, chrono::Utc::now())
        .await
    {
        Ok(true) => {
            debug!(
                jid = %target,
                "RouteToConnection: recipient detached, queued for XEP-0198 replay"
            );
            DeliveryDisposition::QueuedDetached
        }
        Ok(false) => {
            debug!(
                jid = %target,
                "RouteToConnection: target offline and no detached session, dropping"
            );
            DeliveryDisposition::Dropped
        }
        Err(error) => {
            warn!(
                jid = %target,
                %error,
                "RouteToConnection: failed to record stanza for detached resource"
            );
            DeliveryDisposition::Dropped
        }
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
            inbound_count: 0,
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
