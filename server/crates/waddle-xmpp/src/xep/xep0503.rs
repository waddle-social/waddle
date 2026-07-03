//! XEP-0503: Server-side Spaces
//!
//! Implements Waddle's XEP-0503 v0.2.0 surface for native XMPP community
//! discovery. The canonical space is exposed as a pubsub node typed
//! `urn:xmpp:spaces:0`, with channel items represented as XEP-0402
//! `<conference>` bookmarks backed by the canonical channel catalog.
//!
//! The spaces service lives at `spaces.<domain>` and is advertised in
//! `disco#items` alongside the existing MUC/upload/pubsub components.
//!
//! - `disco#info` on `spaces.<domain>`: returns spaces service identity + features
//! - `disco#info` on `spaces.<domain>` with `node=<space_id>`: returns space node identity
//! - `disco#items` on `spaces.<domain>`: returns the canonical space node
//! - pubsub `<items>` on `spaces.<domain>` for `node=<space_id>`: returns XEP-0402 bookmark items
//! - pubsub `<publish/>` and `<retract/>` on the canonical space node update
//!   channel bookmark membership.

use minidom::Element;

use super::xep0004::{DataForm, Field, FieldType, FormType, ToElement};
use crate::pubsub::stanzas::PubSubItem;
use crate::xep::xep0402;
use crate::{managed_room_jid, ChannelInfo, ChannelType, SpaceDetails, XmppError};

/// XEP-0503 Spaces namespace.
pub const NS_SPACES: &str = "urn:xmpp:spaces:0";

/// Waddle's service-discovery form namespace for authenticated server metadata.
pub const NS_WADDLE_SERVER_INFO: &str = "urn:waddle:server-info:0";

/// Waddle room metadata carried in disco#info extension forms.
pub const NS_WADDLE_ROOM_METADATA: &str = "urn:waddle:room:0";

/// XEP-0060 PubSub affiliation for a requester against a Space node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceAffiliation {
    Owner,
    Publisher,
    Member,
    Outcast,
    None,
}

impl SpaceAffiliation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Publisher => "publisher",
            Self::Member => "member",
            Self::Outcast => "outcast",
            Self::None => "none",
        }
    }
}

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
        publisher: None,
        payload: Some(payload),
    })
}

/// Build a rich `pubsub#meta-data` form for a space node's `disco#info` response.
///
/// Includes all metadata fields specified by XEP-0503: type, title, description,
/// owner, creation date, and access model.
pub fn build_spaces_metadata_form(space: &SpaceDetails) -> Element {
    build_spaces_metadata_form_for_requester(space, None)
}

/// Build a rich `pubsub#meta-data` form with an optional requester affiliation.
pub fn build_spaces_metadata_form_for_requester(
    space: &SpaceDetails,
    requester_affiliation: Option<SpaceAffiliation>,
) -> Element {
    build_spaces_metadata_form_for_requester_with_owners(
        space,
        requester_affiliation,
        std::slice::from_ref(&space.owner_id),
    )
}

/// Build a rich `pubsub#meta-data` form with explicit owner JIDs.
pub fn build_spaces_metadata_form_for_requester_with_owners(
    space: &SpaceDetails,
    requester_affiliation: Option<SpaceAffiliation>,
    owner_jids: &[String],
) -> Element {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(
            "http://jabber.org/protocol/pubsub#meta-data",
        ))
        .add_field(Field::text_single("pubsub#type", NS_SPACES))
        .add_field(Field::text_single("pubsub#title", &space.name));

    if let Some(ref desc) = space.description {
        form = form.add_field(Field::text_single("pubsub#description", desc));
    }

    let owners = if owner_jids.is_empty() {
        vec![space.owner_id.clone()]
    } else {
        owner_jids.to_vec()
    };
    let creator = owners
        .first()
        .cloned()
        .unwrap_or_else(|| space.owner_id.clone());
    let mut owner_field = Field::new("pubsub#owner", FieldType::JidMulti);
    for owner in &owners {
        owner_field = owner_field.add_value(owner);
    }

    form = form
        .add_field(Field::new("pubsub#creator", FieldType::JidSingle).with_value(&creator))
        .add_field(owner_field)
        .add_field(Field::text_single(
            "pubsub#creation_date",
            &space.created_at,
        ))
        .add_field(Field::text_single(
            "pubsub#access_model",
            space.access_model.as_str(),
        ));

    if let Some(affiliation) = requester_affiliation {
        form = form.add_field(Field::text_single(
            "pubsub#affiliation",
            affiliation.as_str(),
        ));
    }

    form.to_element()
}

