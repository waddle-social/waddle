//! Hard ceilings applied by the S2S stream codec and negotiation.

use std::fmt;

/// Byte and shape ceilings enforced by [`super::StreamDecoder`].
///
/// Every byte ceiling is enforced against bytes handed to the XML parser, not
/// against the size of the element tree built afterwards, so no ceiling can be
/// bypassed by input that the parser buffers internally before emitting an
/// event. The defaults are the restricted federation pilot profile; a larger
/// value needs fresh exhaustion evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecLimits {
    /// Bytes from the start of a stream (or restart) through the end of the
    /// stream header, including any XML declaration.
    pub max_header_bytes: usize,
    /// Raw bytes of one first-level element, from its `<` to its closing `>`.
    pub max_stanza_bytes: usize,
    /// Raw bytes of one first-level STARTTLS, SASL or dialback element.
    pub max_control_element_bytes: usize,
    /// Element nesting below the stream root; a first-level element has
    /// depth 1.
    pub max_depth: usize,
    /// Attributes, including namespace declarations, on one element.
    pub max_attributes_per_element: usize,
    /// Raw attribute bytes, including namespace declarations, across one
    /// first-level element and its descendants.
    pub max_attribute_bytes_per_stanza: usize,
    /// Elements in one first-level element, counting that element.
    pub max_elements_per_stanza: usize,
    /// Bytes retained by the decoder: the unfinished first-level element plus
    /// input not yet parsed.
    pub max_buffered_bytes: usize,
}

impl CodecLimits {
    /// Ceilings for the restricted federation pilot profile.
    pub const PILOT: Self = Self {
        max_header_bytes: 8 * 1024,
        max_stanza_bytes: 64 * 1024,
        max_control_element_bytes: 8 * 1024,
        max_depth: 32,
        max_attributes_per_element: 32,
        max_attribute_bytes_per_stanza: 8 * 1024,
        max_elements_per_stanza: 1024,
        max_buffered_bytes: 128 * 1024,
    };
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self::PILOT
    }
}

/// Exchange counts enforced during SASL negotiation by the receiving role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaslLimits {
    /// `<auth/>` elements accepted on one connection, counting the first.
    pub max_auth_starts: u8,
    /// `<abort/>` elements accepted on one connection.
    pub max_aborts: u8,
    /// Decoded bytes of a SASL EXTERNAL authorization identity.
    pub max_authzid_bytes: usize,
}

impl SaslLimits {
    /// Counts for the restricted federation pilot profile.
    pub const PILOT: Self = Self {
        max_auth_starts: 3,
        max_aborts: 3,
        max_authzid_bytes: 1024,
    };
}

impl Default for SaslLimits {
    fn default() -> Self {
        Self::PILOT
    }
}

/// The ceiling a peer exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitKind {
    HeaderBytes,
    StanzaBytes,
    ControlElementBytes,
    Depth,
    AttributesPerElement,
    AttributeBytes,
    ElementsPerStanza,
    BufferedBytes,
}

impl LimitKind {
    /// Stable identifier for logs and test assertions.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeaderBytes => "header_bytes",
            Self::StanzaBytes => "stanza_bytes",
            Self::ControlElementBytes => "control_element_bytes",
            Self::Depth => "depth",
            Self::AttributesPerElement => "attributes_per_element",
            Self::AttributeBytes => "attribute_bytes",
            Self::ElementsPerStanza => "elements_per_stanza",
            Self::BufferedBytes => "buffered_bytes",
        }
    }
}

impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
