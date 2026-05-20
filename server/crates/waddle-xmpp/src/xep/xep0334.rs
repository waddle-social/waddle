//! XEP-0334: Message Processing Hints
//!
//! Provides helpers for detecting and attaching processing hints to messages.
//! Hints instruct the server on how to handle a message:
//!
//! - **`<no-permanent-store/>`**: Do not store in persistent archives (MAM).
//! - **`<no-store/>`**: Do not store at all (no MAM, no offline storage).
//! - **`<no-copy/>`**: Do not carbon-copy to other resources (XEP-0280).
//! - **`<store/>`**: Ensure the message is archived even if it would normally be skipped.
//!
//! ## XML Format
//!
//! Hints are empty child elements within `<message/>` stanzas:
//! ```xml
//! <message type='chat' to='romeo@example.com'>
//!   <body>Secret</body>
//!   <no-store xmlns='urn:xmpp:hints'/>
//!   <no-copy xmlns='urn:xmpp:hints'/>
//! </message>
//! ```
//!
//! ## Server Behavior
//!
//! The server MUST respect these hints when processing messages:
//! - `no-copy`: Skip XEP-0280 Message Carbons
//! - `no-store`: Skip MAM archival and offline message storage
//! - `no-permanent-store`: Skip persistent MAM archival (volatile caches OK)
//! - `store`: Force archival even for body-less messages

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0334 Message Processing Hints.
pub const NS_HINTS: &str = "urn:xmpp:hints";

/// Processing hint variants defined by XEP-0334.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hint {
    /// Do not store in persistent archives (MAM).
    NoPermanentStore,
    /// Do not store at all (no MAM, no offline storage).
    NoStore,
    /// Do not carbon-copy to other resources (XEP-0280).
    NoCopy,
    /// Ensure the message is stored even if it would normally be skipped.
    Store,
}

impl Hint {
    /// Returns the XML element name for this hint.
    pub fn element_name(self) -> &'static str {
        match self {
            Self::NoPermanentStore => "no-permanent-store",
            Self::NoStore => "no-store",
            Self::NoCopy => "no-copy",
            Self::Store => "store",
        }
    }

    /// Parse a hint from an element name.
    pub fn from_element_name(name: &str) -> Option<Self> {
        match name {
            "no-permanent-store" => Some(Self::NoPermanentStore),
            "no-store" => Some(Self::NoStore),
            "no-copy" => Some(Self::NoCopy),
            "store" => Some(Self::Store),
            _ => None,
        }
    }
}

impl std::fmt::Display for Hint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.element_name())
    }
}

/// Trait for types that can carry message processing hints.
///
/// Abstracts hint extraction so MUC message wrappers or other
/// carrier types can participate in hint-aware processing.
pub trait HintCarrier {
    /// Extract all processing hints from this carrier.
    fn hints(&self) -> Vec<Hint>;

    /// Returns `true` if this carrier has a `<no-copy/>` hint.
    fn has_no_copy(&self) -> bool {
        self.hints().contains(&Hint::NoCopy)
    }

    /// Returns `true` if this carrier has a `<no-store/>` hint.
    fn has_no_store(&self) -> bool {
        self.hints().contains(&Hint::NoStore)
    }

    /// Returns `true` if this carrier has a `<no-permanent-store/>` hint.
    fn has_no_permanent_store(&self) -> bool {
        self.hints().contains(&Hint::NoPermanentStore)
    }

    /// Returns `true` if this carrier has a `<store/>` hint.
    fn has_store(&self) -> bool {
        self.hints().contains(&Hint::Store)
    }

    /// Returns `true` if archival should be skipped for this carrier.
    ///
    /// Archival is skipped when `no-store` or `no-permanent-store` is present,
    /// UNLESS `store` is also present (which overrides).
    fn should_skip_archive(&self) -> bool {
        let hints = self.hints();
        if hints.contains(&Hint::Store) {
            return false;
        }
        hints.contains(&Hint::NoStore) || hints.contains(&Hint::NoPermanentStore)
    }
}

