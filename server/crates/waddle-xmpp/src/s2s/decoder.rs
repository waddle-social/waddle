//! Bounded incremental decoder for `jabber:server` XML streams.
//!
//! The decoder drives `rxml`'s non-validating [`RawParser`] (the same parser
//! version the vendored `minidom` tree builder uses) and performs namespace
//! resolution itself. The raw parser emits one event per attribute and per
//! element head, so attribute, element and depth ceilings are checked before
//! the corresponding state is retained, and namespace prefixes stay visible
//! for the stream-prefix and content-namespace prefix rules of RFC 6120
//! §4.8.5. The C2S WebSocket classifier in `protocol::frame` parses complete
//! frames with a client content namespace and cannot bound an unframed byte
//! stream, so it is not reused here.
//!
//! Bytes are parsed only when [`StreamDecoder::next_item`] is called and
//! parsing stops at the end of each first-level item. Every parser event
//! reports how many input bytes produced it, so after a first-level element
//! the decoder knows the exact offset where that element ended. Stream
//! restarts use that offset: bytes after `<proceed/>` are handed to the TLS
//! engine and bytes after SASL success are discarded, and neither can reach
//! the XML parser of the next stream.
//!
//! Decoding can amplify input: `minidom` stores an owned namespace name in
//! every element, so a short element inheriting a long default namespace
//! costs far more memory than its raw bytes. Declared namespace names are
//! therefore capped, declarations in scope are shared rather than copied
//! into attribute keys, and the strings retained by each first-level element
//! are charged against [`CodecLimits::max_tree_bytes_per_stanza`] before
//! they are stored.

use minidom::rxml::error::{EndOrError, Error as XmlError};
use minidom::rxml::{AttrMap, Namespace, NcName, Options, Parse, RawEvent, RawParser, WithOptions};
use minidom::Element;
use thiserror::Error;

use crate::ns::{JABBER_CLIENT, JABBER_SERVER, JABBER_SERVER_DIALBACK, SASL, STREAM, TLS};

use super::condition::StreamErrorCondition;
use super::header::{HeaderAddress, StreamHeader, StreamId, StreamLanguage, StreamVersion};
use super::limits::{CodecLimits, LimitKind};

/// The only stream namespace prefix accepted (RFC 6120 §4.8.5).
const STREAM_PREFIX: &str = "stream";
/// The stream root element name.
const STREAM_ROOT: &str = "stream";
/// Prefix bound to the XML namespace without declaration.
const XML_PREFIX: &str = "xml";
/// Unprefixed attribute that declares the default namespace.
const XMLNS: &str = "xmlns";
/// Compact consumed input once this many bytes are no longer needed.
const COMPACT_THRESHOLD: usize = 4096;
/// Bytes inspected before a parser error to recognise restricted markup.
const RESTRICTED_MARKUP_WINDOW: usize = 16;

/// One complete item decoded from the stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// The stream header of a new stream.
    Header(StreamHeader),
    /// A complete first-level element: a stanza or a negotiation element.
    Element(Element),
    /// The peer closed its stream with `</stream:stream>`.
    End,
}

/// Why decoding stopped. Decoding errors are terminal for the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum DecodeError {
    #[error("XML is not well-formed or not namespace-well-formed")]
    NotWellFormed,
    #[error("XML uses a construct XMPP prohibits")]
    RestrictedXml,
    #[error("input is not UTF-8")]
    UnsupportedEncoding,
    #[error("stream or content namespace is not supported")]
    InvalidNamespace,
    #[error("namespace prefix is not supported")]
    BadNamespacePrefix,
    #[error("stream header or stream-level content cannot be processed")]
    BadFormat,
    #[error("peer exceeded the {0} limit")]
    LimitExceeded(LimitKind),
}

impl DecodeError {
    /// The stream error condition reported to the peer.
    pub fn condition(self) -> StreamErrorCondition {
        match self {
            Self::NotWellFormed => StreamErrorCondition::NotWellFormed,
            Self::RestrictedXml => StreamErrorCondition::RestrictedXml,
            Self::UnsupportedEncoding => StreamErrorCondition::UnsupportedEncoding,
            Self::InvalidNamespace => StreamErrorCondition::InvalidNamespace,
            Self::BadNamespacePrefix => StreamErrorCondition::BadNamespacePrefix,
            Self::BadFormat => StreamErrorCondition::BadFormat,
            Self::LimitExceeded(_) => StreamErrorCondition::PolicyViolation,
        }
    }
}

