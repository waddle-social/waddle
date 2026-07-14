//! XML namespace and serialization helpers for typed XMPP payloads.

mod serialization;

pub mod ns;

pub use serialization::{element_to_string, message_to_string, stanza_to_string};
