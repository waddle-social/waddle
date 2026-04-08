//! XEP-0503: Server-side Spaces
//!
//! Implements the read-only surface of XEP-0503 v0.2.0 for native XMPP
//! community discovery. Each Waddle (community) is exposed as a pubsub node
//! typed `urn:xmpp:spaces:0`, with channel items represented as XEP-0402
//! `<conference>` bookmarks.
//!
//! The spaces service lives at `spaces.<domain>` and is advertised in
//! `disco#items` alongside the existing MUC/upload/pubsub components.
//!
//! ## Phase A Scope (Read-Only)
//!
//! - `disco#info` on `spaces.<domain>`: returns spaces service identity + features
//! - `disco#info` on `spaces.<domain>` with `node=<waddle_id>`: returns space node identity
//! - `disco#items` on `spaces.<domain>`: returns list of space nodes (waddles)
//! - `disco#items` on `spaces.<domain>` with `node=<waddle_id>`: returns channel items
//! - pubsub `<items>` on `spaces.<domain>` for `node=<waddle_id>`: returns XEP-0402 bookmark items
//!
//! Write operations (create/delete spaces, publish/retract channels, subscribe)
//! return `<service-unavailable/>` and are planned for Phase F.

use minidom::Element;

use crate::pubsub::stanzas::PubSubItem;
use crate::xep::xep0402;
use crate::ChannelInfo;

/// XEP-0503 Spaces namespace.
pub const NS_SPACES: &str = "urn:xmpp:spaces:0";

/// Build a pubsub `<item>` for a channel inside a space.
///
/// Each channel is represented as an XEP-0402 `<conference>` bookmark element
/// with the channel's MUC room JID and name. The item ID is the channel ID.
pub fn build_channel_item(channel: &ChannelInfo, muc_domain: &str) -> PubSubItem {
    let room_jid_str = format!("{}@{}", channel.id, muc_domain);
    let room_jid: jid::BareJid = room_jid_str
        .parse()
        .expect("Channel ID + MUC domain should form a valid bare JID");
    let bookmark = xep0402::Bookmark::new(room_jid)
        .with_name(&channel.name)
        .with_autojoin(true);
    let payload = xep0402::build_bookmark_element(&bookmark);

    PubSubItem {
        id: Some(channel.id.clone()),
        payload: Some(payload),
    }
}

/// Build a `pubsub#type` data form indicating a spaces node.
///
/// This form is included in `disco#info` responses for individual space nodes
/// to advertise the node type as `urn:xmpp:spaces:0` per XEP-0503 §3.
pub fn build_spaces_type_form() -> Element {
    Element::builder("x", "jabber:x:data")
        .attr("type", "result")
        .append(
            Element::builder("field", "jabber:x:data")
                .attr("var", "FORM_TYPE")
                .attr("type", "hidden")
                .append(
                    Element::builder("value", "jabber:x:data")
                        .append("http://jabber.org/protocol/pubsub#meta-data")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", "jabber:x:data")
                .attr("var", "pubsub#type")
                .append(
                    Element::builder("value", "jabber:x:data")
                        .append(NS_SPACES)
                        .build(),
                )
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ns_spaces_constant() {
        assert_eq!(NS_SPACES, "urn:xmpp:spaces:0");
    }

    #[test]
    fn test_build_channel_item() {
        let channel = ChannelInfo {
            id: "general".to_string(),
            name: "General".to_string(),
            channel_type: "text".to_string(),
        };

        let item = build_channel_item(&channel, "muc.example.com");

        assert_eq!(item.id, Some("general".to_string()));
        assert!(item.payload.is_some());

        let payload = item.payload.unwrap();
        assert_eq!(payload.name(), "conference");
        assert_eq!(payload.ns(), xep0402::NS_BOOKMARKS2);
        assert_eq!(payload.attr("name"), Some("General"));
        assert_eq!(payload.attr("autojoin"), Some("true"));
    }

    #[test]
    fn test_build_channel_item_jid_format() {
        let channel = ChannelInfo {
            id: "dev-chat".to_string(),
            name: "Dev Chat".to_string(),
            channel_type: "text".to_string(),
        };

        let item = build_channel_item(&channel, "muc.waddle.social");

        // The item ID is the channel ID
        assert_eq!(item.id, Some("dev-chat".to_string()));

        // The bookmark payload should reference the MUC room JID
        let payload = item.payload.unwrap();
        // The bookmark element has autojoin=true and name set
        assert_eq!(payload.attr("autojoin"), Some("true"));
        assert_eq!(payload.attr("name"), Some("Dev Chat"));
    }

    #[test]
    fn test_build_spaces_type_form() {
        let form = build_spaces_type_form();

        assert_eq!(form.name(), "x");
        assert_eq!(form.ns(), "jabber:x:data");
        assert_eq!(form.attr("type"), Some("result"));

        let fields: Vec<&Element> = form.children().collect();
        assert_eq!(fields.len(), 2);

        // First field: FORM_TYPE
        assert_eq!(fields[0].attr("var"), Some("FORM_TYPE"));
        assert_eq!(fields[0].attr("type"), Some("hidden"));
        let form_type_value: String = fields[0]
            .children()
            .next()
            .unwrap()
            .texts()
            .collect();
        assert_eq!(
            form_type_value,
            "http://jabber.org/protocol/pubsub#meta-data"
        );

        // Second field: pubsub#type
        assert_eq!(fields[1].attr("var"), Some("pubsub#type"));
        let type_value: String = fields[1]
            .children()
            .next()
            .unwrap()
            .texts()
            .collect();
        assert_eq!(type_value, NS_SPACES);
    }

    #[test]
    fn test_build_channel_item_with_special_characters() {
        let channel = ChannelInfo {
            id: "hello-world".to_string(),
            name: "Hello & World <Test>".to_string(),
            channel_type: "text".to_string(),
        };

        let item = build_channel_item(&channel, "muc.example.com");
        let payload = item.payload.unwrap();

        // Name should be preserved as-is (XML escaping handled by minidom)
        assert_eq!(payload.attr("name"), Some("Hello & World <Test>"));
    }
}