/// Input buffered after `<proceed/>`, owned by the TLS engine.
///
/// These bytes were received on the plaintext connection after the end of
/// the element that started TLS. They are TLS records (or an attack), never
/// XML, and must be passed to the TLS engine before any further socket reads.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "buffered bytes after <proceed/> belong to the TLS engine"]
pub struct TlsHandoff(Vec<u8>);

impl TlsHandoff {
    /// Number of buffered bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no bytes were buffered.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Take ownership of the buffered bytes to feed them to the TLS engine.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Input that arrived after a SASL success boundary and was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "input received before the post-SASL stream restart is a protocol violation"]
pub struct DiscardedInput {
    bytes: usize,
}

impl DiscardedInput {
    /// Number of dropped bytes.
    pub fn bytes(self) -> usize {
        self.bytes
    }

    /// Whether nothing was dropped.
    pub fn is_empty(self) -> bool {
        self.bytes == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Header,
    Open,
    Ended,
    Failed(DecodeError),
}

/// Namespace declarations of one open element.
#[derive(Debug, Default)]
struct Scope {
    /// `Some` when this element declares the default namespace.
    default: Option<Namespace>,
    prefixes: Vec<(NcName, Namespace)>,
}

/// An element head whose attributes are still arriving.
#[derive(Debug)]
struct PendingHead {
    prefix: Option<NcName>,
    local: NcName,
    default_declaration: Option<String>,
    prefix_declarations: Vec<(NcName, String)>,
    attributes: Vec<(Option<NcName>, NcName, String)>,
    attribute_count: usize,
}

/// The first-level element being decoded.
#[derive(Debug)]
struct StanzaInProgress {
    /// Absolute input offset of the element's `<`.
    start: usize,
    limit: usize,
    limit_kind: LimitKind,
    elements: usize,
    attribute_bytes: usize,
    /// Bytes charged against [`CodecLimits::max_tree_bytes_per_stanza`].
    tree_bytes: usize,
    /// Open elements; index 0 is the first-level element.
    stack: Vec<Element>,
}

impl StanzaInProgress {
    /// Account for `bytes` of strings about to be stored in the tree.
    fn charge(&mut self, bytes: usize, limit: usize) -> Result<(), DecodeError> {
        self.tree_bytes = self.tree_bytes.saturating_add(bytes);
        if self.tree_bytes > limit {
            return Err(DecodeError::LimitExceeded(LimitKind::TreeBytes));
        }
        Ok(())
    }
}

/// Incremental, bounded decoder for one direction of an S2S connection.
///
/// Offsets below are absolute byte positions since the last (re)start.
#[derive(Debug)]
pub struct StreamDecoder {
    limits: CodecLimits,
    parser: RawParser,
    /// Retained input; `buffer[0]` is at absolute offset `base`.
    buffer: Vec<u8>,
    base: usize,
    /// Input consumed by the parser.
    parsed: usize,
    /// Input accounted to emitted parser events.
    emitted: usize,
    /// End of the last item returned by [`Self::next_item`].
    boundary: usize,
    phase: Phase,
    scopes: Vec<Scope>,
    head: Option<PendingHead>,
    stanza: Option<StanzaInProgress>,
    whitespace_bytes: u64,
}

impl StreamDecoder {
    /// A decoder awaiting a stream header.
    pub fn new(limits: CodecLimits) -> Self {
        let options = Options {
            max_token_length: limits.max_token_bytes,
            ..Options::default()
        };
        let mut parser = RawParser::with_options(options);
        // Emit character data as soon as it is read so stream-level
        // whitespace is accounted and released immediately instead of being
        // held in the parser until the next element.
        parser.set_text_buffering(false);
        Self {
            limits,
            parser,
            buffer: Vec::new(),
            base: 0,
            parsed: 0,
            emitted: 0,
            boundary: 0,
            phase: Phase::Header,
            scopes: Vec::new(),
            head: None,
            stanza: None,
            whitespace_bytes: 0,
        }
    }

