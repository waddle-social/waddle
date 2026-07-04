//! XEP-0203 delay stamping for XEP-0198 `<resumed/>` replay (issue
//! #1178).
//!
//! Unacked stanzas are queued as the exact wire XML captured at record
//! time. Replaying them verbatim loses the original send time — clients
//! fall back to drain-time timestamps and sort the stanzas to the tail
//! of the timeline. XEP-0198 §5 says resent stanzas should carry a
//! XEP-0203 `<delay/>` with the original timestamp; this module is the
//! pure builder that applies that stamp at the serialization boundary.

use chrono::{DateTime, Utc};
use minidom::Element;
use std::str::FromStr;

use crate::parser::element_to_string;
use crate::xep::xep0203::{build_delay_element, DelayInfo};
use crate::xep::NS_DELAY;

/// One entry of the `<resumed/>` replay set: the queued wire XML plus
/// the server-side receipt time of the original stanza, so the caller
/// can stamp the XEP-0203 `<delay/>` with the true send time.
#[derive(Debug, Clone)]
pub struct ReplayStanza {
    /// The wire XML captured when the stanza was first sent.
    pub stanza_xml: String,
    /// Server-side receipt time of the original stanza.
    pub original_receipt_at: DateTime<Utc>,
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
pub fn stamp_replay_delay(
    stanza_xml: &str,
    server_domain: &str,
    original_receipt_at: DateTime<Utc>,
) -> String {
    let Ok(mut element) = Element::from_str(stanza_xml.trim_start()) else {
        return stanza_xml.to_string();
    };

    // XEP-0203 §3 defines delay annotations for message and presence
    // stanzas only; anything else (notably <iq/>) replays verbatim.
    if !matches!(element.name(), "message" | "presence") {
        return stanza_xml.to_string();
    }

    // Strip ONLY delays this server itself stamped on an earlier path
    // (offline flush before the stanza entered the SM queue) so the
    // replay never carries two `<delay from='our-domain'/>` siblings.
    // Upstream delays are re-appended unchanged — XEP-0203 §5 allows
    // multiple delay elements and they are the recipient's delivery
    // history.
    let mut preserved = Vec::new();
    while let Some(delay) = element.remove_child("delay", NS_DELAY) {
        if delay.attr("from") != Some(server_domain) {
            preserved.push(delay);
        }
    }
    for delay in preserved {
        element.append_child(delay);
    }

    element.append_child(build_delay_element(&DelayInfo {
        from: Some(server_domain.to_string()),
        stamp: original_receipt_at,
        reason: None,
    }));

    match element_to_string(&element) {
        Ok(stamped) => stamped,
        Err(_) => stanza_xml.to_string(),
    }
}