impl HintCarrier for Message {
    fn hints(&self) -> Vec<Hint> {
        extract_hints_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a processing hint.
pub fn is_hint_element(elem: &Element) -> bool {
    elem.ns() == NS_HINTS && Hint::from_element_name(elem.name()).is_some()
}

/// Check if a message contains a specific hint.
pub fn has_hint(msg: &Message, hint: Hint) -> bool {
    msg.payloads
        .iter()
        .any(|e| e.ns() == NS_HINTS && e.name() == hint.element_name())
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract all processing hints from a message.
pub fn extract_hints_from_message(msg: &Message) -> Vec<Hint> {
    msg.payloads
        .iter()
        .filter(|e| e.ns() == NS_HINTS)
        .filter_map(|e| Hint::from_element_name(e.name()))
        .collect()
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a processing hint element.
pub fn build_hint_element(hint: Hint) -> Element {
    Element::builder(hint.element_name(), NS_HINTS).build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a processing hint to a message (no-op if already present).
pub fn add_hint(msg: &mut Message, hint: Hint) {
    if !has_hint(msg, hint) {
        msg.payloads.push(build_hint_element(hint));
    }
}

/// Remove a specific processing hint from a message.
pub fn remove_hint(msg: &mut Message, hint: Hint) {
    let name = hint.element_name();
    msg.payloads
        .retain(|e| !(e.ns() == NS_HINTS && e.name() == name));
}

/// Remove all processing hints from a message.
pub fn strip_hints(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_HINTS);
}

// ── Convenience queries ──────────────────────────────────────────────

/// Returns `true` if the message should NOT be carbon-copied (XEP-0280).
///
/// XEP-0334 limits `<no-copy/>` to messages addressed to full JIDs; for
/// bare-JID targets the hint does not override the normal RFC 6121 routing
/// rules that may fan a stanza out to multiple resources.
pub fn should_skip_carbons(msg: &Message) -> bool {
    has_hint(msg, Hint::NoCopy) && msg.to.as_ref().and_then(|jid| jid.resource()).is_some()
}

/// Returns `true` if the message should NOT be archived (MAM/offline).
pub fn should_skip_storage(msg: &Message) -> bool {
    if has_hint(msg, Hint::Store) {
        return false;
    }
    has_hint(msg, Hint::NoStore) || has_hint(msg, Hint::NoPermanentStore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Message, MessageType};

    #[test]
    fn test_hint_element_name_roundtrip() {
        let hints = [
            Hint::NoPermanentStore,
            Hint::NoStore,
            Hint::NoCopy,
            Hint::Store,
        ];
        for hint in hints {
            let name = hint.element_name();
            let parsed = Hint::from_element_name(name);
            assert_eq!(parsed, Some(hint), "roundtrip failed for {name}");
        }
    }

    #[test]
    fn test_unknown_hint_name() {
        assert_eq!(Hint::from_element_name("no-forward"), None);
    }

    #[test]
    fn test_hint_display() {
        assert_eq!(Hint::NoCopy.to_string(), "no-copy");
        assert_eq!(Hint::NoStore.to_string(), "no-store");
        assert_eq!(Hint::NoPermanentStore.to_string(), "no-permanent-store");
        assert_eq!(Hint::Store.to_string(), "store");
    }

    #[test]
    fn test_is_hint_element() {
        let no_copy = Element::builder("no-copy", NS_HINTS).build();
        assert!(is_hint_element(&no_copy));

        let store = Element::builder("store", NS_HINTS).build();
        assert!(is_hint_element(&store));

        let wrong_ns = Element::builder("no-copy", "jabber:client").build();
        assert!(!is_hint_element(&wrong_ns));

        let wrong_name = Element::builder("no-forward", NS_HINTS).build();
        assert!(!is_hint_element(&wrong_name));
    }

    #[test]
    fn test_has_hint() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Secret</body>\
                    <no-store xmlns='urn:xmpp:hints'/>\
                    <no-copy xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(has_hint(&msg, Hint::NoStore));
        assert!(has_hint(&msg, Hint::NoCopy));
        assert!(!has_hint(&msg, Hint::Store));
        assert!(!has_hint(&msg, Hint::NoPermanentStore));
    }

    #[test]
    fn test_extract_hints() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <no-permanent-store xmlns='urn:xmpp:hints'/>\
                    <no-copy xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let hints = extract_hints_from_message(&msg);
        assert_eq!(hints.len(), 2);
        assert!(hints.contains(&Hint::NoPermanentStore));
        assert!(hints.contains(&Hint::NoCopy));
    }

    #[test]
    fn test_extract_hints_empty() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_hints_from_message(&msg).is_empty());
    }