    /// The ceilings this decoder enforces.
    pub fn limits(&self) -> CodecLimits {
        self.limits
    }

    /// Whitespace received between first-level elements on this stream.
    pub fn whitespace_bytes(&self) -> u64 {
        self.whitespace_bytes
    }

    /// Bytes retained: the unfinished item plus input not yet parsed.
    pub fn retained_bytes(&self) -> usize {
        self.buffer.len() - self.dead_prefix()
    }

    /// Whether the peer has closed its stream.
    pub fn is_ended(&self) -> bool {
        self.phase == Phase::Ended
    }

    /// Append received bytes without parsing them.
    ///
    /// Fails when the retained input would exceed
    /// [`CodecLimits::max_buffered_bytes`]. Bytes received after the peer
    /// closed its stream are dropped.
    ///
    /// The whole of `bytes` is accepted or the stream fails, so callers must
    /// bound their reads: when [`Self::next_item`] is drained to `Ok(None)`
    /// before every push, at most one unfinished item is retained (no more
    /// than [`CodecLimits::max_stanza_bytes`]), and a push of at most
    /// `max_buffered_bytes - max_stanza_bytes` bytes (64 KiB with
    /// [`CodecLimits::PILOT`]) never fails for a peer within the other
    /// limits. Plaintext from one TLS record is at most 16 KiB.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), DecodeError> {
        match self.phase {
            Phase::Failed(error) => return Err(error),
            Phase::Ended => return Ok(()),
            Phase::Header | Phase::Open => {}
        }
        let retained = self.retained_bytes().saturating_add(bytes.len());
        if retained > self.limits.max_buffered_bytes {
            return Err(self.fail(DecodeError::LimitExceeded(LimitKind::BufferedBytes)));
        }
        self.compact(bytes.len());
        self.reserve(bytes.len());
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Parse buffered input up to the end of the next item.
    ///
    /// Returns `Ok(None)` when more input is needed or the stream has ended.
    /// Input after the returned item stays unparsed until the next call, so
    /// a caller that restarts the stream after this item never exposes later
    /// bytes to this stream's parser.
    pub fn next_item(&mut self) -> Result<Option<Inbound>, DecodeError> {
        match self.phase {
            Phase::Failed(error) => return Err(error),
            Phase::Ended => return Ok(None),
            Phase::Header | Phase::Open => {}
        }
        loop {
            let (budget, limit_kind) = self.budget();
            let start = self.parsed.saturating_sub(self.base).min(self.buffer.len());
            let available = self.buffer.len() - start;
            let take = available.min(budget);
            let mut input = &self.buffer[start..start + take];
            let result = self.parser.parse(&mut input, false);
            self.parsed += take - input.len();
            match result {
                Ok(Some(event)) => {
                    let before = self.emitted;
                    self.emitted += event.metrics().len();
                    match self.on_event(event, before) {
                        Ok(Some(item)) => {
                            self.boundary = self.emitted.min(self.parsed);
                            return Ok(Some(item));
                        }
                        Ok(None) => {}
                        Err(error) => return Err(self.fail(error)),
                    }
                }
                Ok(None) => {
                    // Only reachable at end of input, which this decoder
                    // never signals; treat it as an unterminated stream.
                    return Err(self.fail(DecodeError::NotWellFormed));
                }
                Err(EndOrError::NeedMoreData) => {
                    if available > take {
                        return Err(self.fail(DecodeError::LimitExceeded(limit_kind)));
                    }
                    return Ok(None);
                }
                Err(EndOrError::Error(error)) => {
                    let error = self.classify_parser_error(error);
                    return Err(self.fail(error));
                }
            }
        }
    }

    /// Restart after `<proceed/>`: hand the bytes that followed it to the TLS
    /// engine and await a new stream header.
    pub fn restart_after_tls(&mut self) -> TlsHandoff {
        TlsHandoff(self.reset())
    }

    /// Restart after SASL success: drop the bytes that followed the boundary
    /// element and await a new stream header.
    pub fn restart_after_sasl(&mut self) -> DiscardedInput {
        let bytes = self.reset().len();
        DiscardedInput { bytes }
    }

