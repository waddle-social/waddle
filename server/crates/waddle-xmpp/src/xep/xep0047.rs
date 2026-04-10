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
        BASE64.decode(trimmed).map_err(|_| IbbError::InvalidBase64)?
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
pub fn build_ibb_close(
    from: Option<jid::Jid>,
    to: Option<jid::Jid>,
    id: &str,
    sid: &str,
) -> Iq {
    let close = Element::builder("close", NS_IBB)
        .attr("sid", sid)
        .build();

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
    build_ibb_error(
        original_iq,
        "cancel",
        "not-acceptable",
    )
}

/// Build a `<resource-constraint/>` error for when the responder prefers a
/// smaller block-size.
pub fn build_ibb_resource_constraint(original_iq: &Iq) -> Iq {
    build_ibb_error(
        original_iq,
        "modify",
        "resource-constraint",
    )
}

/// Build an `<item-not-found/>` error for when the session ID is unknown.
pub fn build_ibb_item_not_found(original_iq: &Iq) -> Iq {
    build_ibb_error(
        original_iq,
        "cancel",
        "item-not-found",
    )
}

/// Build a `<not-acceptable/>` error for when received data violates constraints
/// (e.g., unexpected sequence number).
pub fn build_ibb_unexpected_request(original_iq: &Iq) -> Iq {
    build_ibb_error(
        original_iq,
        "cancel",
        "unexpected-request",
    )
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
            IbbError::BlockSizeTooLarge(_) => {
                XmppError::not_acceptable(Some(err.to_string()))
            }
            IbbError::InvalidBlockSize => {
                XmppError::bad_request(Some(err.to_string()))
            }
            IbbError::MissingSid => {
                XmppError::bad_request(Some(err.to_string()))
            }
            IbbError::InvalidSeq => {
                XmppError::bad_request(Some(err.to_string()))
            }
            IbbError::InvalidStanzaType(_) => {
                XmppError::bad_request(Some(err.to_string()))
            }
            IbbError::DataTooLarge { .. } => {
                XmppError::not_acceptable(Some(err.to_string()))
            }
            IbbError::InvalidBase64 => {
                XmppError::bad_request(Some(err.to_string()))
            }
            IbbError::NotIbb => {
                XmppError::bad_request(Some(err.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_iq_set(child: Element) -> Iq {
        Iq {
            from: Some("romeo@montague.net/orchard".parse().expect("valid JID")),
            to: Some("juliet@capulet.com/balcony".parse().expect("valid JID")),
            id: "test-1".to_string(),
            payload: IqType::Set(child),
        }
    }

    fn make_iq_get(child: Element) -> Iq {
        Iq {
            from: Some("romeo@montague.net/orchard".parse().expect("valid JID")),
            to: Some("juliet@capulet.com/balcony".parse().expect("valid JID")),
            id: "test-1".to_string(),
            payload: IqType::Get(child),
        }
    }

    // =========================================================================
    // Detection tests
    // =========================================================================

    #[test]
    fn test_is_ibb_open() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "test-sid")
            .attr("block-size", "4096")
            .build();
        let iq = make_iq_set(elem);
        assert!(is_ibb_open(&iq));
    }

    #[test]
    fn test_is_ibb_open_false_for_get() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "test-sid")
            .attr("block-size", "4096")
            .build();
        let iq = make_iq_get(elem);
        assert!(!is_ibb_open(&iq));
    }

    #[test]
    fn test_is_ibb_open_false_for_wrong_ns() {
        let elem = Element::builder("open", "some:other:ns")
            .attr("sid", "test-sid")
            .build();
        let iq = make_iq_set(elem);
        assert!(!is_ibb_open(&iq));
    }

    #[test]
    fn test_is_ibb_data() {
        let elem = Element::builder("data", NS_IBB)
            .attr("sid", "test-sid")
            .attr("seq", "0")
            .build();
        let iq = make_iq_set(elem);
        assert!(is_ibb_data(&iq));
    }

    #[test]
    fn test_is_ibb_close() {
        let elem = Element::builder("close", NS_IBB)
            .attr("sid", "test-sid")
            .build();
        let iq = make_iq_set(elem);
        assert!(is_ibb_close(&iq));
    }

    #[test]
    fn test_message_has_ibb_data() {
        let data_elem = Element::builder("data", NS_IBB)
            .attr("sid", "test-sid")
            .attr("seq", "0")
            .build();
        let msg = Element::builder("message", "jabber:client")
            .append(data_elem)
            .build();
        assert!(message_has_ibb_data(&msg));
    }

    #[test]
    fn test_message_has_no_ibb_data() {
        let msg = Element::builder("message", "jabber:client")
            .append(Element::builder("body", "jabber:client").build())
            .build();
        assert!(!message_has_ibb_data(&msg));
    }

    // =========================================================================
    // Parsing tests
    // =========================================================================

    #[test]
    fn test_parse_ibb_open_default_stanza() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "i781hf64")
            .attr("block-size", "4096")
            .build();
        let iq = make_iq_set(elem);

        let open = parse_ibb_open(&iq).expect("should parse");
        assert_eq!(open.sid, "i781hf64");
        assert_eq!(open.block_size, 4096);
        assert_eq!(open.stanza, StanzaType::Iq);
    }

    #[test]
    fn test_parse_ibb_open_message_stanza() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "sess1")
            .attr("block-size", "1024")
            .attr("stanza", "message")
            .build();
        let iq = make_iq_set(elem);

        let open = parse_ibb_open(&iq).expect("should parse");
        assert_eq!(open.stanza, StanzaType::Message);
    }

    #[test]
    fn test_parse_ibb_open_max_block_size() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "max-test")
            .attr("block-size", "65535")
            .build();
        let iq = make_iq_set(elem);

        let open = parse_ibb_open(&iq).expect("should parse");
        assert_eq!(open.block_size, 65535);
    }

    #[test]
    fn test_parse_ibb_open_block_size_too_large() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "big")
            .attr("block-size", "65536")
            .build();
        let iq = make_iq_set(elem);

        let err = parse_ibb_open(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::BlockSizeTooLarge(65536));
    }

    #[test]
    fn test_parse_ibb_open_zero_block_size() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "zero")
            .attr("block-size", "0")
            .build();
        let iq = make_iq_set(elem);

        let err = parse_ibb_open(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::InvalidBlockSize);
    }

    #[test]
    fn test_parse_ibb_open_missing_sid() {
        let elem = Element::builder("open", NS_IBB)
            .attr("block-size", "4096")
            .build();
        let iq = make_iq_set(elem);

        let err = parse_ibb_open(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::MissingSid);
    }

    #[test]
    fn test_parse_ibb_open_missing_block_size() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "no-bs")
            .build();
        let iq = make_iq_set(elem);

        let err = parse_ibb_open(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::InvalidBlockSize);
    }

    #[test]
    fn test_parse_ibb_open_invalid_stanza_type() {
        let elem = Element::builder("open", NS_IBB)
            .attr("sid", "bad-stanza")
            .attr("block-size", "4096")
            .attr("stanza", "presence")
            .build();
        let iq = make_iq_set(elem);

        let err = parse_ibb_open(&iq).expect_err("should fail");
        assert_eq!(
            err,
            IbbError::InvalidStanzaType("presence".to_string())
        );
    }

    #[test]
    fn test_parse_ibb_data() {
        let raw_data = b"Hello, World!";
        let encoded = BASE64.encode(raw_data);

        let mut data_elem = Element::builder("data", NS_IBB)
            .attr("sid", "i781hf64")
            .attr("seq", "0")
            .build();
        data_elem.append_text_node(encoded);

        let iq = make_iq_set(data_elem);
        let data = parse_ibb_data_from_iq(&iq).expect("should parse");

        assert_eq!(data.sid, "i781hf64");
        assert_eq!(data.seq, 0);
        assert_eq!(data.data, raw_data);
    }

    #[test]
    fn test_parse_ibb_data_empty() {
        let data_elem = Element::builder("data", NS_IBB)
            .attr("sid", "empty-test")
            .attr("seq", "5")
            .build();
        let iq = make_iq_set(data_elem);

        let data = parse_ibb_data_from_iq(&iq).expect("should parse");
        assert_eq!(data.sid, "empty-test");
        assert_eq!(data.seq, 5);
        assert!(data.data.is_empty());
    }

    #[test]
    fn test_parse_ibb_data_max_seq() {
        let data_elem = Element::builder("data", NS_IBB)
            .attr("sid", "seq-test")
            .attr("seq", "65535")
            .build();
        let iq = make_iq_set(data_elem);

        let data = parse_ibb_data_from_iq(&iq).expect("should parse");
        assert_eq!(data.seq, 65535);
    }

    #[test]
    fn test_parse_ibb_data_missing_sid() {
        let data_elem = Element::builder("data", NS_IBB)
            .attr("seq", "0")
            .build();
        let iq = make_iq_set(data_elem);

        let err = parse_ibb_data_from_iq(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::MissingSid);
    }

    #[test]
    fn test_parse_ibb_data_missing_seq() {
        let data_elem = Element::builder("data", NS_IBB)
            .attr("sid", "no-seq")
            .build();
        let iq = make_iq_set(data_elem);

        let err = parse_ibb_data_from_iq(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::InvalidSeq);
    }

    #[test]
    fn test_parse_ibb_data_invalid_base64() {
        let mut data_elem = Element::builder("data", NS_IBB)
            .attr("sid", "bad-b64")
            .attr("seq", "0")
            .build();
        data_elem.append_text_node("not-valid-base64!!!");

        let iq = make_iq_set(data_elem);
        let err = parse_ibb_data_from_iq(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::InvalidBase64);
    }

    #[test]
    fn test_parse_ibb_data_from_message() {
        let raw_data = b"message transport";
        let encoded = BASE64.encode(raw_data);

        let mut data_elem = Element::builder("data", NS_IBB)
            .attr("sid", "msg-test")
            .attr("seq", "3")
            .build();
        data_elem.append_text_node(encoded);

        let msg = Element::builder("message", "jabber:client")
            .append(data_elem)
            .build();

        let data = parse_ibb_data_from_message(&msg).expect("should parse");
        assert_eq!(data.sid, "msg-test");
        assert_eq!(data.seq, 3);
        assert_eq!(data.data, raw_data);
    }

    #[test]
    fn test_parse_ibb_close() {
        let close_elem = Element::builder("close", NS_IBB)
            .attr("sid", "i781hf64")
            .build();
        let iq = make_iq_set(close_elem);

        let close = parse_ibb_close(&iq).expect("should parse");
        assert_eq!(close.sid, "i781hf64");
    }

    #[test]
    fn test_parse_ibb_close_missing_sid() {
        let close_elem = Element::builder("close", NS_IBB).build();
        let iq = make_iq_set(close_elem);

        let err = parse_ibb_close(&iq).expect_err("should fail");
        assert_eq!(err, IbbError::MissingSid);
    }

    // =========================================================================
    // Building tests
    // =========================================================================

    #[test]
    fn test_build_ibb_result() {
        let open_elem = Element::builder("open", NS_IBB)
            .attr("sid", "test")
            .attr("block-size", "4096")
            .build();
        let iq = make_iq_set(open_elem);

        let result = build_ibb_result(&iq);
        assert_eq!(result.id, "test-1");
        assert_eq!(result.from, iq.to);
        assert_eq!(result.to, iq.from);
        assert!(matches!(result.payload, IqType::Result(None)));
    }

    #[test]
    fn test_build_ibb_open() {
        let iq = build_ibb_open(
            Some("romeo@montague.net/orchard".parse().expect("valid JID")),
            Some("juliet@capulet.com/balcony".parse().expect("valid JID")),
            "open-1",
            "session-abc",
            4096,
            StanzaType::Iq,
        );

        assert_eq!(iq.id, "open-1");
        if let IqType::Set(elem) = &iq.payload {
            assert_eq!(elem.name(), "open");
            assert_eq!(elem.ns(), NS_IBB);
            assert_eq!(elem.attr("sid"), Some("session-abc"));
            assert_eq!(elem.attr("block-size"), Some("4096"));
            assert_eq!(elem.attr("stanza"), Some("iq"));
        } else {
            panic!("Expected IQ set");
        }
    }

    #[test]
    fn test_build_ibb_data_iq() {
        let raw = b"Hello!";
        let iq = build_ibb_data_iq(
            Some("romeo@montague.net".parse().expect("valid JID")),
            Some("juliet@capulet.com".parse().expect("valid JID")),
            "data-1",
            "session-abc",
            0,
            raw,
        );

        if let IqType::Set(elem) = &iq.payload {
            assert_eq!(elem.name(), "data");
            assert_eq!(elem.ns(), NS_IBB);
            assert_eq!(elem.attr("sid"), Some("session-abc"));
            assert_eq!(elem.attr("seq"), Some("0"));

            let decoded = BASE64.decode(elem.text().trim()).expect("valid base64");
            assert_eq!(decoded, raw);
        } else {
            panic!("Expected IQ set");
        }
    }

    #[test]
    fn test_build_ibb_data_element() {
        let raw = b"chunk data";
        let elem = build_ibb_data_element("sid-1", 42, raw);

        assert_eq!(elem.name(), "data");
        assert_eq!(elem.ns(), NS_IBB);
        assert_eq!(elem.attr("sid"), Some("sid-1"));
        assert_eq!(elem.attr("seq"), Some("42"));

        let decoded = BASE64.decode(elem.text().trim()).expect("valid base64");
        assert_eq!(decoded, raw);
    }

    #[test]
    fn test_build_ibb_close() {
        let iq = build_ibb_close(
            Some("romeo@montague.net".parse().expect("valid JID")),
            Some("juliet@capulet.com".parse().expect("valid JID")),
            "close-1",
            "session-abc",
        );

        if let IqType::Set(elem) = &iq.payload {
            assert_eq!(elem.name(), "close");
            assert_eq!(elem.ns(), NS_IBB);
            assert_eq!(elem.attr("sid"), Some("session-abc"));
        } else {
            panic!("Expected IQ set");
        }
    }

    // =========================================================================
    // Error response tests
    // =========================================================================

    #[test]
    fn test_build_ibb_not_acceptable() {
        let open_elem = Element::builder("open", NS_IBB)
            .attr("sid", "rejected")
            .attr("block-size", "4096")
            .build();
        let iq = make_iq_set(open_elem);

        let err_iq = build_ibb_not_acceptable(&iq);
        assert_eq!(err_iq.id, "test-1");
        assert!(matches!(err_iq.payload, IqType::Error(_)));
    }

    #[test]
    fn test_build_ibb_resource_constraint() {
        let open_elem = Element::builder("open", NS_IBB)
            .attr("sid", "too-big")
            .attr("block-size", "65535")
            .build();
        let iq = make_iq_set(open_elem);

        let err_iq = build_ibb_resource_constraint(&iq);
        assert!(matches!(err_iq.payload, IqType::Error(_)));
    }

    #[test]
    fn test_build_ibb_item_not_found() {
        let close_elem = Element::builder("close", NS_IBB)
            .attr("sid", "unknown")
            .build();
        let iq = make_iq_set(close_elem);

        let err_iq = build_ibb_item_not_found(&iq);
        assert!(matches!(err_iq.payload, IqType::Error(_)));
    }

    // =========================================================================
    // Validation tests
    // =========================================================================

    #[test]
    fn test_validate_data_size_ok() {
        let data = IbbData {
            sid: "test".to_string(),
            seq: 0,
            data: vec![0u8; 4096],
        };
        assert!(validate_data_size(&data, 4096).is_ok());
    }

    #[test]
    fn test_validate_data_size_too_large() {
        let data = IbbData {
            sid: "test".to_string(),
            seq: 0,
            data: vec![0u8; 4097],
        };
        let err = validate_data_size(&data, 4096).expect_err("should fail");
        assert_eq!(
            err,
            IbbError::DataTooLarge {
                actual: 4097,
                limit: 4096
            }
        );
    }

    #[test]
    fn test_validate_data_size_empty() {
        let data = IbbData {
            sid: "test".to_string(),
            seq: 0,
            data: Vec::new(),
        };
        assert!(validate_data_size(&data, 4096).is_ok());
    }

    // =========================================================================
    // Sequence number tests
    // =========================================================================

    #[test]
    fn test_next_seq_normal() {
        assert_eq!(next_seq(0), 1);
        assert_eq!(next_seq(100), 101);
    }

    #[test]
    fn test_next_seq_wraps() {
        assert_eq!(next_seq(65535), 0);
    }

    // =========================================================================
    // StanzaType tests
    // =========================================================================

    #[test]
    fn test_stanza_type_from_attr() {
        assert_eq!(StanzaType::from_attr(None).expect("ok"), StanzaType::Iq);
        assert_eq!(
            StanzaType::from_attr(Some("iq")).expect("ok"),
            StanzaType::Iq
        );
        assert_eq!(
            StanzaType::from_attr(Some("message")).expect("ok"),
            StanzaType::Message
        );
        assert!(StanzaType::from_attr(Some("presence")).is_err());
    }

    #[test]
    fn test_stanza_type_as_str() {
        assert_eq!(StanzaType::Iq.as_str(), "iq");
        assert_eq!(StanzaType::Message.as_str(), "message");
    }

    // =========================================================================
    // IbbError Display / From<IbbError> tests
    // =========================================================================

    #[test]
    fn test_ibb_error_display() {
        assert!(IbbError::MissingSid.to_string().contains("missing session ID"));
        assert!(IbbError::InvalidBase64.to_string().contains("base64"));
        assert!(
            IbbError::BlockSizeTooLarge(70000)
                .to_string()
                .contains("70000")
        );
    }

    #[test]
    fn test_ibb_error_to_xmpp_error() {
        let xmpp_err: XmppError = IbbError::MissingSid.into();
        assert!(matches!(
            xmpp_err,
            XmppError::Stanza {
                condition: crate::StanzaErrorCondition::BadRequest,
                ..
            }
        ));

        let xmpp_err: XmppError = IbbError::BlockSizeTooLarge(99999).into();
        assert!(matches!(
            xmpp_err,
            XmppError::Stanza {
                condition: crate::StanzaErrorCondition::NotAcceptable,
                ..
            }
        ));
    }

    // =========================================================================
    // Roundtrip tests
    // =========================================================================

    #[test]
    fn test_roundtrip_open() {
        let iq = build_ibb_open(
            Some("alice@example.com".parse().expect("valid JID")),
            Some("bob@example.com".parse().expect("valid JID")),
            "rt-1",
            "roundtrip-sid",
            8192,
            StanzaType::Message,
        );

        let parsed = parse_ibb_open(&iq).expect("should parse");
        assert_eq!(parsed.sid, "roundtrip-sid");
        assert_eq!(parsed.block_size, 8192);
        assert_eq!(parsed.stanza, StanzaType::Message);
    }

    #[test]
    fn test_roundtrip_data() {
        let payload = b"The quick brown fox jumps over the lazy dog";
        let iq = build_ibb_data_iq(
            Some("alice@example.com".parse().expect("valid JID")),
            Some("bob@example.com".parse().expect("valid JID")),
            "rt-2",
            "roundtrip-sid",
            42,
            payload,
        );

        let parsed = parse_ibb_data_from_iq(&iq).expect("should parse");
        assert_eq!(parsed.sid, "roundtrip-sid");
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.data, payload);
    }

    #[test]
    fn test_roundtrip_close() {
        let iq = build_ibb_close(
            Some("alice@example.com".parse().expect("valid JID")),
            Some("bob@example.com".parse().expect("valid JID")),
            "rt-3",
            "roundtrip-sid",
        );

        let parsed = parse_ibb_close(&iq).expect("should parse");
        assert_eq!(parsed.sid, "roundtrip-sid");
    }

    #[test]
    fn test_roundtrip_data_element_in_message() {
        let payload = b"message-based data";
        let data_elem = build_ibb_data_element("msg-sid", 7, payload);

        let msg = Element::builder("message", "jabber:client")
            .append(data_elem)
            .build();

        assert!(message_has_ibb_data(&msg));
        let parsed = parse_ibb_data_from_message(&msg).expect("should parse");
        assert_eq!(parsed.sid, "msg-sid");
        assert_eq!(parsed.seq, 7);
        assert_eq!(parsed.data, payload);
    }
}
