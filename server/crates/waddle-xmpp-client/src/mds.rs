//! XEP-0490: Message Displayed Synchronization client helpers.
//!
//! Builds the wire IQs for publish, catch-up retrieve, and the
//! self-subscribe used to receive `+notify` events when the chat
//! client's presence does not yet carry XEP-0115 caps. Parsing of the
//! `<displayed/>` payload reuses
//! `waddle_xmpp::xep::xep0490::parse_displayed_element` so the typed
//! `MdsDisplayed` value flows end-to-end.

use minidom::Element;

const NS_CLIENT: &str = "jabber:client";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_DATA_FORMS: &str = "jabber:x:data";

pub const NS_MDS: &str = "urn:xmpp:mds:displayed:0";
pub const MDS_NODE: &str = "urn:xmpp:mds:displayed:0";
pub const NS_MDS_NOTIFY: &str = "urn:xmpp:mds:displayed:0+notify";
pub const NS_STANZA_ID: &str = "urn:xmpp:sid:0";
const PUBSUB_PUBLISH_OPTIONS_FORM_TYPE: &str = "http://jabber.org/protocol/pubsub#publish-options";

/// Typed representation of one MDS catch-up entry. `chat_id` is the
/// PEP item id (= JID of the chat). `stanza_id` is the XEP-0359 id of
/// the displayed message; `stanza_id_by` is the JID that injected the
/// stanza-id (the room for MUC, the user's server for 1:1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdsCatchupEntry {
    pub chat_id: String,
    pub stanza_id: String,
    pub stanza_id_by: String,
}

fn build_publish_options_form() -> Element {
    // XEP-0490 §3 mandates these exact publish-options values.
    let form_type = Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(PUBSUB_PUBLISH_OPTIONS_FORM_TYPE)
                .build(),
        )
        .build();
    let persist = field("pubsub#persist_items", "true");
    let max_items = field("pubsub#max_items", "max");
    let send_last = field("pubsub#send_last_published_item", "never");
    let access = field("pubsub#access_model", "whitelist");
    Element::builder("publish-options", NS_PUBSUB)
        .append(
            Element::builder("x", NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(form_type)
                .append(persist)
                .append(max_items)
                .append(send_last)
                .append(access)
                .build(),
        )
        .build()
}

fn field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

fn build_displayed_payload(stanza_id: &str, stanza_id_by: &str) -> Element {
    Element::builder("displayed", NS_MDS)
        .append(
            Element::builder("stanza-id", NS_STANZA_ID)
                .attr(minidom::rxml::xml_ncname!("by").to_owned(), stanza_id_by)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), stanza_id)
                .build(),
        )
        .build()
}

/// Build the XEP-0490 §3 publish IQ for a single chat-id displayed
/// marker. The IQ is addressed to no `to` attribute, which routes to
/// the user's own PEP service per XEP-0163.
pub fn build_mds_publish_iq(
    iq_id: &str,
    chat_id: &str,
    stanza_id: &str,
    stanza_id_by: &str,
) -> Element {
    let item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), chat_id)
        .append(build_displayed_payload(stanza_id, stanza_id_by))
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), MDS_NODE)
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .append(build_publish_options_form())
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
        .append(pubsub)
        .build()
}

/// Build the XEP-0490 §3.1 catch-up retrieve IQ for the user's own
/// MDS node.
pub fn build_mds_catchup_iq(iq_id: &str) -> Element {
    let items = Element::builder("items", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), MDS_NODE)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
        .append(pubsub)
        .build()
}

/// Build an explicit XEP-0060 `<subscribe>` IQ for the MDS node. Used
/// in place of caps-driven `+notify` filtering when the chat client
/// does not advertise XEP-0115 caps in its presence.
pub fn build_mds_subscribe_iq(iq_id: &str, subscriber_bare_jid: &str) -> Element {
    let subscribe = Element::builder("subscribe", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), MDS_NODE)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            subscriber_bare_jid,
        )
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(subscribe)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
        .append(pubsub)
        .build()
}

