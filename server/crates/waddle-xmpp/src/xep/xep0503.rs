//! XEP-0503: Server-side Spaces
//!
//! Implements the read-only surface of XEP-0503 v0.2.0 for native XMPP
//! community discovery. The canonical space is exposed as a pubsub node
//! typed `urn:xmpp:spaces:0`, with channel items represented as XEP-0402
//! `<conference>` bookmarks.
//!
//! The spaces service lives at `spaces.<domain>` and is advertised in
//! `disco#items` alongside the existing MUC/upload/pubsub components.
//!
//! ## Phase A Scope (Read-Only)
//!
//! - `disco#info` on `spaces.<domain>`: returns spaces service identity + features
//! - `disco#info` on `spaces.<domain>` with `node=<space_id>`: returns space node identity
//! - `disco#items` on `spaces.<domain>`: returns the canonical space node
//! - `disco#items` on `spaces.<domain>` with `node=<space_id>`: returns channel items
//! - pubsub `<items>` on `spaces.<domain>` for `node=<space_id>`: returns XEP-0402 bookmark items
//!
//! **Note:** The full XEP-0503 feature set is advertised in `disco#info` even
//! though write operations (create/delete spaces, publish/retract channels,
//! subscribe) return `<service-unavailable/>`. This is acceptable for an
//! Experimental XEP and will be implemented in Phase F.

use minidom::Element;

use super::xep0004::{DataForm, Field, FormType, IntoElement};
use crate::pubsub::stanzas::PubSubItem;
use crate::xep::xep0402;
use crate::{managed_room_jid, ChannelInfo, ChannelType, SpaceDetails, XmppError};

/// XEP-0503 Spaces namespace.
pub const NS_SPACES: &str = "urn:xmpp:spaces:0";

/// Build a pubsub `<item>` for a channel inside a space.
///
/// Each channel is represented as an XEP-0402 `<conference>` bookmark element
/// with the channel's canonical MUC room JID and name. The item ID is the room JID.
pub fn build_channel_item(
    channel: &ChannelInfo,
    muc_domain: &str,
) -> Result<PubSubItem, XmppError> {
    let Some(_channel_type) = ChannelType::parse(&channel.channel_type) else {
        return Err(XmppError::bad_request(Some(format!(
            "Unsupported channel type '{}'",
            channel.channel_type
        ))));
    };
    let room_jid = managed_room_jid(&channel.id, muc_domain).map_err(|e| {
        XmppError::internal(format!(
            "Invalid room JID for managed channel '{}': {}",
            channel.id, e
        ))
    })?;
    let bookmark = xep0402::Bookmark::new(room_jid)
        .with_name(&channel.name)
        .with_autojoin(true);
    let payload = xep0402::build_bookmark_element(&bookmark);

    Ok(PubSubItem {
        id: Some(bookmark.jid.to_string()),
        payload: Some(payload),
    })
}

/// Build a rich `pubsub#meta-data` form for a space node's `disco#info` response.
///
/// Includes all metadata fields specified by XEP-0503: type, title, description,
/// owner, creation date, and access model.
pub fn build_spaces_metadata_form(space: &SpaceDetails) -> Element {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(
            "http://jabber.org/protocol/pubsub#meta-data",
        ))
        .add_field(Field::text_single("pubsub#type", NS_SPACES))
        .add_field(Field::text_single("pubsub#title", &space.name));

    if let Some(ref desc) = space.description {
        form = form.add_field(Field::text_single("pubsub#description", desc));
    }

    form.add_field(Field::text_single("pubsub#owner", &space.owner_id))
        .add_field(Field::text_single(
            "pubsub#creation_date",
            &space.created_at,
        ))
        .add_field(Field::text_single(
            "pubsub#access_model",
            if space.is_public { "open" } else { "whitelist" },
        ))
        .into_element()
}