    fn reset(&mut self) -> Vec<u8> {
        let start = self.dead_prefix();
        let leftover = self.buffer.split_off(start);
        *self = Self::new(self.limits);
        leftover
    }

    fn dead_prefix(&self) -> usize {
        self.boundary
            .saturating_sub(self.base)
            .min(self.buffer.len())
    }

    /// Drop consumed input before `incoming` bytes are appended. Consumed
    /// input is kept while it is small and the buffer has room, to avoid
    /// shifting the buffer on every push; the buffer length therefore never
    /// exceeds [`CodecLimits::max_buffered_bytes`].
    fn compact(&mut self, incoming: usize) {
        let dead = self.dead_prefix();
        let full = self.buffer.len().saturating_add(incoming) > self.limits.max_buffered_bytes;
        if dead > 0 && (dead >= COMPACT_THRESHOLD || dead == self.buffer.len() || full) {
            self.buffer.drain(..dead);
            self.base += dead;
        }
    }

    /// Grow the buffer for `additional` bytes without letting amortised
    /// growth allocate beyond [`CodecLimits::max_buffered_bytes`].
    fn reserve(&mut self, additional: usize) {
        let needed = self.buffer.len().saturating_add(additional);
        if needed <= self.buffer.capacity() {
            return;
        }
        let target = self
            .buffer
            .capacity()
            .saturating_mul(2)
            .min(self.limits.max_buffered_bytes)
            .max(needed);
        self.buffer.reserve_exact(target - self.buffer.len());
    }

    fn fail(&mut self, error: DecodeError) -> DecodeError {
        self.phase = Phase::Failed(error);
        self.head = None;
        self.stanza = None;
        self.scopes.clear();
        error
    }

    /// Bytes the parser may consume before the current ceiling is reached.
    fn budget(&self) -> (usize, LimitKind) {
        match (self.phase, &self.stanza) {
            (Phase::Header, _) => (
                self.limits.max_header_bytes.saturating_sub(self.parsed),
                LimitKind::HeaderBytes,
            ),
            (_, Some(stanza)) => (
                stanza
                    .limit
                    .saturating_sub(self.parsed.saturating_sub(stanza.start)),
                stanza.limit_kind,
            ),
            (_, None) => (
                self.limits
                    .max_stanza_bytes
                    .saturating_sub(self.parsed.saturating_sub(self.emitted)),
                LimitKind::StanzaBytes,
            ),
        }
    }

    /// Map a parser error to the stream error it warrants.
    ///
    /// An XML declaration naming an encoding other than UTF-8 is reported
    /// as `restricted-xml`, not `unsupported-encoding` (RFC 6120 §4.9.3.22):
    /// the parser rejects it with the same error variant as a declaration
    /// naming an XML version other than 1.0 or a non-standalone document,
    /// and the variant's message is documented as not for interpretation.
    fn classify_parser_error(&self, error: XmlError) -> DecodeError {
        match error {
            XmlError::RestrictedXml(_) if self.unemitted_bytes() >= self.limits.max_token_bytes => {
                // The parser's only byte-count restriction: a name or
                // attribute value filled its scratch buffer. Every other
                // restriction is detected within a few bytes of the
                // previous event.
                DecodeError::LimitExceeded(LimitKind::TokenBytes)
            }
            XmlError::RestrictedXml(_) | XmlError::UndeclaredEntity => DecodeError::RestrictedXml,
            XmlError::InvalidUtf8Byte(_) => DecodeError::UnsupportedEncoding,
            _ if self.ends_in_restricted_markup() => DecodeError::RestrictedXml,
            _ => DecodeError::NotWellFormed,
        }
    }

    /// Input consumed by the parser that no event has accounted for yet.
    fn unemitted_bytes(&self) -> usize {
        self.parsed.saturating_sub(self.emitted)
    }

