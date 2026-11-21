//! Writes stream headers (RFC 6120 section 4.7) through `rxml`'s encoder,
//! which escapes attribute values and declares the namespaces; no header
//! is assembled from strings.

use minidom::rxml::parser::XmlVersion;
use minidom::rxml::writer::{Encoder, Item, TrackNamespace};
use minidom::rxml::{xml_ncname, Namespace, NcNameStr};
use minidom::Element;

use crate::xml::ns;

/// The content namespace of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Client,
    Server,
}

/// What a stream header says. Addresses are text as written, so the
/// negative cases can send forms a JID parser would refuse or fold.
#[derive(Debug, Clone)]
pub struct StreamOpen {
    pub content: Content,
    pub to: Option<String>,
    pub from: Option<String>,
    pub id: Option<String>,
    pub version: bool,
    pub declare_dialback: bool,
    pub xml_declaration: bool,
}

impl StreamOpen {
    pub fn client(to: &str) -> Self {
        Self {
            content: Content::Client,
            to: Some(to.to_owned()),
            from: None,
            id: None,
            version: true,
            declare_dialback: false,
            xml_declaration: true,
        }
    }

    pub fn server(from: Option<&str>, to: &str) -> Self {
        Self {
            content: Content::Server,
            to: Some(to.to_owned()),
            from: from.map(str::to_owned),
            id: None,
            version: true,
            declare_dialback: false,
            xml_declaration: true,
        }
    }

    pub fn with_dialback(mut self) -> Self {
        self.declare_dialback = true;
        self
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self.id = Some(id.to_owned());
        self
    }

    /// The header's bytes.
    pub fn encode(&self) -> Result<Vec<u8>, minidom::rxml::Error> {
        let mut encoder = Encoder::new();
        let stream = Namespace::from(ns::STREAM);
        let content = Namespace::from(match self.content {
            Content::Client => ns::CLIENT,
            Content::Server => ns::SERVER,
        });
        encoder
            .ns_tracker_mut()
            .declare_fixed(Some(xml_ncname!("stream")), stream.clone());
        encoder.ns_tracker_mut().declare_fixed(None, content);
        if self.declare_dialback {
            encoder
                .ns_tracker_mut()
                .declare_fixed(Some(xml_ncname!("db")), Namespace::from(ns::DIALBACK));
        }
        let mut out = minidom::rxml::bytes::BytesMut::new();
        if self.xml_declaration {
            encoder.encode(Item::XmlDeclaration(XmlVersion::V1_0), &mut out)?;
        }
        encoder.encode(
            Item::ElementHeadStart(&stream, xml_ncname!("stream")),
            &mut out,
        )?;
        let attributes: [(&NcNameStr, Option<&str>); 4] = [
            (xml_ncname!("to"), self.to.as_deref()),
            (xml_ncname!("from"), self.from.as_deref()),
            (xml_ncname!("id"), self.id.as_deref()),
            (xml_ncname!("version"), self.version.then_some("1.0")),
        ];
        for (name, value) in attributes {
            if let Some(value) = value {
                encoder.encode(Item::Attribute(Namespace::none(), name, value), &mut out)?;
            }
        }
        encoder.encode(
            Item::Attribute(&Namespace::XML, xml_ncname!("lang"), "en"),
            &mut out,
        )?;
        encoder.encode(Item::ElementHeadEnd, &mut out)?;
        Ok(out.to_vec())
    }
}

/// An element's bytes as written on a stream.
pub fn element_bytes(element: &Element) -> Vec<u8> {
    let mut bytes = Vec::new();
    // Writing into a Vec only fails for an element minidom could not
    // have built; such an element is sent as nothing.
    if element.write_to(&mut bytes).is_err() {
        bytes.clear();
    }
    bytes
}

/// The closing tag of a stream (RFC 6120 section 4.4).
pub const STREAM_CLOSE: &[u8] = b"</stream:stream>";