fn parse_displayed_into_entry(item: &Element) -> Option<MdsCatchupEntry> {
    let chat_id = item.attr("id")?.to_string();
    let displayed = item.get_child("displayed", NS_MDS)?;
    let stanza_id_elem = displayed.get_child("stanza-id", NS_STANZA_ID)?;
    let stanza_id = stanza_id_elem.attr("id")?.to_string();
    let stanza_id_by = stanza_id_elem.attr("by")?.to_string();
    Some(MdsCatchupEntry {
        chat_id,
        stanza_id,
        stanza_id_by,
    })
}

/// Parse a catch-up `<iq type='result'>` into typed entries. Items
/// that don't carry the expected `<displayed>/<stanza-id>` shape are
/// silently dropped — they're not a XEP-0490 entry and never will be.
pub fn parse_mds_catchup_result(iq: &Element) -> Vec<MdsCatchupEntry> {
    let Some(pubsub) = iq.get_child("pubsub", NS_PUBSUB) else {
        return Vec::new();
    };
    let Some(items) = pubsub
        .get_child("items", NS_PUBSUB)
        .filter(|items| items.attr("node") == Some(MDS_NODE))
    else {
        return Vec::new();
    };
    items
        .children()
        .filter(|child| child.is("item", NS_PUBSUB))
        .filter_map(parse_displayed_into_entry)
        .collect()
}

