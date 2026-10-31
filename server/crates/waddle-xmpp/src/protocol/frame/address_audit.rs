//! Offline evidence from the same decoded-element conversion as live C2S parsing.
//! This inspects only top-level addresses; it does not establish identity or authority.

use jid::Jid;

use super::{
    decode_stanza, parse_stanza_element, peek_root_name, InboundFrame, ParseError, MAX_FRAME_SIZE,
};
use crate::Stanza;

/// Original XML-decoded attribute, deliberately not prepared as a JID.
/// Kept distinct from `Jid` because parsing may change or reject its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedAddress(String);

impl DecodedAddress {
    /// Original value for an explicitly requested diagnostic artifact only.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct AddressPreparation {
    pub original: Option<DecodedAddress>,
    pub prepared: Option<Jid>,
}

pub struct StanzaAddressAudit {
    pub from: AddressPreparation,
    pub to: AddressPreparation,
}

/// Inspect a bounded stanza using the live parser's actual conversion, once.
///
/// Errors can contain input material, just like the live parser's errors. Audit
/// callers must map them to static codes before logging. This function never logs.
pub fn inspect_stanza_addresses(frame: &str) -> Result<StanzaAddressAudit, ParseError> {
    if frame.len() > MAX_FRAME_SIZE {
        return Err(ParseError::TooLarge);
    }
    let frame = frame.trim();
    if frame.is_empty() {
        return Err(ParseError::Empty);
    }
    let kind = peek_root_name(frame)
        .ok_or_else(|| ParseError::InvalidXml("missing root element".to_owned()))?;
    if !matches!(kind, "iq" | "message" | "presence") {
        return Err(ParseError::UnknownRoot(kind.to_owned()));
    }
    let element = decode_stanza(frame)?;
    let original_from = element
        .attr("from")
        .map(|value| DecodedAddress(value.to_owned()));
    let original_to = element
        .attr("to")
        .map(|value| DecodedAddress(value.to_owned()));
    let parsed = parse_stanza_element(element, kind)?;
    let InboundFrame::Stanza(stanza) = parsed.frame else {
        return Err(ParseError::UnknownRoot(kind.to_owned()));
    };
    let (from, to) = match *stanza {
        Stanza::Iq(iq) => (iq.from().cloned(), iq.to().cloned()),
        Stanza::Message(message) => (message.from, message.to),
        Stanza::Presence(presence) => (presence.from, presence.to),
    };
    Ok(StanzaAddressAudit {
        from: AddressPreparation {
            original: original_from,
            prepared: from,
        },
        to: AddressPreparation {
            original: original_to,
            prepared: to,
        },
    })
}

#[cfg(test)]
mod tests;
