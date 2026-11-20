//! Serialisation of one outbound `jabber:server` stream.

use std::fmt;

use minidom::rxml::writer::{SimpleNamespaces, TrackNamespace};
use minidom::rxml::{xml_ncname, Encoder, Item, Namespace, NcNameStr, XmlVersion};
use minidom::{Element, Node};
use thiserror::Error;

use crate::ns::{JABBER_SERVER, STREAM};

use super::condition::StreamErrorCondition;
use super::header::{HeaderAddress, StreamHeader};

/// Why an outbound item could not be serialised.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EncodeError {
    #[error("item cannot be serialised as XML")]
    Xml,
    #[error("outbound stream is already closed")]
    Closed,
}

/// Serialises the stream header, first-level elements and the closing tag
/// of one outbound stream. A stream restart needs a new encoder.
pub struct StreamEncoder {
    encoder: Encoder<SimpleNamespaces>,
    closed: bool,
}

impl fmt::Debug for StreamEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamEncoder")
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl StreamEncoder {
    /// Serialise an XML declaration and the opening stream tag.
    ///
    /// The content namespace `jabber:server` is the default namespace and the
    /// stream namespace uses the `stream` prefix, the form every deployed
    /// server accepts (RFC 6120 §4.8.5).
    pub fn open(header: &StreamHeader) -> Result<(Self, Vec<u8>), EncodeError> {
        let mut encoder = Encoder::new();
        let mut out = Vec::new();
        let stream = Namespace::from(STREAM);
        encoder
            .encode(Item::XmlDeclaration(XmlVersion::V1_0), &mut out)
            .map_err(|_| EncodeError::Xml)?;
        declare_stream_namespaces(encoder.ns_tracker_mut());
        encoder
            .encode(
                Item::ElementHeadStart(&stream, xml_ncname!("stream")),
                &mut out,
            )
            .map_err(|_| EncodeError::Xml)?;

        let none = Namespace::none();
        let version = header.version.map(|version| version.to_string());
        let attributes = [
            (none, xml_ncname!("from"), address(&header.from)),
            (none, xml_ncname!("to"), address(&header.to)),
            (
                none,
                xml_ncname!("id"),
                header.id.as_ref().map(|id| id.as_str()),
            ),
            (none, xml_ncname!("version"), version.as_deref()),
            (
                Namespace::xml(),
                xml_ncname!("lang"),
                header.lang.as_ref().map(|lang| lang.as_str()),
            ),
        ];
        for (namespace, name, value) in attributes {
            if let Some(value) = value {
                encoder
                    .encode(Item::Attribute(namespace, name, value), &mut out)
                    .map_err(|_| EncodeError::Xml)?;
            }
        }
        encoder
            .encode(Item::ElementHeadEnd, &mut out)
            .map_err(|_| EncodeError::Xml)?;
        Ok((
            Self {
                encoder,
                closed: false,
            },
            out,
        ))
    }

    /// Serialise one first-level element.
    ///
    /// The element is written in the scope of the open stream header:
    /// elements in the stream namespace use the `stream` prefix bound there,
    /// which a peer may require (RFC 6120 §4.8.5), and elements in
    /// `jabber:server` inherit the default namespace without redeclaring it.
    /// Namespaces are written from each element's and attribute's namespace
    /// name; prefixes recorded on a [`Element`] are not reproduced.
    pub fn element(&mut self, element: &Element) -> Result<Vec<u8>, EncodeError> {
        if self.closed {
            return Err(EncodeError::Closed);
        }
        // A separate encoder per element, entered into the header's scope
        // without output, keeps a serialisation failure from leaving the
        // stream encoder inside a half-written element.
        let mut namespaces = SimpleNamespaces::new();
        declare_stream_namespaces(&mut namespaces);
        namespaces.push();
        let mut encoder = Encoder::from(namespaces);
        let mut out = Vec::new();
        write_element(&mut encoder, element, &mut out)?;
        Ok(out)
    }

    /// Serialise the closing stream tag.
    pub fn close(&mut self) -> Result<Vec<u8>, EncodeError> {
        if self.closed {
            return Err(EncodeError::Closed);
        }
        let mut out = Vec::new();
        self.encoder
            .encode(Item::ElementFoot, &mut out)
            .map_err(|_| EncodeError::Xml)?;
        self.closed = true;
        Ok(out)
    }

    /// Serialise a stream error followed by the closing stream tag.
    pub fn stream_error(
        &mut self,
        condition: StreamErrorCondition,
    ) -> Result<Vec<u8>, EncodeError> {
        let mut out = self.element(&condition.to_element())?;
        out.extend(self.close()?);
        Ok(out)
    }

    /// Whether the closing stream tag has been serialised.
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Bind the namespaces declared on every outbound stream header.
fn declare_stream_namespaces(namespaces: &mut SimpleNamespaces) {
    namespaces.declare_fixed(None, Namespace::from(JABBER_SERVER));
    namespaces.declare_fixed(Some(xml_ncname!("stream")), Namespace::from(STREAM));
}

/// Write `root` and its descendants without recursion.
fn write_element(
    encoder: &mut Encoder<SimpleNamespaces>,
    root: &Element,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    write_head(encoder, root, out)?;
    let mut open = vec![root.nodes()];
    while let Some(children) = open.last_mut() {
        match children.next() {
            Some(Node::Element(child)) => {
                write_head(encoder, child, out)?;
                open.push(child.nodes());
            }
            Some(Node::Text(text)) => encode(encoder, Item::Text(text), out)?,
            None => {
                open.pop();
                encode(encoder, Item::ElementFoot, out)?;
            }
        }
    }
    Ok(())
}

/// Write an element's start tag; an element without children is closed by
/// the following [`Item::ElementFoot`] as an empty-element tag.
fn write_head(
    encoder: &mut Encoder<SimpleNamespaces>,
    element: &Element,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    let namespace = Namespace::from(element.ns());
    let name = NcNameStr::from_str(element.name()).map_err(|_| EncodeError::Xml)?;
    encode(encoder, Item::ElementHeadStart(&namespace, name), out)?;
    for ((namespace, name), value) in element.attrs() {
        encode(encoder, Item::Attribute(namespace, name, value), out)?;
    }
    if element.nodes().next().is_some() {
        encode(encoder, Item::ElementHeadEnd, out)?;
    }
    Ok(())
}

fn encode(
    encoder: &mut Encoder<SimpleNamespaces>,
    item: Item<'_>,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    encoder.encode(item, out).map_err(|_| EncodeError::Xml)
}

fn address(address: &HeaderAddress) -> Option<&str> {
    address.domain().map(|domain| domain.as_str())
}
