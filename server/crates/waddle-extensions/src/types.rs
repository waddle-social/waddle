//! Validated extension identifiers, XML payloads, manifests, effects, and UI models.

use std::fmt;

use minidom::Element;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use xmpp_parsers::jid::{BareJid, FullJid};
use xmpp_parsers::message::Message;

/// Namespace for Waddle extension envelopes, launch descriptors, and host-rendered UI.
pub const FRAMEWORK_NAMESPACE: &str = "urn:waddle:extension:1";

/// Local name of the generic Waddle extension PubSub item envelope.
///
/// Every extension publishes its PubSub state items as
/// `<extension-item xmlns="urn:waddle:extension:1">…</extension-item>` with a
/// fixed UI-primitive vocabulary. The host renders these uniformly without
/// per-extension knowledge.
pub const EXTENSION_ITEM_LOCAL_NAME: &str = "extension-item";
/// Generic ad-hoc command node through which signed extension launches are invoked.
pub const INVOKE_COMMAND_NODE: &str = "urn:waddle:extension:1:invoke";

const MAX_XML_DEPTH: usize = 16;
const MAX_XML_ATTRIBUTES: usize = 64;
const MAX_XML_CHILDREN: usize = 256;
const MAX_XML_TEXT_BYTES: usize = 16 * 1024;
const MAX_XML_SERIALIZED_BYTES: usize = 64 * 1024;

/// Validation failures for extension identifiers, XML trees, and artifact references.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameworkTypeError {
    /// A durable observation generation was zero.
    #[error("observation generation must be positive")]
    InvalidObservationGeneration,
    /// A required text field was empty or whitespace-only.
    #[error("{field} must not be empty")]
    Empty {
        #[doc = "Name of the required text field that was empty."]
        field: &'static str,
    },
    /// Plugin identifier violated the lowercase ASCII grammar or required alphanumeric ends.
    #[error("plugin id {0:?} must use lowercase ASCII letters, digits, and hyphens")]
    InvalidPluginId(String),
    /// Payload namespace lacked the accepted `urn:` or `https://` prefix.
    #[error("namespace {0:?} must be an absolute non-official namespace URI/URN")]
    InvalidPayloadNamespace(String),
    /// Payload namespace used a reserved official XMPP prefix.
    #[error("official XMPP namespace {0:?} cannot carry Waddle extension semantics")]
    OfficialNamespace(String),
    /// Command node was outside the extension framework namespace.
    #[error("command node {0:?} must be under urn:waddle:extension:1")]
    InvalidCommandNode(String),
    /// SHA-256 digest was not exactly 64 ASCII hexadecimal characters.
    #[error("sha256 digest {0:?} must be exactly 64 hexadecimal characters")]
    InvalidSha256Digest(String),
    /// Artifact locator lacked an HTTP(S) prefix or the `/sha256/` path marker.
    #[error("artifact URI {0:?} must be immutable HTTP(S) and include /sha256/")]
    InvalidArtifactUri(String),
    /// Room address failed bare-JID parsing.
    #[error("bare JID {0:?} is invalid")]
    InvalidBareJid(String),
    /// Address failed full-JID parsing, including its required resource.
    #[error("full JID {0:?} is invalid")]
    InvalidFullJid(String),
    /// Artifact locator did not contain the expected digest, compared case-insensitively.
    #[error("artifact URI {uri:?} must include digest {sha256:?}")]
    ArtifactDigestMismatch {
        #[doc = "URI identifying the referenced resource."]
        uri: String,
        #[doc = "Hexadecimal SHA-256 digest used to verify artifact content."]
        sha256: String,
    },
    /// A body range had an end less than or equal to its start.
    #[error("body range end {end} must be greater than start {start}")]
    InvalidBodyRange {
        #[doc = "Inclusive offset supplied for the body range."]
        start: u32,
        #[doc = "Exclusive offset supplied for the body range."]
        end: u32,
    },
    /// Local name failed the supported unprefixed ASCII XML-name grammar.
    #[error("XML local name {0:?} is invalid")]
    InvalidXmlName(String),
    /// An element repeated the same attribute namespace and local-name pair.
    #[error("XML element has duplicate attribute {namespace:?}:{local_name}")]
    DuplicateXmlAttribute {
        /// XML namespace URI of the element or attribute.
        namespace: Option<String>,
        /// Local XML name without a namespace prefix.
        local_name: String,
    },
    /// The element model contained a namespaced attribute unsupported by serialization.
    #[error("XML namespaced attributes are not supported by the framework serializer")]
    NamespacedXmlAttributeUnsupported,
    /// XML tree exceeded its depth, per-node, text, or aggregate content-size budget.
    #[error("XML payload exceeds {limit}")]
    XmlLimitExceeded {
        #[doc = "Name of the XML budget that was exceeded."]
        limit: &'static str,
    },
}

macro_rules! typed_non_empty_string {
    ($name:ident, $field:literal, $description:literal) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[doc = $description]
        #[doc = ""]
        #[doc = "The constructor rejects empty or whitespace-only input and otherwise retains"]
        #[doc = "the exact text, including surrounding whitespace. Serde deserialization does"]
        #[doc = "not call the constructor; validate untrusted deserialized values at their boundary."]
        pub struct $name(String);

        impl $name {
            /// Accept nonblank text while preserving the exact input, including
            /// surrounding whitespace.
            ///
            /// # Errors
            ///
            /// Returns `FrameworkTypeError::Empty` if the value is empty or
            /// contains only whitespace; accepted text is retained without
            /// trimming.
            pub fn try_new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
                let value = value.into();
                if value.trim().is_empty() {
                    Err(FrameworkTypeError::Empty { field: $field })
                } else {
                    Ok(Self(value))
                }
            }

            /// Borrow the stored text without allocating or changing whitespace.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume this wrapper and return its original owned text without copying.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

mod effects;
mod events;
mod manifest;
mod message;
mod payload;
mod primitives;
mod ui;

pub use effects::*;
pub use events::*;
pub use manifest::*;
pub use message::*;
pub use payload::*;
pub use primitives::*;
pub use ui::*;
/// Whether a namespace starts with a reserved `urn:xmpp:`, `jabber:`, or `http://jabber.org/`
/// prefix.
#[must_use]
pub fn is_official_namespace(value: &str) -> bool {
    value.starts_with("urn:xmpp:")
        || value.starts_with("jabber:")
        || value.starts_with("http://jabber.org/")
}

fn validate_xml_local_name(value: String) -> Result<String, FrameworkTypeError> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(FrameworkTypeError::InvalidXmlName(value));
    };
    let valid_start = first == '_' || first.is_ascii_alphabetic();
    let valid_rest =
        chars.all(|ch| ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_alphanumeric());
    if valid_start && valid_rest && !value.contains(':') && !value.starts_with("xml") {
        Ok(value)
    } else {
        Err(FrameworkTypeError::InvalidXmlName(value))
    }
}

#[cfg(test)]
mod tests;

mod observation;
pub use observation::*;
