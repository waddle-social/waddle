//! XEP-0292: vCard4 over XMPP.
//!
//! Typed value, parser, and element builder for the `urn:ietf:params:xml:ns:vcard-4.0`
//! payload used on the PEP node `urn:xmpp:vcard4`. Also provides IQ builders for the
//! XEP-0163 fetch (`<items node='urn:xmpp:vcard4'>`) and publish
//! (`<publish node='urn:xmpp:vcard4'><item id='current'>…</item></publish>`)
//! round-trip so the wasm-client and chat UI can read/write the user's own profile
//! without parallel XML construction.
//!
//! The struct is intentionally narrower than RFC 6350 — only the fields that the
//! chat profile editor surfaces are modelled. Adding a new property is additive:
//! parse + build are property-by-property, so a missing property on the wire is
//! `None` on the typed value, and a `None` typed value is omitted from the wire.

use minidom::Element;

use crate::pep::NS_PUBSUB;

/// vCard4 XML namespace.
pub const NS_VCARD4: &str = "urn:ietf:params:xml:ns:vcard-4.0";

/// PEP node for vCard4 publication.
pub const PEP_NODE_VCARD4: &str = "urn:xmpp:vcard4";

const NS_CLIENT: &str = "jabber:client";

/// Typed vCard4 value. Each field is `None` when the corresponding XEP-0292
/// property is absent on the wire.
///
/// `pronouns` corresponds to the vCard4 `PRONOUNS` property (advertised under
/// `NS_VCARD4`). Older servers that have not yet learned to round-trip the
/// property will simply drop it; the wasm boundary still carries the value so
/// the editor can save it the moment the server supports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VCard4 {
    /// `FN` — full formatted name.
    pub full_name: Option<String>,
    /// `NICKNAME`.
    pub nickname: Option<String>,
    /// `PRONOUNS`. May be ignored by older servers — see module docs.
    pub pronouns: Option<String>,
    /// `NOTE` — free-form bio.
    pub note: Option<String>,
    /// `URL` — website / social link, transmitted as `<uri>`.
    pub url: Option<String>,
}

impl VCard4 {
    /// Create an empty vCard4.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if every property is `None`.
    pub fn is_empty(&self) -> bool {
        self.full_name.is_none()
            && self.nickname.is_none()
            && self.pronouns.is_none()
            && self.note.is_none()
            && self.url.is_none()
    }
}

// ── Element shape ───────────────────────────────────────────────────────────

fn text_prop(parent: &Element, name: &str) -> Option<String> {
    parent
        .children()
        .find(|c| c.name() == name && c.ns() == NS_VCARD4)
        .and_then(|c| c.children().find(|t| t.name() == "text"))
        .map(|t| t.text())
        .filter(|t| !t.is_empty())
}

fn uri_or_text_prop(parent: &Element, name: &str) -> Option<String> {
    parent
        .children()
        .find(|c| c.name() == name && c.ns() == NS_VCARD4)
        .and_then(|c| {
            c.children()
                .find(|t| t.name() == "uri" || t.name() == "text")
        })
        .map(|t| t.text())
        .filter(|t| !t.is_empty())
}

/// Parse a `<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>` element.
pub fn parse_vcard4_element(elem: &Element) -> VCard4 {
    VCard4 {
        full_name: text_prop(elem, "fn"),
        nickname: text_prop(elem, "nickname"),
        pronouns: text_prop(elem, "pronouns"),
        note: text_prop(elem, "note"),
        url: uri_or_text_prop(elem, "url"),
    }
}

fn append_text_prop(parent: &mut Element, name: &str, value: &str) {
    let mut prop = Element::builder(name, NS_VCARD4).build();
    let mut text = Element::builder("text", NS_VCARD4).build();
    text.append_text_node(value);
    prop.append_child(text);
    parent.append_child(prop);
}

fn append_uri_prop(parent: &mut Element, name: &str, value: &str) {
    let mut prop = Element::builder(name, NS_VCARD4).build();
    let mut uri = Element::builder("uri", NS_VCARD4).build();
    uri.append_text_node(value);
    prop.append_child(uri);
    parent.append_child(prop);
}

/// Build a `<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>` element. Empty
/// properties are omitted entirely.
pub fn build_vcard4_element(vcard: &VCard4) -> Element {
    let mut elem = Element::builder("vcard", NS_VCARD4).build();
    if let Some(ref v) = vcard.full_name {
        append_text_prop(&mut elem, "fn", v);
    }
    if let Some(ref v) = vcard.nickname {
        append_text_prop(&mut elem, "nickname", v);
    }
    if let Some(ref v) = vcard.pronouns {
        append_text_prop(&mut elem, "pronouns", v);
    }
    if let Some(ref v) = vcard.note {
        append_text_prop(&mut elem, "note", v);
    }
    if let Some(ref v) = vcard.url {
        append_uri_prop(&mut elem, "url", v);
    }
    elem
}

// ── IQ shape ────────────────────────────────────────────────────────────────