/// Parse an inbound PEP event (`<message><event><items
/// node='urn:xmpp:mds:displayed:0'>...`) into typed entries. Returns
/// `None` when the message is not an MDS event.
pub fn parse_mds_event(message: &Element) -> Option<Vec<MdsCatchupEntry>> {
    if message.name() != "message" {
        return None;
    }
    let event = message.get_child("event", NS_PUBSUB_EVENT)?;
    let items = event
        .get_child("items", NS_PUBSUB_EVENT)
        .filter(|items| items.attr("node") == Some(MDS_NODE))?;
    let entries = items
        .children()
        .filter(|child| child.is("item", NS_PUBSUB_EVENT))
        .filter_map(|item| {
            let chat_id = item.attr("id")?.to_string();
            let displayed = item.get_child("displayed", NS_MDS)?;
            let stanza_id_elem = displayed.get_child("stanza-id", NS_STANZA_ID)?;
            let stanza_id = stanza_id_elem.attr("id")?.to_string();
            let stanza_id_by = stanza_id_elem.attr("by")?.to_string();
            Some(MdsCatchupEntry {
                chat_id,
                stanza_id,
                stanza_id_by,
            })
        })
        .collect::<Vec<_>>();
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml_of(elem: &Element) -> String {
        String::from(elem)
    }

    #[test]
    fn publish_iq_carries_spec_publish_options() {
        let iq = build_mds_publish_iq(
            "iq-1",
            "romeo@montague.lit",
            "stanza-id-1",
            "juliet@capulet.lit",
        );
        let xml = xml_of(&iq);
        // `minidom` serialises namespace attributes with single
        // quotes and other attributes with double quotes; the test
        // checks the substrings without pinning the quote style for
        // namespace-bearing attributes.
        assert!(xml.contains(r#"node='urn:xmpp:mds:displayed:0'"#), "{xml}");
        assert!(xml.contains(r#"id='romeo@montague.lit'"#), "{xml}");
        assert!(xml.contains("urn:xmpp:mds:displayed:0"), "{xml}");
        assert!(xml.contains(r#"by='juliet@capulet.lit'"#), "{xml}");
        assert!(xml.contains(r#"id='stanza-id-1'"#), "{xml}");
        // Publish-options form fields per XEP-0490 §3.
        assert!(xml.contains("pubsub#persist_items"), "{xml}");
        assert!(xml.contains("pubsub#max_items"), "{xml}");
        assert!(xml.contains("pubsub#access_model"), "{xml}");
        assert!(xml.contains("pubsub#send_last_published_item"), "{xml}");
        assert!(
            xml.contains(">max<"),
            "max_items value must be the literal max: {xml}"
        );
        assert!(
            xml.contains(">whitelist<"),
            "access_model value must be whitelist: {xml}"
        );
        assert!(
            xml.contains(">never<"),
            "send_last_published_item must be never: {xml}"
        );
    }

    #[test]
    fn catchup_iq_targets_mds_node() {
        let iq = build_mds_catchup_iq("iq-cu-1");
        let xml = xml_of(&iq);
        assert!(xml.contains(r#"type='get'"#), "{xml}");
        assert!(xml.contains(r#"node='urn:xmpp:mds:displayed:0'"#), "{xml}");
        assert!(xml.contains("<items"), "{xml}");
    }

    #[test]
    fn subscribe_iq_includes_jid_attribute() {
        let iq = build_mds_subscribe_iq("iq-sub-1", "alice@example.com");
        let xml = xml_of(&iq);
        assert!(xml.contains(r#"node='urn:xmpp:mds:displayed:0'"#), "{xml}");
        assert!(xml.contains(r#"jid='alice@example.com'"#), "{xml}");
        assert!(xml.contains("<subscribe"), "{xml}");
    }

    #[test]
    fn parse_catchup_result_returns_typed_entries() {
        let iq_xml = format!(
            r#"<iq xmlns="{NS_CLIENT}" type="result" id="cu-1">
              <pubsub xmlns="{NS_PUBSUB}">
                <items node="{MDS_NODE}">
                  <item id="romeo@montague.lit">
                    <displayed xmlns="{NS_MDS}">
                      <stanza-id xmlns="{NS_STANZA_ID}" id="dm-sid" by="juliet@capulet.lit"/>
                    </displayed>
                  </item>
                  <item id="example@conference.shakespeare.lit">
                    <displayed xmlns="{NS_MDS}">
                      <stanza-id xmlns="{NS_STANZA_ID}" id="muc-sid" by="example@conference.shakespeare.lit"/>
                    </displayed>
                  </item>
                </items>
              </pubsub>
            </iq>"#
        );
        let iq: Element = iq_xml.parse().expect("parse iq");
        let entries = parse_mds_catchup_result(&iq);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].chat_id, "romeo@montague.lit");
        assert_eq!(entries[0].stanza_id, "dm-sid");
        assert_eq!(entries[0].stanza_id_by, "juliet@capulet.lit");
        assert_eq!(entries[1].chat_id, "example@conference.shakespeare.lit");
        assert_eq!(
            entries[1].stanza_id_by,
            "example@conference.shakespeare.lit"
        );
    }

    #[test]
    fn parse_event_returns_typed_entry() {
        let msg_xml = format!(
            r#"<message xmlns="{NS_CLIENT}" from="alice@example.com" to="alice@example.com/r" type="headline">
              <event xmlns="{NS_PUBSUB_EVENT}">
                <items node="{MDS_NODE}">
                  <item id="romeo@montague.lit">
                    <displayed xmlns="{NS_MDS}">
                      <stanza-id xmlns="{NS_STANZA_ID}" id="evt-sid" by="alice@example.com"/>
                    </displayed>
                  </item>
                </items>
              </event>
            </message>"#
        );
        let msg: Element = msg_xml.parse().expect("parse message");
        let entries = parse_mds_event(&msg).expect("is MDS event");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].chat_id, "romeo@montague.lit");
        assert_eq!(entries[0].stanza_id, "evt-sid");
    }

    #[test]
    fn parse_event_returns_none_for_non_mds_messages() {
        let msg_xml = format!(
            r#"<message xmlns="{NS_CLIENT}" from="alice@example.com" type="headline">
              <event xmlns="{NS_PUBSUB_EVENT}">
                <items node="http://jabber.org/protocol/mood">
                  <item id="current"><mood xmlns="http://jabber.org/protocol/mood"><happy/></mood></item>
                </items>
              </event>
            </message>"#
        );
        let msg: Element = msg_xml.parse().expect("parse message");
        assert!(parse_mds_event(&msg).is_none());
    }
}