/// Build authenticated server role metadata for XEP-0030 disco#info.
pub fn build_server_role_form(role: SpaceAffiliation) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NS_WADDLE_SERVER_INFO))
        .add_field(Field::text_single(
            "waddle#server_affiliation",
            role.as_str(),
        ))
        .to_element()
}

/// Build the XEP-0503 space node IRI used by room disco metadata.
pub fn build_space_node_iri(spaces_service: &str, node: &str) -> String {
    let encoded_node = url::form_urlencoded::byte_serialize(node.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    format!("xmpp:{spaces_service}?;node={encoded_node}")
}

/// Build the `urn:xmpp:spaces:0` parent metadata form for an entity in a space.
pub fn build_space_parent_form(spaces_service: &str, node: &str) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NS_SPACES))
        .add_field(Field::text_single(
            "parent",
            build_space_node_iri(spaces_service, node),
        ))
        .to_element()
}

/// Build the MUC roominfo compatibility form for room disco metadata.
pub fn build_muc_roominfo_form(
    spaces_service: &str,
    node: &str,
    description: Option<&str>,
) -> Element {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type("http://jabber.org/protocol/muc#roominfo"))
        .add_field(Field::text_single(
            "muc#roomconfig_pubsub",
            build_space_node_iri(spaces_service, node),
        ));
    if let Some(description) = description.filter(|value| !value.trim().is_empty()) {
        form = form.add_field(Field::text_single("muc#roominfo_description", description));
    }
    form.to_element()
}

/// Build the MUC roominfo compatibility form for XEP-0503 parent metadata.
pub fn build_muc_roominfo_pubsub_form(spaces_service: &str, node: &str) -> Element {
    build_muc_roominfo_form(spaces_service, node, None)
}

/// Build all XEP-0503 room disco extension forms for a room linked to a space.
pub fn build_room_space_metadata_forms(spaces_service: &str, node: &str) -> Vec<Element> {
    build_room_space_metadata_forms_with_description(spaces_service, node, None)
}

pub fn build_room_space_metadata_forms_with_description(
    spaces_service: &str,
    node: &str,
    description: Option<&str>,
) -> Vec<Element> {
    vec![
        build_space_parent_form(spaces_service, node),
        build_muc_roominfo_form(spaces_service, node, description),
    ]
}