    /// Whether the parser stopped inside `<!` followed by a letter or `-`:
    /// a DOCTYPE, ENTITY or other markup declaration, or a comment. The
    /// parser reports these as a malformed CDATA section; RFC 6120 §11.1
    /// classes all of them as restricted XML.
    fn ends_in_restricted_markup(&self) -> bool {
        let end = self.parsed.saturating_sub(self.base).min(self.buffer.len());
        let start = end.saturating_sub(RESTRICTED_MARKUP_WINDOW);
        self.buffer[start..end].windows(3).any(|window| {
            window[0] == b'<'
                && window[1] == b'!'
                && (window[2].is_ascii_alphabetic() || window[2] == b'-')
        })
    }

    fn on_event(&mut self, event: RawEvent, start: usize) -> Result<Option<Inbound>, DecodeError> {
        match event {
            RawEvent::XmlDeclaration(..) => Ok(None),
            RawEvent::ElementHeadOpen(_, (prefix, local)) => {
                self.open_head(prefix, local, start)?;
                Ok(None)
            }
            RawEvent::Attribute(metrics, (prefix, local), value) => {
                self.add_attribute(prefix, local, value, metrics.len())?;
                Ok(None)
            }
            RawEvent::ElementHeadClose(_) => self.close_head(),
            RawEvent::ElementFoot(_) => self.close_element(),
            RawEvent::Text(metrics, text) => {
                self.add_text(text, metrics.len())?;
                Ok(None)
            }
        }
    }

    fn open_head(
        &mut self,
        prefix: Option<NcName>,
        local: NcName,
        start: usize,
    ) -> Result<(), DecodeError> {
        let depth = self.scopes.len();
        if depth > self.limits.max_depth {
            return Err(DecodeError::LimitExceeded(LimitKind::Depth));
        }
        if depth == 1 {
            self.stanza = Some(StanzaInProgress {
                start,
                limit: self.limits.max_stanza_bytes,
                limit_kind: LimitKind::StanzaBytes,
                elements: 0,
                attribute_bytes: 0,
                tree_bytes: 0,
                stack: Vec::new(),
            });
        }
        if depth >= 1 {
            let stanza = self.stanza.as_mut().ok_or(DecodeError::NotWellFormed)?;
            stanza.elements += 1;
            if stanza.elements > self.limits.max_elements_per_stanza {
                return Err(DecodeError::LimitExceeded(LimitKind::ElementsPerStanza));
            }
        }
        self.head = Some(PendingHead {
            prefix,
            local,
            default_declaration: None,
            prefix_declarations: Vec::new(),
            attributes: Vec::new(),
            attribute_count: 0,
        });
        Ok(())
    }

    fn add_attribute(
        &mut self,
        prefix: Option<NcName>,
        local: NcName,
        value: String,
        raw_bytes: usize,
    ) -> Result<(), DecodeError> {
        let head = self.head.as_mut().ok_or(DecodeError::NotWellFormed)?;
        head.attribute_count += 1;
        if head.attribute_count > self.limits.max_attributes_per_element {
            return Err(DecodeError::LimitExceeded(LimitKind::AttributesPerElement));
        }
        if let Some(stanza) = self.stanza.as_mut() {
            stanza.attribute_bytes = stanza.attribute_bytes.saturating_add(raw_bytes);
            if stanza.attribute_bytes > self.limits.max_attribute_bytes_per_stanza {
                return Err(DecodeError::LimitExceeded(LimitKind::AttributeBytes));
            }
        }
        let declaration = match &prefix {
            None => local.as_str() == XMLNS,
            Some(prefix) => prefix.as_str() == XMLNS,
        };
        if declaration && value.len() > self.limits.max_namespace_bytes {
            return Err(DecodeError::LimitExceeded(LimitKind::NamespaceBytes));
        }
        match prefix {
            None if local.as_str() == XMLNS => {
                if head.default_declaration.replace(value).is_some() {
                    return Err(DecodeError::NotWellFormed);
                }
            }
            Some(prefix) if prefix.as_str() == XMLNS => {
                if head
                    .prefix_declarations
                    .iter()
                    .any(|(declared, _)| declared == &local)
                {
                    return Err(DecodeError::NotWellFormed);
                }
                head.prefix_declarations.push((local, value));
            }
            prefix => head.attributes.push((prefix, local, value)),
        }
        Ok(())
    }