/// Build a simple `pubsub#type` data form indicating a spaces node.
///
/// Lighter-weight alternative to `build_spaces_metadata_form` when only the
/// node type needs to be advertised.
pub fn build_spaces_type_form() -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(
            "http://jabber.org/protocol/pubsub#meta-data",
        ))
        .add_field(Field::text_single("pubsub#type", NS_SPACES))
        .into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_space() -> SpaceDetails {
        SpaceDetails {
            id: "space-1".to_string(),
            name: "My Space".to_string(),
            description: Some("A test space".to_string()),
            owner_id: "alice".to_string(),
            icon_url: None,
            is_public: true,
            created_at: "2026-01-15T10:00:00Z".to_string(),
        }
    }

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

        let item = build_channel_item(&channel, "muc.example.com").unwrap();

        assert_eq!(item.id, Some("general@muc.example.com".to_string()));
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

        let item = build_channel_item(&channel, "muc.waddle.social").unwrap();

        assert_eq!(item.id, Some("dev-chat@muc.waddle.social".to_string()));

        let payload = item.payload.unwrap();
        assert_eq!(payload.attr("autojoin"), Some("true"));
        assert_eq!(payload.attr("name"), Some("Dev Chat"));
    }

    #[test]
    fn test_build_channel_item_with_special_characters() {
        let channel = ChannelInfo {
            id: "hello-world".to_string(),
            name: "Hello & World <Test>".to_string(),
            channel_type: "text".to_string(),
        };

        let item = build_channel_item(&channel, "muc.example.com").unwrap();
        let payload = item.payload.unwrap();

        // Name should be preserved as-is (XML escaping handled by minidom)
        assert_eq!(payload.attr("name"), Some("Hello & World <Test>"));
    }

    #[test]
    fn test_build_channel_item_rejects_unknown_channel_type() {
        let channel = ChannelInfo {
            id: "voice".to_string(),
            name: "Voice".to_string(),
            channel_type: "voice".to_string(),
        };

        assert!(build_channel_item(&channel, "muc.example.com").is_err());
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
        let form_type_value: String = fields[0].children().next().unwrap().texts().collect();
        assert_eq!(
            form_type_value,
            "http://jabber.org/protocol/pubsub#meta-data"
        );

        // Second field: pubsub#type
        assert_eq!(fields[1].attr("var"), Some("pubsub#type"));
        let type_value: String = fields[1].children().next().unwrap().texts().collect();
        assert_eq!(type_value, NS_SPACES);
    }

    #[test]
    fn test_build_spaces_metadata_form_public_space() {
        let space = test_space();
        let form = build_spaces_metadata_form(&space);

        assert_eq!(form.name(), "x");
        assert_eq!(form.ns(), "jabber:x:data");
        assert_eq!(form.attr("type"), Some("result"));

        let fields: Vec<&Element> = form.children().collect();
        // FORM_TYPE, pubsub#type, pubsub#title, pubsub#description, pubsub#owner,
        // pubsub#creation_date, pubsub#access_model
        assert_eq!(fields.len(), 7);

        // FORM_TYPE
        assert_eq!(fields[0].attr("var"), Some("FORM_TYPE"));
        assert_eq!(fields[0].attr("type"), Some("hidden"));

        // pubsub#type = urn:xmpp:spaces:0
        assert_eq!(fields[1].attr("var"), Some("pubsub#type"));
        let type_val: String = fields[1].children().next().unwrap().texts().collect();
        assert_eq!(type_val, NS_SPACES);

        // pubsub#title
        assert_eq!(fields[2].attr("var"), Some("pubsub#title"));
        let title_val: String = fields[2].children().next().unwrap().texts().collect();
        assert_eq!(title_val, "My Space");

        // pubsub#description
        assert_eq!(fields[3].attr("var"), Some("pubsub#description"));
        let desc_val: String = fields[3].children().next().unwrap().texts().collect();
        assert_eq!(desc_val, "A test space");

        // pubsub#owner
        assert_eq!(fields[4].attr("var"), Some("pubsub#owner"));
        let owner_val: String = fields[4].children().next().unwrap().texts().collect();
        assert_eq!(owner_val, "alice");

        // pubsub#creation_date
        assert_eq!(fields[5].attr("var"), Some("pubsub#creation_date"));
        let date_val: String = fields[5].children().next().unwrap().texts().collect();
        assert_eq!(date_val, "2026-01-15T10:00:00Z");

        // pubsub#access_model = open (public space)
        assert_eq!(fields[6].attr("var"), Some("pubsub#access_model"));
        let access_val: String = fields[6].children().next().unwrap().texts().collect();
        assert_eq!(access_val, "open");
    }

    #[test]
    fn test_build_spaces_metadata_form_private_space() {
        let mut space = test_space();
        space.is_public = false;
        space.description = None;

        let form = build_spaces_metadata_form(&space);
        let fields: Vec<&Element> = form.children().collect();

        // Without description: FORM_TYPE, type, title, owner, creation_date, access_model
        assert_eq!(fields.len(), 6);

        // Last field: pubsub#access_model = whitelist (private space)
        let last = fields.last().unwrap();
        assert_eq!(last.attr("var"), Some("pubsub#access_model"));
        let access_val: String = last.children().next().unwrap().texts().collect();
        assert_eq!(access_val, "whitelist");
    }
}
