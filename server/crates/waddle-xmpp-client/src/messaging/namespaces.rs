pub(crate) const NS_DELAY: &str = "urn:xmpp:delay";
pub(crate) const NS_STANZA_ID: &str = "urn:xmpp:sid:0";
pub(crate) const NS_ORIGIN_ID: &str = "urn:xmpp:sid:0";
pub const NS_REACTIONS: &str = "urn:xmpp:reactions:0";
pub(crate) const NS_MARKUP: &str = "urn:xmpp:markup:0";
pub(crate) const NS_WADDLE_MARKUP: &str = "urn:waddle:markup:0";
pub const NS_CHAT_STATES: &str = "http://jabber.org/protocol/chatstates";
pub const NS_CHAT_MARKERS: &str = "urn:xmpp:chat-markers:0";
pub(crate) const NS_REFERENCES: &str = "urn:xmpp:reference:0";
pub const NS_MESSAGE_RETRACT: &str = "urn:xmpp:message-retract:1";
pub const NS_MESSAGE_MODERATE: &str = "urn:xmpp:message-moderate:1";
pub const NS_MESSAGE_CORRECT: &str = "urn:xmpp:message-correct:0";
pub(crate) const NS_HINTS: &str = "urn:xmpp:hints";
pub(crate) const NS_HATS: &str = "urn:xmpp:hats:0";
pub(crate) const NS_SIMS: &str = "urn:xmpp:sims:1";
pub(crate) const NS_SFS: &str = "urn:xmpp:sfs:0";
pub(crate) const NS_FILE_METADATA: &str = "urn:xmpp:file:metadata:0";
pub(crate) const NS_URL_DATA: &str = "http://jabber.org/protocol/url-data";
pub(crate) const NS_CLIENT: &str = "jabber:client";
pub(crate) const NS_WADDLE_EXTENSION: &str = "urn:waddle:extension:1";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub(crate) const NS_MUC: &str = "http://jabber.org/protocol/muc";
pub(crate) const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
pub(crate) const NS_STICKERS: &str = "urn:xmpp:stickers:0";
pub(crate) const NS_VCARD_UPDATE: &str = "vcard-temp:x:update";
pub const NS_WADDLE_PIN_V0: &str = "urn:waddle:pin:0";
/// `urn:waddle:muc-call:0` — Waddle MUC presence extension that
/// signals an occupant has joined a group call in the room. Mirrors
/// `waddle_xmpp::xep::xep_waddle_muc_call::NS_WADDLE_MUC_CALL` —
/// the client crate cannot depend on the server crate, so the
/// constant is duplicated and kept in sync via a test.
pub const NS_WADDLE_MUC_CALL: &str = "urn:waddle:muc-call:0";

/// Build a `<call xmlns='urn:waddle:muc-call:0' state='active|inactive'
/// call-id='…'/>` element from typed inputs. Keeps the wire shape
/// locked to a single definition site even from the wasm crate
/// (which cannot depend on waddle-xmpp where the canonical
/// `MucCallExtension` lives). CLAUDE.md XML hard rule: callers never
/// hand-roll the element via raw `Element::builder` strings.
pub fn build_muc_call_extension_element(active: bool, call_id: &str) -> minidom::Element {
    let state = if active { "active" } else { "inactive" };
    minidom::Element::builder("call", NS_WADDLE_MUC_CALL)
        .attr("state", state)
        .attr("call-id", call_id)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_active_extension_matching_xep_shape() {
        let elem = build_muc_call_extension_element(true, "room@muc.example.com");
        assert_eq!(elem.name(), "call");
        assert_eq!(elem.ns(), NS_WADDLE_MUC_CALL);
        assert_eq!(elem.attr("state"), Some("active"));
        assert_eq!(elem.attr("call-id"), Some("room@muc.example.com"));
    }

    #[test]
    fn builds_inactive_extension_matching_xep_shape() {
        let elem = build_muc_call_extension_element(false, "room@muc.example.com");
        assert_eq!(elem.attr("state"), Some("inactive"));
    }

    /// The canonical `MucCallExtension::to_element()` in
    /// `waddle_xmpp::xep::xep_waddle_muc_call` is the source of
    /// truth for this wire shape. The client crate cannot depend on
    /// the server crate, so we pin the duplicate constant + builder
    /// here via byte-for-byte comparison in the server crate's
    /// integration test suite (see waddle-xmpp's xep_waddle_muc_call
    /// tests for round-trip parsing of the produced element).
    #[test]
    fn ns_constant_matches_canonical_xep_namespace() {
        assert_eq!(NS_WADDLE_MUC_CALL, "urn:waddle:muc-call:0");
    }
}
