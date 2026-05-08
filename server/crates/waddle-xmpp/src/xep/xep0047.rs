//! XEP-0047: In-Band Bytestreams
//!
//! Enables two entities to establish a one-to-one bytestream where data is
//! broken into smaller chunks and transported in-band over XMPP using
//! base64-encoded IQ or message stanzas.
//!
//! ## Protocol Flow
//!
//! 1. **Open**: Initiator sends `<open/>` IQ-set to create a session.
//! 2. **Data**: Either party sends `<data/>` IQ-set (or message) with base64 chunks.
//! 3. **Close**: Either party sends `<close/>` IQ-set to tear down the session.
//!
//! ## Service Discovery
//!
//! Advertises `http://jabber.org/protocol/ibb` as a feature in disco#info.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

use crate::XmppError;

/// Namespace for XEP-0047 In-Band Bytestreams.
pub const NS_IBB: &str = "http://jabber.org/protocol/ibb";

/// Maximum allowed block size per the spec (65535 bytes).
pub const MAX_BLOCK_SIZE: u32 = 65535;

/// Maximum sequence number (16-bit unsigned, wraps to 0 after 65535).
pub const MAX_SEQ: u16 = u16::MAX;

/// Transport stanza type for IBB data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaType {
    /// Use IQ stanzas for data transport (default, recommended).
    Iq,
    /// Use message stanzas for data transport.
    Message,
}

impl StanzaType {
    /// Parse from the `stanza` attribute value.
    pub fn from_attr(attr: Option<&str>) -> Result<Self, IbbError> {
        match attr {
            None | Some("iq") => Ok(Self::Iq),
            Some("message") => Ok(Self::Message),
            Some(other) => Err(IbbError::InvalidStanzaType(other.to_string())),
        }
    }

    /// Return the attribute value string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Iq => "iq",
            Self::Message => "message",
        }
    }
}

/// Errors specific to IBB operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IbbError {
    /// The block-size exceeds the maximum (65535).
    BlockSizeTooLarge(u32),
    /// The block-size is missing or zero.
    InvalidBlockSize,
    /// The session ID is missing.
    MissingSid,
    /// The sequence number is missing or invalid.
    InvalidSeq,
    /// Invalid stanza type attribute.
    InvalidStanzaType(String),
    /// Data exceeds the negotiated block-size.
    DataTooLarge {
        /// Actual decoded data size in bytes.
        actual: usize,
        /// Negotiated block-size limit.
        limit: u32,
    },
    /// Invalid base64 data.
    InvalidBase64,
    /// Not an IBB stanza.
    NotIbb,
}

impl std::fmt::Display for IbbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlockSizeTooLarge(size) => {
                write!(f, "block-size {} exceeds maximum {}", size, MAX_BLOCK_SIZE)
            }
            Self::InvalidBlockSize => write!(f, "invalid or missing block-size"),
            Self::MissingSid => write!(f, "missing session ID (sid)"),
            Self::InvalidSeq => write!(f, "invalid or missing sequence number"),
            Self::InvalidStanzaType(s) => write!(f, "invalid stanza type: {}", s),
            Self::DataTooLarge { actual, limit } => {
                write!(f, "data size {} exceeds block-size {}", actual, limit)
            }
            Self::InvalidBase64 => write!(f, "invalid base64 data"),
            Self::NotIbb => write!(f, "not an IBB stanza"),
        }
    }
}

impl std::error::Error for IbbError {}

/// Parsed IBB `<open/>` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IbbOpen {
    /// Unique session ID.
    pub sid: String,
    /// Maximum data chunk size in bytes (before base64 encoding).
    pub block_size: u32,
    /// Transport stanza type.
    pub stanza: StanzaType,
}

/// Parsed IBB `<data/>` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IbbData {
    /// Session ID this data belongs to.
    pub sid: String,
    /// Sequence number (0..65535, wraps around).
    pub seq: u16,
    /// The decoded binary data.
    pub data: Vec<u8>,
}

/// Parsed IBB `<close/>` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IbbClose {
    /// Session ID to close.
    pub sid: String,
}

// =============================================================================
// Detection helpers
// =============================================================================

/// Check if an IQ stanza is an IBB `<open/>` request (IQ-set).
pub fn is_ibb_open(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.name() == "open" && elem.ns() == NS_IBB)
}

/// Check if an IQ stanza is an IBB `<data/>` stanza (IQ-set).
pub fn is_ibb_data(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.name() == "data" && elem.ns() == NS_IBB)
}

