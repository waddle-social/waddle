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
    /// No `message_dispatcher` in `Deps` (unit-test fixtures) — the
    /// caller falls back to per-resource `PeerStanza` delivery.
    Unavailable,
    /// XEP-0191 blocklist load failed — fail closed and drop the
    /// message entirely, mirroring [`run_headless_recipient_pass`].
    DropFailClosed,
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
                    "fanout recipient-pass: blocklist load failed; dropping \
                     delivery to preserve XEP-0191 incoming-block enforcement \
                     (fail-closed)"
                );
                return FanoutPassResult::DropFailClosed;
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
    } = nested;
    debug!(
        bare_jid = %recipient_bare,
        discarded_frames = frames.len(),
        discarded_feedback = feedback.len(),
        nested_close = close,
        "fanout recipient-pass: persistence interpreted once; transient \
         outcome discarded"
    );

    FanoutPassResult::Ran {
        processed,
        side_routes,
    }
}

/// Apply a XEP-0424 §"prevent further distribution" tombstone to the
/// retraction target inside `archive`. Looks up the target via the
/// retraction's wire id (matches legacy
/// `lookup_retraction_target_message`), then replaces the row with a
/// tombstone using `mam_storage.replace_with_tombstone`.
///
/// Called from the [`OutboundEvent::ArchiveDirect`] arm once per
/// archive write, so sender's and recipient's archives both
/// independently observe the tombstone. Failures are logged at WARN
/// and ignored — the retraction message itself was already archived
/// and the original is the SHOULD-be-tombstoned target, never the
/// authoritative payload after this point.
pub(super) async fn deliver_peer_to_full(
    registry: &waddle_xmpp::registry::ConnectionRegistry,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) {
    // Issue #699: the MUC reflector emits one `RouteToConnection` per
    // occupant. Sending with the blocking `send_peer_to` (which
    // `mpsc::Sender::send().await`s on a 256-slot per-connection
    // channel) means a single zombie WebSocket peer — TCP gone
    // without FIN, consumer task hung on `Sink::send` — wedges every
    // subsequent groupchat dispatch through the same interpreter
    // loop. In prod that froze global MAM + inbox writes for hours
    // until a pod restart. Groupchat reflection is a fan-out by
    // design, so route it through the non-blocking `try_send_peer_to`
    // and tolerate `DroppedFull` for slow consumers. 1:1 chat
    // (message type='chat', IQ, presence-to-full-JID) keeps the
    // blocking path: those targets are singular and the natural
    // backpressure is desirable.
    let is_groupchat_reflection = matches!(
        stanza,
        Stanza::Message(message)
            if message.type_ == xmpp_parsers::message::MessageType::Groupchat
    );
    if is_groupchat_reflection {
        match registry.try_send_peer_to(target, stanza.clone()) {
            waddle_xmpp::registry::BroadcastOutcome::Delivered => {
                debug!(jid = %target, "RouteToConnection: groupchat reflection queued for recipient pass");
            }
            waddle_xmpp::registry::BroadcastOutcome::DroppedFull => {
                // Per-recipient log at debug; the broadcast-level
                // Prometheus counter (`waddle_broadcast_dropped_full_total`)
                // is the canonical signal under load.
                debug!(
                    jid = %target,
                    "RouteToConnection: groupchat reflection dropped (recipient mpsc full)"
                );
            }
            waddle_xmpp::registry::BroadcastOutcome::NotConnected
            | waddle_xmpp::registry::BroadcastOutcome::DroppedClosed => {
                deliver_to_detached(sm_session_registry, target, stanza).await;
            }
        }
        return;
    }
    // The live-send path needs ownership for `send_peer_to`; the
    // detached fallback only borrows. Clone once here on the live
    // branch so the caller hands us an `&Stanza` and avoids a
    // redundant clone per live target on the bare-JID fan-out hot
    // path (Copilot review on PR #276).
    match registry.send_peer_to(target, stanza.clone()).await {
        waddle_xmpp::registry::SendResult::Sent => {
            debug!(jid = %target, "RouteToConnection: peer-stanza queued for recipient pass");
        }
        waddle_xmpp::registry::SendResult::NotConnected
        | waddle_xmpp::registry::SendResult::ChannelClosed => {
            deliver_to_detached(sm_session_registry, target, stanza).await;
        }
    }
}

/// Shared "live target unavailable" fallback. Queues the stanza
/// into the recipient's detached XEP-0198 replay buffer if a
/// resumable session exists, otherwise drops with a debug log.
///
/// Known limitation (Copilot review on PR #276): the buffered XML
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
) {
    let Some(sm) = sm_session_registry else {
        debug!(jid = %target, "RouteToConnection: target offline, dropping");
        return;
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
        }
        Ok(false) => {
            debug!(
                jid = %target,
                "RouteToConnection: target offline and no detached session, dropping"
            );
        }
        Err(error) => {
            warn!(
                jid = %target,
                %error,
                "RouteToConnection: failed to record stanza for detached resource"
            );
        }
    }
}
