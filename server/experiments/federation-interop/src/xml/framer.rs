//! An incremental reader of one XML stream (RFC 6120 section 4): the
//! stream header, then each first-level element as a `minidom::Element`,
//! then the stream's close. It is the harness's own reader, independent of
//! Waddle's codec, built on `minidom`'s tree builder over `rxml`'s raw
//! parser. A stream restart after STARTTLS or SASL needs a new framer.

use minidom::rxml::error::EndOrError;
use minidom::rxml::{Parse, RawParser};
use minidom::tree_builder::TreeBuilder;
use minidom::Element;

/// The opening tag of a stream, with its attributes as received.
#[derive(Debug, Clone)]
pub struct Header {
    pub name: String,
    pub namespace: String,
    pub attributes: Vec<(String, String)>,
}

impl Header {
    /// An unqualified attribute's value.
    pub fn attribute(&self, name: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Header(Header),
    Element(Element),
    /// The peer closed the stream with `</stream:stream>`.
    Close,
}

/// A parse failure: the bytes are not a well-formed XML stream.
#[derive(Debug, Clone)]
pub struct FramerError(pub String);

impl std::fmt::Display for FramerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FramerError {}

pub struct Framer {
    parser: RawParser,
    tree: TreeBuilder,
    buffer: Vec<u8>,
    header_sent: bool,
    closed: bool,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Self {
            parser: RawParser::new(),
            tree: TreeBuilder::new(),
            buffer: Vec::new(),
            header_sent: false,
            closed: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete item, or `None` until more bytes arrive.
    pub fn next_item(&mut self) -> Result<Option<Item>, FramerError> {
        loop {
            if let Some(item) = self.ready() {
                return Ok(Some(item));
            }
            if self.closed {
                return Ok(None);
            }
            let mut input: &[u8] = &self.buffer;
            let result = self.parser.parse(&mut input, false);
            let consumed = self.buffer.len() - input.len();
            self.buffer.drain(..consumed);
            match result {
                Ok(Some(event)) => self
                    .tree
                    .process_event(event)
                    .map_err(|error| FramerError(error.to_string()))?,
                Ok(None) => {
                    self.closed = true;
                }
                Err(EndOrError::NeedMoreData) => return Ok(None),
                Err(EndOrError::Error(error)) => return Err(FramerError(error.to_string())),
            }
        }
    }

    fn ready(&mut self) -> Option<Item> {
        if !self.header_sent && self.tree.depth() >= 1 {
            let root = self.tree.top()?;
            self.header_sent = true;
            let attributes = root
                .attrs()
                .iter()
                .filter(|((namespace, _), _)| namespace.is_none())
                .map(|((_, name), value)| (name.to_string(), value.clone()))
                .collect();
            return Some(Item::Header(Header {
                name: root.name().to_owned(),
                namespace: root.ns(),
                attributes,
            }));
        }
        if self.tree.depth() == 1 {
            if let Some(element) = self.tree.unshift_child() {
                return Some(Item::Element(element));
            }
        }
        if self.header_sent && self.tree.depth() == 0 {
            if let Some(mut root) = self.tree.root.take() {
                if let Some(element) = root.unshift_child() {
                    self.tree.root = Some(root);
                    return Some(Item::Element(element));
                }
                self.closed = true;
                return Some(Item::Close);
            }
        }
        None
    }
}