/// Check if an IQ stanza is an IBB `<close/>` request (IQ-set).
pub fn is_ibb_close(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.name() == "close" && elem.ns() == NS_IBB)
}

/// Check if a message stanza contains an IBB `<data/>` element.
pub fn message_has_ibb_data(message: &Element) -> bool {
    message
        .children()
        .any(|child| child.name() == "data" && child.ns() == NS_IBB)
}

// =============================================================================
// Parsing
// =============================================================================

/// Parse an IBB `<open/>` from an IQ stanza.
pub fn parse_ibb_open(iq: &Iq) -> Result<IbbOpen, IbbError> {
    let elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "open" && elem.ns() == NS_IBB => elem,
        _ => return Err(IbbError::NotIbb),
    };

    let sid = elem
        .attr("sid")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or(IbbError::MissingSid)?;

    let block_size: u32 = elem
        .attr("block-size")
        .and_then(|s| s.parse().ok())
        .filter(|&bs| bs > 0)
        .ok_or(IbbError::InvalidBlockSize)?;

    if block_size > MAX_BLOCK_SIZE {
        return Err(IbbError::BlockSizeTooLarge(block_size));
    }

    let stanza = StanzaType::from_attr(elem.attr("stanza"))?;

    Ok(IbbOpen {
        sid,
        block_size,
        stanza,
    })
}

/// Parse an IBB `<data/>` from an IQ stanza.
pub fn parse_ibb_data_from_iq(iq: &Iq) -> Result<IbbData, IbbError> {
    let elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "data" && elem.ns() == NS_IBB => elem,
        _ => return Err(IbbError::NotIbb),
    };

    parse_ibb_data_element(elem)
}

/// Parse an IBB `<data/>` from a message stanza's child element.
pub fn parse_ibb_data_from_message(message: &Element) -> Result<IbbData, IbbError> {
    let elem = message
        .children()
        .find(|child| child.name() == "data" && child.ns() == NS_IBB)
        .ok_or(IbbError::NotIbb)?;

    parse_ibb_data_element(elem)
}

/// Parse an IBB `<data/>` element (shared between IQ and message transport).
fn parse_ibb_data_element(elem: &Element) -> Result<IbbData, IbbError> {
    let sid = elem
        .attr("sid")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or(IbbError::MissingSid)?;

    let seq: u16 = elem
        .attr("seq")
        .and_then(|s| s.parse().ok())
        .ok_or(IbbError::InvalidSeq)?;

    let base64_text = elem.text();
    let trimmed = base64_text.trim();

    let data = if trimmed.is_empty() {
        Vec::new()
    } else {
        BASE64
            .decode(trimmed)
            .map_err(|_| IbbError::InvalidBase64)?
    };

    Ok(IbbData { sid, seq, data })
}

/// Parse an IBB `<close/>` from an IQ stanza.
pub fn parse_ibb_close(iq: &Iq) -> Result<IbbClose, IbbError> {
    let elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "close" && elem.ns() == NS_IBB => elem,
        _ => return Err(IbbError::NotIbb),
    };

    let sid = elem
        .attr("sid")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or(IbbError::MissingSid)?;

    Ok(IbbClose { sid })
}

// =============================================================================
// Building responses
// =============================================================================

/// Build an IQ-result acknowledging an IBB open, data, or close.
pub fn build_ibb_result(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(None),
    }
}

/// Build an IBB `<open/>` IQ-set stanza.
pub fn build_ibb_open(
    from: Option<jid::Jid>,
    to: Option<jid::Jid>,
    id: &str,
    sid: &str,
    block_size: u32,
    stanza: StanzaType,
) -> Iq {
    let open = Element::builder("open", NS_IBB)
        .attr("sid", sid)
        .attr("block-size", block_size.to_string())
        .attr("stanza", stanza.as_str())
        .build();

    Iq {
        from,
        to,
        id: id.to_string(),
        payload: IqType::Set(open),
    }
}

/// Build an IBB `<data/>` IQ-set stanza.
///
/// The `data` parameter is raw bytes; it will be base64-encoded automatically.
pub fn build_ibb_data_iq(
    from: Option<jid::Jid>,
    to: Option<jid::Jid>,
    id: &str,
    sid: &str,
    seq: u16,
    data: &[u8],
) -> Iq {
    let encoded = BASE64.encode(data);

    let mut data_elem = Element::builder("data", NS_IBB)
        .attr("sid", sid)
        .attr("seq", seq.to_string())
        .build();
    data_elem.append_text_node(encoded);

    Iq {
        from,
        to,
        id: id.to_string(),
        payload: IqType::Set(data_elem),
    }
}