    fn close_head(&mut self) -> Result<Option<Inbound>, DecodeError> {
        let head = self.head.take().ok_or(DecodeError::NotWellFormed)?;
        let depth = self.scopes.len();
        // Declarations in scope are shared so that resolving a prefix for an
        // attribute key clones a reference instead of the namespace name.
        let scope = Scope {
            default: head
                .default_declaration
                .clone()
                .map(namespace_from_declaration),
            prefixes: head
                .prefix_declarations
                .iter()
                .map(|(prefix, uri)| (prefix.clone(), Namespace::from(uri.clone()).shared()))
                .collect(),
        };
        self.scopes.push(scope);
        let namespace = self
            .resolve_prefix(head.prefix.as_ref())
            .ok_or(DecodeError::NotWellFormed)?;
        let attributes = self.resolve_attributes(&head.attributes)?;

        if depth == 0 {
            let header = self.stream_header(&head, &namespace, &attributes)?;
            self.phase = Phase::Open;
            return Ok(Some(Inbound::Header(header)));
        }

        if depth == 1 {
            let content_namespace =
                namespace.as_str() == JABBER_SERVER || namespace.as_str() == JABBER_CLIENT;
            if content_namespace && head.prefix.is_some() {
                return Err(DecodeError::BadNamespacePrefix);
            }
            let control = [SASL, TLS, JABBER_SERVER_DIALBACK].contains(&namespace.as_str());
            if control {
                let consumed = self.emitted;
                let stanza = self.stanza.as_mut().ok_or(DecodeError::NotWellFormed)?;
                stanza.limit = stanza.limit.min(self.limits.max_control_element_bytes);
                stanza.limit_kind = LimitKind::ControlElementBytes;
                if consumed.saturating_sub(stanza.start) > stanza.limit {
                    return Err(DecodeError::LimitExceeded(LimitKind::ControlElementBytes));
                }
            }
        }

        // The element copies its local name and namespace name; attribute
        // keys share the declaration in scope but are charged as if copied,
        // which keeps the charge independent of how the tree is stored.
        let attribute_bytes = attributes
            .iter()
            .map(|((namespace, local), value)| namespace.len() + local.len() + value.len())
            .fold(0usize, usize::saturating_add);
        let element_bytes = head
            .local
            .len()
            .saturating_add(namespace.len())
            .saturating_add(attribute_bytes);
        let limit = self.limits.max_tree_bytes_per_stanza;
        let stanza = self.stanza.as_mut().ok_or(DecodeError::NotWellFormed)?;
        stanza.charge(element_bytes, limit)?;
        let mut element = Element::bare(head.local.as_str(), String::from(namespace));
        *element.attrs_mut() = attributes;
        stanza.stack.push(element);
        Ok(None)
    }

    fn close_element(&mut self) -> Result<Option<Inbound>, DecodeError> {
        self.scopes.pop().ok_or(DecodeError::NotWellFormed)?;
        if self.scopes.is_empty() {
            self.phase = Phase::Ended;
            return Ok(Some(Inbound::End));
        }
        let stanza = self.stanza.as_mut().ok_or(DecodeError::NotWellFormed)?;
        let element = stanza.stack.pop().ok_or(DecodeError::NotWellFormed)?;
        match stanza.stack.last_mut() {
            Some(parent) => {
                parent.append_child(element);
                Ok(None)
            }
            None => {
                self.stanza = None;
                Ok(Some(Inbound::Element(element)))
            }
        }
    }

    fn add_text(&mut self, text: String, raw_bytes: usize) -> Result<(), DecodeError> {
        match self.scopes.len() {
            0 => Err(DecodeError::NotWellFormed),
            1 => {
                // RFC 6120 §11.7: only whitespace may separate first-level
                // elements.
                if !text.chars().all(|c| matches!(c, ' ' | '\t' | '\r' | '\n')) {
                    return Err(DecodeError::BadFormat);
                }
                self.whitespace_bytes = self.whitespace_bytes.saturating_add(raw_bytes as u64);
                // Consumed whitespace precedes any later boundary element and
                // need not be retained.
                self.boundary = self.emitted.min(self.parsed);
                Ok(())
            }
            _ => {
                let limit = self.limits.max_tree_bytes_per_stanza;
                let stanza = self.stanza.as_mut().ok_or(DecodeError::NotWellFormed)?;
                stanza.charge(text.len(), limit)?;
                let element = stanza.stack.last_mut().ok_or(DecodeError::NotWellFormed)?;
                element.append_text(text);
                Ok(())
            }
        }
    }