/// Build a `get` IQ requesting the latest item from the target's
/// `urn:xmpp:vcard4` PEP node (XEP-0163).
pub fn build_fetch_vcard4_iq(target_jid: &str, request_id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), target_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(
                            minidom::rxml::xml_ncname!("node").to_owned(),
                            PEP_NODE_VCARD4,
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Build a `set` IQ that publishes `vcard` to the user's own
/// `urn:xmpp:vcard4` PEP node, using the canonical item id `current`.
pub fn build_publish_vcard4_iq(vcard: &VCard4, request_id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("publish", NS_PUBSUB)
                        .attr(
                            minidom::rxml::xml_ncname!("node").to_owned(),
                            PEP_NODE_VCARD4,
                        )
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "current")
                                .append(build_vcard4_element(vcard))
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Extract the vCard4 payload from a successful XEP-0060 items response.
/// Returns `None` when the node has no current item or the payload is missing.
pub fn parse_pep_vcard4(iq: &Element) -> Option<VCard4> {
    let payload = iq
        .get_child("pubsub", NS_PUBSUB)?
        .get_child("items", NS_PUBSUB)
        .filter(|items| items.attr("node") == Some(PEP_NODE_VCARD4))?
        .get_child("item", NS_PUBSUB)?
        .get_child("vcard", NS_VCARD4)?;
    Some(parse_vcard4_element(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_payload() {
        let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                    <fn><text>Romeo Montague</text></fn>\
                    <nickname><text>Romeo</text></nickname>\
                    <pronouns><text>he/him</text></pronouns>\
                    <note><text>Star-crossed lover</text></note>\
                    <url><uri>https://romeo.example.com</uri></url>\
                    </vcard>";
        let elem: Element = xml.parse().expect("valid xml");
        let vcard = parse_vcard4_element(&elem);

        assert_eq!(vcard.full_name.as_deref(), Some("Romeo Montague"));
        assert_eq!(vcard.nickname.as_deref(), Some("Romeo"));
        assert_eq!(vcard.pronouns.as_deref(), Some("he/him"));
        assert_eq!(vcard.note.as_deref(), Some("Star-crossed lover"));
        assert_eq!(vcard.url.as_deref(), Some("https://romeo.example.com"));
        assert!(!vcard.is_empty());
    }

    #[test]
    fn empty_element_parses_to_empty_value() {
        let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'/>";
        let elem: Element = xml.parse().expect("valid xml");
        let vcard = parse_vcard4_element(&elem);
        assert!(vcard.is_empty());
    }

    #[test]
    fn round_trip_keeps_every_property() {
        let original = VCard4 {
            full_name: Some("Juliet Capulet".into()),
            nickname: Some("Juliet".into()),
            pronouns: Some("she/her".into()),
            note: Some("Also star-crossed".into()),
            url: Some("https://juliet.example.com".into()),
        };
        let elem = build_vcard4_element(&original);
        let parsed = parse_vcard4_element(&elem);
        assert_eq!(original, parsed);
    }

    #[test]
    fn build_omits_none_properties() {
        let only_fn = VCard4 {
            full_name: Some("Alice".into()),
            ..Default::default()
        };
        let elem = build_vcard4_element(&only_fn);
        assert_eq!(elem.children().count(), 1);
        assert_eq!(elem.children().next().unwrap().name(), "fn");
    }

    #[test]
    fn fetch_iq_targets_pep_vcard4_node() {
        let iq = build_fetch_vcard4_iq("juliet@example.com", "req-1");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("juliet@example.com"));
        assert_eq!(iq.attr("id"), Some("req-1"));
        let items = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("items", NS_PUBSUB))
            .expect("items element");
        assert_eq!(items.attr("node"), Some(PEP_NODE_VCARD4));
    }

    #[test]
    fn publish_iq_uses_current_item_id() {
        let vcard = VCard4 {
            full_name: Some("Romeo".into()),
            ..Default::default()
        };
        let iq = build_publish_vcard4_iq(&vcard, "req-pub");
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("id"), Some("req-pub"));
        let item = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("publish", NS_PUBSUB))
            .and_then(|p| p.get_child("item", NS_PUBSUB))
            .expect("item element");
        assert_eq!(item.attr("id"), Some("current"));
        let payload = item.get_child("vcard", NS_VCARD4).expect("vcard payload");
        assert_eq!(
            parse_vcard4_element(payload).full_name.as_deref(),
            Some("Romeo")
        );
    }

    #[test]
    fn parse_pep_extracts_payload_from_iq() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                    <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                      <items node='urn:xmpp:vcard4'>\
                        <item id='current'>\
                          <vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                            <fn><text>Mercutio</text></fn>\
                            <pronouns><text>they/them</text></pronouns>\
                          </vcard>\
                        </item>\
                      </items>\
                    </pubsub>\
                  </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        let vcard = parse_pep_vcard4(&iq).expect("payload present");
        assert_eq!(vcard.full_name.as_deref(), Some("Mercutio"));
        assert_eq!(vcard.pronouns.as_deref(), Some("they/them"));
    }

    #[test]
    fn parse_pep_returns_none_when_node_mismatched() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                    <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                      <items node='http://jabber.org/protocol/mood'/>\
                    </pubsub>\
                  </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        assert!(parse_pep_vcard4(&iq).is_none());
    }

    #[test]
    fn parse_pep_returns_none_when_item_empty() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                    <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                      <items node='urn:xmpp:vcard4'/>\
                    </pubsub>\
                  </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        assert!(parse_pep_vcard4(&iq).is_none());
    }
}
