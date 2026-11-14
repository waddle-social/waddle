//! Serialisation of one outbound `jabber:server` stream.

use std::fmt;

use minidom::rxml::writer::{SimpleNamespaces, TrackNamespace};
use minidom::rxml::{xml_ncname, Encoder, Item, Namespace, XmlVersion};
use minidom::Element;
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
        let tracker = encoder.ns_tracker_mut();
        tracker.declare_fixed(None, Namespace::from(JABBER_SERVER));
        tracker.declare_fixed(Some(xml_ncname!("stream")), stream.clone());
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
    pub fn element(&mut self, element: &Element) -> Result<Vec<u8>, EncodeError> {
        if self.closed {
            return Err(EncodeError::Closed);
        }
        let mut out = Vec::new();
        element.write_to(&mut out).map_err(|_| EncodeError::Xml)?;
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

fn address(address: &HeaderAddress) -> Option<&str> {
    address.domain().map(|domain| domain.as_str())
}
