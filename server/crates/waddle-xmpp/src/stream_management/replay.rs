//! XEP-0203 delay stamping for XEP-0198 `<resumed/>` replay (issue
//! #1178).
//!
//! Unacked stanzas remain typed from initial delivery through replay.
//! Replaying them without annotation loses the original send time — clients
//! fall back to drain-time timestamps and sort the stanzas to the tail
//! of the timeline. XEP-0198's Acks section requires a XEP-0203
//! `<delay/>` with the original timestamp when unacknowledged stanzas
//! are redelivered after a failed session; we apply the same stamping
//! to the `<resumed/>` replay (the spec is silent there, but the
//! rationale — preserving the original send date for the recipient —
//! is identical). This module is the pure typed builder that applies that
//! stamp before the WebSocket transport serializes the stanza.

use chrono::{DateTime, Utc};

use super::persistence::SmUnackedStanzaPurpose;
use crate::xep::xep0203::{build_delay_element, DelayInfo};
use crate::xep::NS_DELAY;
use crate::Stanza;

/// One entry of the `<resumed/>` replay set: the queued typed stanza plus
/// the server-side receipt time of the original stanza, so the caller
/// can stamp the XEP-0203 `<delay/>` with the true send time.
///
#[derive(Debug, Clone)]
pub struct ReplayStanza {
    /// Typed stanza retained until the WebSocket replay boundary serializes it.
    pub stanza: Stanza,
    /// Server-side receipt time of the original stanza.
    pub original_receipt_at: DateTime<Utc>,
    /// Typed recovery disposition preserved across partial replay/requeue.
    pub purpose: SmUnackedStanzaPurpose,
}

/// Stamp a queued replay stanza with a XEP-0203 `<delay/>` carrying
/// the original server-side receipt time.
///
/// Mirrors the offline-flush builder
/// [`crate::pending_delivery::flush::build_replay_stanza`] but operates
/// on the unacked queue's typed form. No reason text is attached — like
/// [`crate::pending_delivery::flush::ReplayReason::SmRedelivery`], the
/// stanza was never persisted offline; only the timestamp is late.
///
/// If the stanza already carries a `<delay/>` this server stamped on an
/// earlier path it is returned unchanged: that stamp records the true
/// original time, which can be EARLIER than the queue's
/// `original_receipt_at`. A Q6 SM-expiry redelivery, for example, is
/// recorded into the destination's unacked queue at redelivery time —
/// overwriting its flush-stamped delay would shift the message to the
/// redelivery time, the exact corruption this module exists to prevent.
pub fn stamp_replay_delay(
    stanza: &Stanza,
    server_domain: &str,
    original_receipt_at: DateTime<Utc>,
) -> Stanza {
    fn already_self_stamped(payloads: &[minidom::Element], server_domain: &str) -> bool {
        payloads.iter().any(|child| {
            child.name() == "delay"
                && child.ns() == NS_DELAY
                && child.attr("from") == Some(server_domain)
        })
    }

    let delay = || {
        build_delay_element(&DelayInfo {
            from: Some(server_domain.to_string()),
            stamp: original_receipt_at,
            reason: None,
        })
    };

    // XEP-0203 §2 defines delay annotations for message and presence
    // stanzas only; anything else (notably <iq/>) replays unchanged.
    // Delays stamped by OTHER entities remain in place as delivery history;
    // this server adds at most one stamp of its own.
    match stanza {
        Stanza::Message(original) => {
            let mut message = original.clone();
            if !already_self_stamped(&message.payloads, server_domain) {
                message.payloads.push(delay());
            }
            Stanza::Message(message)
        }
        Stanza::Presence(original) => {
            let mut presence = original.clone();
            if !already_self_stamped(&presence.payloads, server_domain) {
                presence.payloads.push(delay());
            }
            Stanza::Presence(presence)
        }
        Stanza::Iq(_) => stanza.clone(),
    }
}
