//! XEP-0203 delay stamping for XEP-0198 `<resumed/>` replay (issue
//! #1178).
//!
//! Unacked stanzas are queued as the exact wire XML captured at record
//! time. Replaying them verbatim loses the original send time — clients
//! fall back to drain-time timestamps and sort the stanzas to the tail
//! of the timeline. XEP-0198's Acks section requires a XEP-0203
//! `<delay/>` with the original timestamp when unacknowledged stanzas
//! are redelivered after a failed session; we apply the same stamping
//! to the `<resumed/>` replay (the spec is silent there, but the
//! rationale — preserving the original send date for the recipient —
//! is identical). This module is the pure builder that applies that
//! stamp at the serialization boundary.

use chrono::{DateTime, Utc};
use minidom::Element;
use std::str::FromStr;
use tracing::warn;

use super::persistence::SmUnackedStanzaPurpose;
use crate::parser::element_to_string;
use crate::xep::xep0203::{build_delay_element, DelayInfo};
use crate::xep::NS_DELAY;

/// One entry of the `<resumed/>` replay set: the queued wire XML plus
/// the server-side receipt time of the original stanza, so the caller
/// can stamp the XEP-0203 `<delay/>` with the true send time.
///
/// `stanza_xml` is deliberately the raw wire form, not a typed
/// [`minidom::Element`]: the unacked queue is XEP-0198's retransmission
/// log — a byte-preserving record of the frames that already hit the
/// socket (see [`super::UnackedStanza`] / [`super::DetachedUnackedStanza`],
/// which model the same domain). Replay entries are the literal
/// write-to-wire frames, i.e. the I/O boundary where the typed-payloads
/// rule permits `String`. Keeping the bytes also lets
/// [`stamp_replay_delay`] fall back to verbatim (unstamped) replay when
/// an entry does not re-parse, instead of dropping an unacked stanza —
/// XEP-0198's no-loss guarantee outranks the stamp.
#[derive(Debug, Clone)]
pub struct ReplayStanza {
    /// The wire XML captured when the stanza was first sent.
    pub stanza_xml: String,
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
/// on the unacked queue's wire-XML form: the queue sits past the
/// serialization boundary, so the stanza re-enters the typed domain
/// here as a [`minidom::Element`], is annotated, and is serialized
/// back. No reason text is attached — like
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
    stanza_xml: &str,
    server_domain: &str,
    original_receipt_at: DateTime<Utc>,
) -> String {
    let Ok(mut element) = Element::from_str(stanza_xml.trim_start()) else {
        // Queue entries are produced by our own serializers, so this
        // arm should be unreachable; if it ever fires, the stanza
        // replays unstamped and the #1178 out-of-order symptom returns
        // for it — make that observable instead of silent.
        warn!(
            stanza_prefix = %stanza_xml.chars().take(120).collect::<String>(),
            "SM replay stanza is not a well-formed element; replaying unstamped"
        );
        return stanza_xml.to_string();
    };

    // XEP-0203 §2 defines delay annotations for message and presence
    // stanzas only; anything else (notably <iq/>) replays verbatim.
    if !matches!(element.name(), "message" | "presence") {
        return stanza_xml.to_string();
    }

    let already_self_stamped = element.children().any(|child| {
        child.name() == "delay"
            && child.ns() == NS_DELAY
            && child.attr("from") == Some(server_domain)
    });
    if already_self_stamped {
        return stanza_xml.to_string();
    }

    // Delays stamped by OTHER entities (an upstream server in a
    // federated chain, a MUC service) are left in place: they are the
    // recipient's delivery history. XEP-0203 §2 has each delaying
    // entity add one and only one <delay/>, and this server adds only
    // its own; stacking alongside foreign delays matches the offline
    // flush path and common ecosystem practice (MUC history + MAM).
    element.append_child(build_delay_element(&DelayInfo {
        from: Some(server_domain.to_string()),
        stamp: original_receipt_at,
        reason: None,
    }));

    match element_to_string(&element) {
        Ok(stamped) => stamped,
        Err(error) => {
            warn!(%error, "failed to serialize delay-stamped SM replay; replaying unstamped");
            stanza_xml.to_string()
        }
    }
}