/// Build an IBB `<data/>` element for embedding in a message stanza.
pub fn build_ibb_data_element(sid: &str, seq: u16, data: &[u8]) -> Element {
    let encoded = BASE64.encode(data);

    let mut data_elem = Element::builder("data", NS_IBB)
        .attr("sid", sid)
        .attr("seq", seq.to_string())
        .build();
    data_elem.append_text_node(encoded);

    data_elem
}

/// Build an IBB `<close/>` IQ-set stanza.
pub fn build_ibb_close(from: Option<jid::Jid>, to: Option<jid::Jid>, id: &str, sid: &str) -> Iq {
    let close = Element::builder("close", NS_IBB).attr("sid", sid).build();

    Iq {
        from,
        to,
        id: id.to_string(),
        payload: IqType::Set(close),
    }
}

// =============================================================================
// Error responses
// =============================================================================

/// Build a `<not-acceptable/>` error for when the responder rejects an IBB open.
pub fn build_ibb_not_acceptable(original_iq: &Iq) -> Iq {
    build_ibb_error(original_iq, "cancel", "not-acceptable")
}

/// Build a `<resource-constraint/>` error for when the responder prefers a
/// smaller block-size.
pub fn build_ibb_resource_constraint(original_iq: &Iq) -> Iq {
    build_ibb_error(original_iq, "modify", "resource-constraint")
}

/// Build an `<item-not-found/>` error for when the session ID is unknown.
pub fn build_ibb_item_not_found(original_iq: &Iq) -> Iq {
    build_ibb_error(original_iq, "cancel", "item-not-found")
}

/// Build a `<not-acceptable/>` error for when received data violates constraints
/// (e.g., unexpected sequence number).
pub fn build_ibb_unexpected_request(original_iq: &Iq) -> Iq {
    build_ibb_error(original_iq, "cancel", "unexpected-request")
}

/// Build a generic IBB error response.
fn build_ibb_error(original_iq: &Iq, error_type: &str, condition: &str) -> Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let et = match error_type {
        "modify" => ErrorType::Modify,
        "cancel" => ErrorType::Cancel,
        "auth" => ErrorType::Auth,
        "wait" => ErrorType::Wait,
        _ => ErrorType::Cancel,
    };

    let dc = match condition {
        "not-acceptable" => DefinedCondition::NotAcceptable,
        "resource-constraint" => DefinedCondition::ResourceConstraint,
        "item-not-found" => DefinedCondition::ItemNotFound,
        "unexpected-request" => DefinedCondition::UnexpectedRequest,
        "service-unavailable" => DefinedCondition::ServiceUnavailable,
        _ => DefinedCondition::UndefinedCondition,
    };

    let stanza_error = StanzaError::new(et, dc, "en", "");

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Error(stanza_error),
    }
}

// =============================================================================
// Validation helpers
// =============================================================================

/// Validate that a data chunk does not exceed the negotiated block-size.
pub fn validate_data_size(data: &IbbData, block_size: u32) -> Result<(), IbbError> {
    let len = data.data.len();
    if len > block_size as usize {
        return Err(IbbError::DataTooLarge {
            actual: len,
            limit: block_size,
        });
    }
    Ok(())
}

/// Compute the next expected sequence number (wraps at 65535 -> 0).
pub fn next_seq(current: u16) -> u16 {
    current.wrapping_add(1)
}

// =============================================================================
// Conversion to XmppError
// =============================================================================

impl From<IbbError> for XmppError {
    fn from(err: IbbError) -> Self {
        match err {
            IbbError::BlockSizeTooLarge(_) => XmppError::not_acceptable(Some(err.to_string())),
            IbbError::InvalidBlockSize => XmppError::bad_request(Some(err.to_string())),
            IbbError::MissingSid => XmppError::bad_request(Some(err.to_string())),
            IbbError::InvalidSeq => XmppError::bad_request(Some(err.to_string())),
            IbbError::InvalidStanzaType(_) => XmppError::bad_request(Some(err.to_string())),
            IbbError::DataTooLarge { .. } => XmppError::not_acceptable(Some(err.to_string())),
            IbbError::InvalidBase64 => XmppError::bad_request(Some(err.to_string())),
            IbbError::NotIbb => XmppError::bad_request(Some(err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests;