    #[test]
    fn test_build_hint_element() {
        for hint in [
            Hint::NoPermanentStore,
            Hint::NoStore,
            Hint::NoCopy,
            Hint::Store,
        ] {
            let elem = build_hint_element(hint);
            assert_eq!(elem.name(), hint.element_name());
            assert_eq!(elem.ns(), NS_HINTS);
        }
    }

    #[test]
    fn test_add_hint() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_hint(&mut msg, Hint::NoCopy);
        assert!(has_hint(&msg, Hint::NoCopy));

        // Adding again is a no-op
        add_hint(&mut msg, Hint::NoCopy);
        let count = msg
            .payloads
            .iter()
            .filter(|e| e.ns() == NS_HINTS && e.name() == "no-copy")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_remove_hint() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <no-store xmlns='urn:xmpp:hints'/>\
                    <no-copy xmlns='urn:xmpp:hints'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        remove_hint(&mut msg, Hint::NoStore);
        assert!(!has_hint(&msg, Hint::NoStore));
        assert!(has_hint(&msg, Hint::NoCopy));
    }

    #[test]
    fn test_strip_hints() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hello</body>\
                    <no-store xmlns='urn:xmpp:hints'/>\
                    <no-copy xmlns='urn:xmpp:hints'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_hints(&mut msg);
        assert!(extract_hints_from_message(&msg).is_empty());
        // Body should still be there
        assert!(!msg.bodies.is_empty());
    }

    #[test]
    fn test_should_skip_carbons_for_full_jid_target() {
        let xml = "<message xmlns='jabber:client' type='chat' to='juliet@example.com/phone'>\
                    <body>Secret</body>\
                    <no-copy xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(should_skip_carbons(&msg));

        let plain = Message::new(None::<jid::Jid>);
        assert!(!should_skip_carbons(&plain));
    }

    #[test]
    fn test_should_not_skip_carbons_for_bare_jid_target() {
        let xml = "<message xmlns='jabber:client' type='chat' to='juliet@example.com'>\
                    <body>Secret</body>\
                    <no-copy xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(!should_skip_carbons(&msg));
    }

    #[test]
    fn test_should_skip_storage_no_store() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Ephemeral</body>\
                    <no-store xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(should_skip_storage(&msg));
    }

    #[test]
    fn test_should_skip_storage_no_permanent_store() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Temp</body>\
                    <no-permanent-store xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(should_skip_storage(&msg));
    }

    #[test]
    fn test_store_hint_overrides_no_store() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <no-store xmlns='urn:xmpp:hints'/>\
                    <store xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        // store overrides no-store
        assert!(!should_skip_storage(&msg));
    }

    #[test]
    fn test_hint_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Secret</body>\
                    <no-store xmlns='urn:xmpp:hints'/>\
                    <no-copy xmlns='urn:xmpp:hints'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_no_store());
        assert!(msg.has_no_copy());
        assert!(!msg.has_store());
        assert!(!msg.has_no_permanent_store());
        assert!(msg.should_skip_archive());
    }

    #[test]
    fn test_hint_carrier_store_overrides() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.bodies
            .insert(xmpp_parsers::message::Lang::new(), "test".to_string());
        add_hint(&mut msg, Hint::NoStore);
        add_hint(&mut msg, Hint::Store);

        assert!(msg.has_no_store());
        assert!(msg.has_store());
        // store overrides
        assert!(!msg.should_skip_archive());
    }

    #[test]
    fn test_plain_message_no_hints() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "Normal message".to_string(),
        );

        assert!(!msg.has_no_copy());
        assert!(!msg.has_no_store());
        assert!(!msg.should_skip_archive());
        assert!(msg.hints().is_empty());
    }
}
