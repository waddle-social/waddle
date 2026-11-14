//! Hard ceilings applied by the S2S stream codec and negotiation.

use std::fmt;

/// Byte and shape ceilings enforced by [`super::StreamDecoder`].
///
/// Raw byte ceilings are enforced against bytes handed to the XML parser, so
/// none of them can be bypassed by input the parser buffers internally before
/// emitting an event. The decoded element tree is bounded separately by
/// [`Self::max_tree_bytes_per_stanza`], charged as the tree is built, because
/// decoding can copy one namespace name into every element that inherits it.
///
/// # Resident memory per connection
///
/// One decoder retains at most:
///
/// - the raw input buffer: [`Self::max_buffered_bytes`]. The unfinished
///   first-level element stays in this buffer until it completes, so it is
///   held both raw and decoded;
/// - the parser's scratch buffer, allocated up front:
///   [`Self::max_token_bytes`];
/// - the parser's stack of open element names and the decoder's namespace
///   declarations in scope, both copies of raw bytes of the stream header and
///   the unfinished element: at most [`Self::max_header_bytes`] plus
///   [`Self::max_stanza_bytes`];
/// - the strings of the decoded tree: [`Self::max_tree_bytes_per_stanza`],
///   up to twice that in allocated capacity because text nodes grow by
///   doubling as text events are appended;
/// - the tree's node structure, which is bounded by element and attribute
///   counts rather than by bytes: about 130 bytes per element plus about
///   1.1 KiB for each element that carries attributes (the B-tree leaves of
///   its attribute map). At the pilot's 1,024 elements this is about
///   1.25 MiB, and it dominates the worst case.
///
/// With [`Self::PILOT`] the worst case is therefore roughly 1.7 MiB per
/// connection, reached only by a peer that keeps one maximal first-level
/// element unfinished. Lowering [`Self::max_elements_per_stanza`] is the
/// effective lever for reducing it.
///
/// The defaults are the restricted federation pilot profile; a larger value
/// needs fresh exhaustion evidence.
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
    /// Bytes of one element name, attribute name or attribute value while
    /// the XML parser assembles it. The parser allocates a scratch buffer of
    /// this size per connection. Character data is not limited by it: the
    /// parser emits text in pieces of at most this size.
    pub max_token_bytes: usize,
    /// Bytes of one declared namespace name.
    pub max_namespace_bytes: usize,
    /// Bytes of the strings retained by the decoded tree of one first-level
    /// element: every element's local name and namespace name, every
    /// attribute's local name, namespace name and value, and all text. A
    /// namespace name is charged once per element or attribute that carries
    /// it, inherited or not, because each element stores its own copy.
    pub max_tree_bytes_per_stanza: usize,
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
        // No legitimate name or attribute value exceeds the header or
        // per-stanza attribute ceilings.
        max_token_bytes: 8 * 1024,
        max_namespace_bytes: 1024,
        // A maximal stanza's own bytes plus a short namespace name inherited
        // by each of its elements.
        max_tree_bytes_per_stanza: 128 * 1024,
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
    TokenBytes,
    NamespaceBytes,
    TreeBytes,
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
            Self::TokenBytes => "token_bytes",
            Self::NamespaceBytes => "namespace_bytes",
            Self::TreeBytes => "tree_bytes",
        }
    }
}

impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
