use std::str::FromStr;

use minidom::Element;

use super::SM_NS;

/// Namespace for XMPP stanza error conditions, used as the `<failed/>` child.
const STANZA_ERROR_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

/// Parse an `xs:boolean` attribute value per W3C XML Schema Part 2 §3.2.2.
///
/// Both lexical forms are accepted for true (`"true"`, `"1"`) and false
/// (`"false"`, `"0"`). The XMPP ecosystem uses both: `prosody` and `ejabberd`
/// emit `"true"`, while `stanza.js` (our WebSocket client) emits the
/// canonical `"1"` / `"0"`. Missing or unrecognised values are treated as
/// `false`, matching the XMPP convention for optional boolean attributes.
fn parse_xs_boolean(value: &str) -> bool {
    matches!(value, "true" | "1")
}

/// Serialize a minidom `Element` to an XML string.
///
/// Writing to `Vec<u8>` is infallible (minidom only fails on I/O errors,
/// which `Vec<u8>` cannot produce) and minidom is documented to emit valid
/// UTF-8, so the two `expect`s are unreachable in practice.
fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element
        .write_to(&mut buf)
        .expect("minidom Element::write_to(Vec<u8>) cannot fail");
    String::from_utf8(buf).expect("minidom emits valid UTF-8")
}

/// Enable request from client to activate stream management.
///
/// The client sends this after resource binding to enable SM features.
/// Optional attributes:
/// - `resume`: Request ability to resume the stream after disconnection
/// - `max`: Maximum resumption time in seconds the client can support
#[derive(Debug, Clone, Default)]
pub struct SmEnable {
    /// Whether the client wants to be able to resume the stream
    pub resume: bool,
    /// Maximum resumption time in seconds (optional)
    pub max: Option<u32>,
}

impl SmEnable {
    /// Create a new enable request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an enable request with resumption support.
    pub fn with_resume(max_seconds: Option<u32>) -> Self {
        Self {
            resume: true,
            max: max_seconds,
        }
    }

    /// Parse an `<enable/>` element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.name() != "enable" || element.ns() != SM_NS {
            return None;
        }
        Some(Self::from_element(&element))
    }

    fn from_element(element: &Element) -> Self {
        let resume = element
            .attr("resume")
            .map(parse_xs_boolean)
            .unwrap_or(false);
        let max = element.attr("max").and_then(|s| s.parse().ok());
        Self { resume, max }
    }
}

/// Enabled response from server confirming stream management is active.
///
/// Sent by the server in response to `<enable/>`.
#[derive(Debug, Clone)]
pub struct SmEnabled {
    /// Unique identifier for this stream (for resumption)
    pub id: String,
    /// Whether stream resumption is available
    pub resume: bool,
    /// Maximum time in seconds the server will allow resumption
    pub max: Option<u32>,
    /// Server location hint for resumption (optional)
    pub location: Option<String>,
}

impl SmEnabled {
    /// Create a new enabled response.
    pub fn new(id: String) -> Self {
        Self {
            id,
            resume: false,
            max: None,
            location: None,
        }
    }

    /// Create an enabled response with resumption support.
    pub fn with_resume(id: String, max_seconds: u32) -> Self {
        Self {
            id,
            resume: true,
            max: Some(max_seconds),
            location: None,
        }
    }

    /// Serialize to an `<enabled/>` XML string.
    pub fn to_xml(&self) -> String {
        let mut builder = Element::builder("enabled", SM_NS).attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            self.id.as_str(),
        );
        if self.resume {
            builder = builder.attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true");
        }
        if let Some(max) = self.max {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("max").to_owned(),
                max.to_string(),
            );
        }
        if let Some(location) = self.location.as_deref() {
            builder = builder.attr(minidom::rxml::xml_ncname!("location").to_owned(), location);
        }
        element_to_xml(builder.build())
    }
}

/// Resume request from client to restore a previous stream.
///
/// The client sends this instead of resource binding when reconnecting.
#[derive(Debug, Clone)]
pub struct SmResume {
    /// The stream ID from the original `<enabled/>` response
    pub previd: String,
    /// The last handled stanza count from the client's perspective
    pub h: u32,
}

impl SmResume {
    /// Parse a `<resume/>` element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.name() != "resume" || element.ns() != SM_NS {
            return None;
        }
        Self::from_element(&element)
    }

    /// Parse a `<resume/>` element already extracted from its parent.
    pub fn from_element(element: &Element) -> Option<Self> {
        let previd = element.attr("previd")?.to_string();
        let h = element.attr("h").and_then(|s| s.parse().ok())?;
        Some(Self { previd, h })
    }
}

/// Resumed response from server confirming stream was restored.
#[derive(Debug, Clone)]
pub struct SmResumed {
    /// The stream ID that was resumed
    pub previd: String,
    /// The server's last handled stanza count
    pub h: u32,
}

impl SmResumed {
    /// Create a new resumed response.
    pub fn new(previd: String, h: u32) -> Self {
        Self { previd, h }
    }

