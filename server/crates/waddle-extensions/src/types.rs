use std::fmt;

use minidom::Element;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use xmpp_parsers::jid::{BareJid, FullJid};
use xmpp_parsers::message::Message;

pub const FRAMEWORK_NAMESPACE: &str = "urn:waddle:extension:1";

/// Local name of the generic Waddle extension PubSub item envelope.
///
/// Every extension publishes its PubSub state items as
/// `<extension-item xmlns="urn:waddle:extension:1">…</extension-item>` with a
/// fixed UI-primitive vocabulary. The host renders these uniformly without
/// per-extension knowledge.
pub const EXTENSION_ITEM_LOCAL_NAME: &str = "extension-item";
pub const INVOKE_COMMAND_NODE: &str = "urn:waddle:extension:1:invoke";

const MAX_XML_DEPTH: usize = 16;
const MAX_XML_ATTRIBUTES: usize = 64;
const MAX_XML_CHILDREN: usize = 256;
const MAX_XML_TEXT_BYTES: usize = 16 * 1024;
const MAX_XML_SERIALIZED_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FrameworkTypeError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("plugin id {0:?} must use lowercase ASCII letters, digits, and hyphens")]
    InvalidPluginId(String),
    #[error("namespace {0:?} must be an absolute non-official namespace URI/URN")]
    InvalidPayloadNamespace(String),
    #[error("official XMPP namespace {0:?} cannot carry Waddle extension semantics")]
    OfficialNamespace(String),
    #[error("command node {0:?} must be under urn:waddle:extension:1")]
    InvalidCommandNode(String),
    #[error("sha256 digest {0:?} must be exactly 64 hexadecimal characters")]
    InvalidSha256Digest(String),
    #[error("artifact URI {0:?} must be immutable HTTP(S) and include /sha256/")]
    InvalidArtifactUri(String),
    #[error("bare JID {0:?} is invalid")]
    InvalidBareJid(String),
    #[error("full JID {0:?} is invalid")]
    InvalidFullJid(String),
    #[error("artifact URI {uri:?} must include digest {sha256:?}")]
    ArtifactDigestMismatch { uri: String, sha256: String },
    #[error("body range end {end} must be greater than start {start}")]
    InvalidBodyRange { start: u32, end: u32 },
    #[error("XML local name {0:?} is invalid")]
    InvalidXmlName(String),
    #[error("XML element has duplicate attribute {namespace:?}:{local_name}")]
    DuplicateXmlAttribute {
        namespace: Option<String>,
        local_name: String,
    },
    #[error("XML namespaced attributes are not supported by the framework serializer")]
    NamespacedXmlAttributeUnsupported,
    #[error("XML payload exceeds {limit}")]
    XmlLimitExceeded { limit: &'static str },
}

macro_rules! typed_non_empty_string {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, FrameworkTypeError> {
                let value = value.into();
                if value.trim().is_empty() {
                    Err(FrameworkTypeError::Empty { field: $field })
                } else {
                    Ok(Self(value))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

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