/// Build Waddle-specific room metadata for values not standardized by XEP-0045.
///
/// `pin_permission` carries the room's current
/// `urn:waddle:roomconfig:pinpermission` value (#415, #422). Surfacing it
/// here on disco-info lets non-owner clients render the Pin action
/// correctly under the `anyone` policy without going through the
/// owner-config GET (which would require Owner affiliation).
pub fn build_room_metadata_form(channel_type: &str, pin_permission: &str) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NS_WADDLE_ROOM_METADATA))
        .add_field(Field::text_single("waddle#channel_type", channel_type))
        .add_field(Field::text_single(
            crate::muc::owner::FIELD_PIN_PERMISSION,
            pin_permission,
        ))
        .to_element()
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
        .to_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_space() -> SpaceDetails {
        SpaceDetails {
            id: "space-1".to_string(),
            name: "My Space".to_string(),
            description: Some("A test space".to_string()),
            owner_id: "alice@example.com".to_string(),
            icon_url: None,
            is_public: true,
            access_model: crate::SpaceAccessModel::Open,
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
    fn test_build_room_space_metadata_forms() {
        let forms = build_room_space_metadata_forms("spaces.example.com", "general & ops");

        assert_eq!(forms.len(), 2);
        assert_eq!(forms[0].ns(), "jabber:x:data");
        assert_eq!(forms[1].ns(), "jabber:x:data");

        let parent = forms[0]
            .children()
            .find(|field| field.attr("var") == Some("parent"))
            .expect("parent field");
        let parent_value: String = parent.children().next().unwrap().texts().collect();
        assert_eq!(
            parent_value,
            "xmpp:spaces.example.com?;node=general%20%26%20ops"
        );

        let roominfo = forms[1]
            .children()
            .find(|field| field.attr("var") == Some("muc#roomconfig_pubsub"))
            .expect("muc roominfo pubsub field");
        let roominfo_value: String = roominfo.children().next().unwrap().texts().collect();
        assert_eq!(
            roominfo_value,
            "xmpp:spaces.example.com?;node=general%20%26%20ops"
        );
    }

    fn pin_permission_value(form: &Element) -> String {
        let field = form
            .children()
            .find(|field| field.attr("var") == Some(crate::muc::owner::FIELD_PIN_PERMISSION))
            .expect("pin permission field");
        field.children().next().unwrap().texts().collect()
    }

    #[test]
    fn test_build_room_metadata_form_anyone() {
        let form = build_room_metadata_form("forum", "anyone");
        let form_type = form
            .children()
            .find(|field| field.attr("var") == Some("FORM_TYPE"))
            .expect("FORM_TYPE field");
        let form_type_value: String = form_type.children().next().unwrap().texts().collect();
        assert_eq!(form_type_value, NS_WADDLE_ROOM_METADATA);

        let channel_type = form
            .children()
            .find(|field| field.attr("var") == Some("waddle#channel_type"))
            .expect("channel type field");
        let channel_type_value: String = channel_type.children().next().unwrap().texts().collect();
        assert_eq!(channel_type_value, "forum");

        // #422: pin permission is surfaced on disco-info so non-owners
        // can render the Pin action correctly under the `anyone` policy.
        assert_eq!(pin_permission_value(&form), "anyone");
    }

    /// #422: companion to `test_build_room_metadata_form_anyone` —
    /// confirms the `admins-only` policy round-trips on disco. Both
    /// values are tested so a regression in either branch is caught.
    #[test]
    fn test_build_room_metadata_form_admins_only() {
        let form = build_room_metadata_form("text", "admins-only");
        assert_eq!(pin_permission_value(&form), "admins-only");
    }

    #[test]
    fn test_build_spaces_metadata_form_public_space() {
        let space = test_space();
        let form = build_spaces_metadata_form(&space);

        assert_eq!(form.name(), "x");
        assert_eq!(form.ns(), "jabber:x:data");
        assert_eq!(form.attr("type"), Some("result"));

        let fields: Vec<&Element> = form.children().collect();
        // FORM_TYPE, pubsub#type, pubsub#title, pubsub#description,
        // pubsub#creator, pubsub#owner, pubsub#creation_date,
        // pubsub#access_model.
        assert_eq!(fields.len(), 8);

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

        // pubsub#creator
        assert_eq!(fields[4].attr("var"), Some("pubsub#creator"));
        assert_eq!(fields[4].attr("type"), Some("jid-single"));
        let creator_val: String = fields[4].children().next().unwrap().texts().collect();
        assert_eq!(creator_val, "alice@example.com");

        // pubsub#owner
        assert_eq!(fields[5].attr("var"), Some("pubsub#owner"));
        assert_eq!(fields[5].attr("type"), Some("jid-multi"));
        let owner_val: String = fields[5].children().next().unwrap().texts().collect();
        assert_eq!(owner_val, "alice@example.com");

        // pubsub#creation_date
        assert_eq!(fields[6].attr("var"), Some("pubsub#creation_date"));
        let date_val: String = fields[6].children().next().unwrap().texts().collect();
        assert_eq!(date_val, "2026-01-15T10:00:00Z");

        // pubsub#access_model = open (public space)
        assert_eq!(fields[7].attr("var"), Some("pubsub#access_model"));
        let access_val: String = fields[7].children().next().unwrap().texts().collect();
        assert_eq!(access_val, "open");
    }

    #[test]
    fn test_build_spaces_metadata_form_private_space() {
        let mut space = test_space();
        space.is_public = false;
        space.access_model = crate::SpaceAccessModel::Whitelist;
        space.description = None;

        let form = build_spaces_metadata_form(&space);
        let fields: Vec<&Element> = form.children().collect();

        // Without description: FORM_TYPE, type, title, creator, owner,
        // creation_date, access_model.
        assert_eq!(fields.len(), 7);

        // Last field: pubsub#access_model = whitelist (private space)
        let last = fields.last().unwrap();
        assert_eq!(last.attr("var"), Some("pubsub#access_model"));
        let access_val: String = last.children().next().unwrap().texts().collect();
        assert_eq!(access_val, "whitelist");
    }
}