    fn resolve_prefix(&self, prefix: Option<&NcName>) -> Option<Namespace> {
        match prefix {
            None => {
                let declared = self
                    .scopes
                    .iter()
                    .rev()
                    .find_map(|scope| scope.default.clone());
                Some(declared.unwrap_or_else(|| Namespace::none().clone()))
            }
            Some(prefix) if prefix.as_str() == XML_PREFIX => Some(Namespace::xml().clone()),
            Some(prefix) => self.scopes.iter().rev().find_map(|scope| {
                scope
                    .prefixes
                    .iter()
                    .find(|(declared, _)| declared == prefix)
                    .map(|(_, namespace)| namespace.clone())
            }),
        }
    }

    fn resolve_attributes(
        &self,
        attributes: &[(Option<NcName>, NcName, String)],
    ) -> Result<AttrMap, DecodeError> {
        let mut map = AttrMap::new();
        for (prefix, local, value) in attributes {
            // Unprefixed attributes are in no namespace (XML Namespaces §6.2).
            let namespace = match prefix {
                None => Namespace::none().clone(),
                Some(_) => self
                    .resolve_prefix(prefix.as_ref())
                    .ok_or(DecodeError::NotWellFormed)?,
            };
            if map
                .insert(namespace, local.clone(), value.clone())
                .is_some()
            {
                return Err(DecodeError::NotWellFormed);
            }
        }
        Ok(map)
    }

    /// Validate the stream root and extract its typed attributes.
    fn stream_header(
        &self,
        head: &PendingHead,
        namespace: &Namespace,
        attributes: &AttrMap,
    ) -> Result<StreamHeader, DecodeError> {
        if namespace.as_str() != STREAM {
            return Err(DecodeError::InvalidNamespace);
        }
        if head.local.as_str() != STREAM_ROOT {
            return Err(DecodeError::BadFormat);
        }
        if let Some(prefix) = &head.prefix {
            if prefix.as_str() != STREAM_PREFIX {
                return Err(DecodeError::BadNamespacePrefix);
            }
        }
        // Content namespace: `jabber:server` as the default namespace, or
        // the prefix-free form where the stream namespace is the default.
        if let Some(default) = &head.default_declaration {
            let prefix_free = head.prefix.is_none() && default == STREAM;
            if default != JABBER_SERVER && !prefix_free {
                return Err(DecodeError::InvalidNamespace);
            }
        }
        // RFC 6120 §4.8.5: namespaces for stanza content must not be declared
        // on the stream header. The dialback namespace is the documented
        // exception (§4.8.4).
        for (prefix, uri) in &head.prefix_declarations {
            if uri == STREAM {
                if prefix.as_str() != STREAM_PREFIX {
                    return Err(DecodeError::BadNamespacePrefix);
                }
            } else if uri != JABBER_SERVER_DIALBACK {
                return Err(DecodeError::InvalidNamespace);
            }
        }

        let none = Namespace::none();
        let text = |name: &str| attributes.get(none, name).map(String::as_str);
        let version = match text("version") {
            None => None,
            Some(value) => Some(StreamVersion::parse(value).ok_or(DecodeError::BadFormat)?),
        };
        let lang = match attributes.get(Namespace::xml(), "lang") {
            None => None,
            Some(value) => Some(StreamLanguage::parse(value).ok_or(DecodeError::BadFormat)?),
        };
        Ok(StreamHeader {
            from: HeaderAddress::parse(text("from")),
            to: HeaderAddress::parse(text("to")),
            id: text("id").and_then(StreamId::parse),
            version,
            lang,
        })
    }
}

fn namespace_from_declaration(uri: String) -> Namespace {
    if uri.is_empty() {
        Namespace::none().clone()
    } else {
        Namespace::from(uri).shared()
    }
}
