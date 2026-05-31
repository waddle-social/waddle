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
    } = nested;
    debug!(
        bare_jid = %recipient_bare,
        discarded_frames = frames.len(),
        discarded_feedback = feedback.len(),
        nested_close = close,
        "headless recipient-pass: completed; transient outcome discarded"
    );
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
async fn deliver_to_detached(
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