    /// Build the `<resumed/>` element.
    pub fn to_element(&self) -> Element {
        Element::builder("resumed", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                self.previd.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("h").to_owned(),
                self.h.to_string(),
            )
            .build()
    }

    /// Serialize to a `<resumed/>` XML string.
    pub fn to_xml(&self) -> String {
        element_to_xml(self.to_element())
    }
}

/// Failed response indicating a stream management operation failed.
#[derive(Debug, Clone)]
pub struct SmFailed {
    /// Error condition (e.g., "item-not-found" for unknown stream ID)
    pub condition: Option<String>,
    /// The handled count at time of failure (for resume failures)
    pub h: Option<u32>,
}

impl SmFailed {
    /// Create a simple failed response.
    pub fn new() -> Self {
        Self {
            condition: None,
            h: None,
        }
    }

    /// Create a failed response with an error condition.
    pub fn with_condition(condition: &str) -> Self {
        Self {
            condition: Some(condition.to_string()),
            h: None,
        }
    }

    /// Create a failed response for resume failure with handled count.
    pub fn resume_failed(condition: &str, h: u32) -> Self {
        Self {
            condition: Some(condition.to_string()),
            h: Some(h),
        }
    }

    /// Build the `<failed/>` element as a typed stream-management response.
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("failed", SM_NS);
        if let Some(h) = self.h {
            builder = builder.attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string());
        }
        if let Some(condition) = self.condition.as_deref() {
            builder = builder.append(Element::builder(condition, STANZA_ERROR_NS).build());
        }
        builder.build()
    }

    /// Serialize to a `<failed/>` XML string.
    pub fn to_xml(&self) -> String {
        element_to_xml(self.to_element())
    }
}

impl Default for SmFailed {
    fn default() -> Self {
        Self::new()
    }
}

/// Acknowledgment request from either party.
///
/// When received, the other party should respond with `<a/>`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SmRequest;

impl SmRequest {
    /// Check if XML is an ack request.
    pub fn is_request(xml: &str) -> bool {
        Element::from_str(xml)
            .map(|el| el.name() == "r" && el.ns() == SM_NS)
            .unwrap_or(false)
    }

    /// Serialize to an `<r/>` XML string.
    pub fn to_xml() -> String {
        element_to_xml(Element::builder("r", SM_NS).build())
    }
}

/// Acknowledgment response containing the count of handled stanzas.
///
/// The `h` attribute indicates the sequence number of the last handled stanza.
#[derive(Debug, Clone, Copy)]
pub struct SmAck {
    /// The sequence number of the last handled inbound stanza
    pub h: u32,
}

impl SmAck {
    /// Create a new acknowledgment with the given count.
    pub fn new(h: u32) -> Self {
        Self { h }
    }

    /// Parse an `<a/>` ack element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.name() != "a" || element.ns() != SM_NS {
            return None;
        }
        Self::from_element(&element)
    }

    fn from_element(element: &Element) -> Option<Self> {
        let h = element.attr("h").and_then(|s| s.parse().ok())?;
        Some(Self { h })
    }

    /// Serialize to an `<a/>` XML string.
    pub fn to_xml(&self) -> String {
        element_to_xml(
            Element::builder("a", SM_NS)
                .attr(
                    minidom::rxml::xml_ncname!("h").to_owned(),
                    self.h.to_string(),
                )
                .build(),
        )
    }
}

/// Parsed stream management stanza variants.
#[derive(Debug, Clone)]
pub enum SmStanza {
    /// Enable stream management request
    Enable(SmEnable),
    /// Stream management enabled response
    Enabled(SmEnabled),
    /// Resume stream request
    Resume(SmResume),
    /// Stream resumed response
    Resumed(SmResumed),
    /// Stream management failed
    Failed(SmFailed),
    /// Request acknowledgment
    Request,
    /// Acknowledgment with handled count
    Ack(SmAck),
}

impl SmStanza {
    /// Cheap lexical prefilter used by hot paths (e.g. WebSocket frame
    /// routing) so we only pay full XML parsing for likely SM control nonzas.
    pub fn is_client_nonza_candidate(xml: &str) -> bool {
        let trimmed = xml.trim_start();
        (trimmed.starts_with("<enable")
            || trimmed.starts_with("<resume")
            || trimmed.starts_with("<r")
            || trimmed.starts_with("<a"))
            && trimmed.contains(SM_NS)
    }

    /// Try to parse a stream management nonza from XML.
    ///
    /// Only parses client-origin nonzas: `<enable/>`, `<resume/>`, `<r/>`,
    /// `<a/>`. Server-origin nonzas (`<enabled/>`, `<resumed/>`, `<failed/>`)
    /// return `None` — the server authors those itself.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.ns() != SM_NS {
            return None;
        }
        match element.name() {
            "enable" => Some(SmStanza::Enable(SmEnable::from_element(&element))),
            "resume" => SmResume::from_element(&element).map(SmStanza::Resume),
            "a" => SmAck::from_element(&element).map(SmStanza::Ack),
            "r" => Some(SmStanza::Request),
            _ => None,
        }
    }
}
