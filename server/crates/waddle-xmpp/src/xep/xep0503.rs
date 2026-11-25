//! XEP-0503: Server-side Spaces
//!
//! Implements Waddle's XEP-0503 version 0.3.0 surface (xsf/xeps
//! `4132c531487fe9450ae90b905b8df950d0d478b2`) for native XMPP community
//! discovery. The canonical space is exposed as a pubsub node typed
//! `urn:xmpp:spaces:0`, with channel items represented as XEP-0402
//! `<conference>` bookmarks backed by the canonical channel catalog.
//!
//! Version 0.3.0 keeps the namespace of 0.2.0. Its two changes are the
//! room `disco#info` field that names a room's Space,
//! `muc#roomconfig_pubsub`, which the room description emits
//! (`muc::roominfo`), and the avatar and banner items of a Space
//! ([`ITEM_SPACE_AVATAR_METADATA`], [`ITEM_SPACE_BANNER_METADATA`]), whose
//! XEP-0084 metadata points at image URLs. A Space's owner may publish
//! either item; [`SpaceImage`] names them and [`space_image_payload`]
//! decides what such an item may carry. The items are stored and served
//! as published, and neither the server nor its clients fetch the images
//! they name (ADR-0020 D4).
//!
//! The spaces service lives at `spaces.<domain>` and is advertised in
//! `disco#items` alongside the existing MUC/upload/pubsub components.
//!
//! - `disco#info` on `spaces.<domain>`: returns spaces service identity + features
//! - `disco#info` on `spaces.<domain>` with `node=<space_id>`: returns space node identity
//! - `disco#items` on `spaces.<domain>`: returns the canonical space node
//! - pubsub `<items>` on `spaces.<domain>` for `node=<space_id>`: returns XEP-0402 bookmark items
//!   and the Space's avatar and banner items
//! - pubsub `<publish/>` and `<retract/>` on the canonical space node update
//!   channel bookmark membership, or set and remove the Space's avatar and
//!   banner items.
//!
//! XEP-0503 defines no access control, membership or inheritance for a
//! Space's children: a subscription to a Space node is not a grant in any
//! of its rooms.

use crate::muc::owner::FIELD_PIN_PERMISSION;
use minidom::Element;
use thiserror::Error;

use super::xep0004::{DataForm, Field, FieldType, FormType, ToElement};
use super::xep0084::NS_AVATAR_METADATA;
use crate::pubsub::stanzas::PubSubItem;
use crate::xep::xep0402;
use crate::{managed_room_jid, ChannelInfo, ChannelType, SpaceDetails, XmppError};

/// XEP-0503 Spaces namespace.
pub const NS_SPACES: &str = "urn:xmpp:spaces:0";

/// Item identifier of a Space's avatar metadata (XEP-0503 0.3.0, "Spaces
/// images"): a XEP-0084 `<metadata/>` payload naming where the image is.
pub const ITEM_SPACE_AVATAR_METADATA: &str = "urn:xmpp:spaces:avatar:metadata:0";

/// Item identifier of a Space's banner metadata (XEP-0503 0.3.0, "Spaces
/// images").
pub const ITEM_SPACE_BANNER_METADATA: &str = "urn:xmpp:spaces:banner:metadata:0";

/// Most `<info/>` elements a Space image's metadata may hold: the
/// alternate formats and resolutions of one image (XEP-0084, "Info
/// Element").
pub const MAX_SPACE_IMAGE_FORMATS: usize = 8;

/// Longest `url` an `<info/>` element of a Space image may carry, in bytes.
pub const MAX_SPACE_IMAGE_URL_LEN: usize = 2048;

/// Longest `id` (the image's hash) an `<info/>` element of a Space image
/// may carry, in bytes.
pub const MAX_SPACE_IMAGE_ID_LEN: usize = 128;

/// One of the images of a Space (XEP-0503 0.3.0, "Spaces images"), named
/// by the id of the Space node item that carries its metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceImage {
    /// The Space's avatar, item [`ITEM_SPACE_AVATAR_METADATA`].
    Avatar,
    /// The Space's banner, item [`ITEM_SPACE_BANNER_METADATA`].
    Banner,
}

impl SpaceImage {
    /// The image a Space node item id names, if it names one. Every other
    /// id of a Space node names a room by its JID (XEP-0402 bookmark).
    #[must_use]
    pub fn from_item_id(item_id: &str) -> Option<Self> {
        match item_id {
            ITEM_SPACE_AVATAR_METADATA => Some(Self::Avatar),
            ITEM_SPACE_BANNER_METADATA => Some(Self::Banner),
            _ => None,
        }
    }

    /// The id of the Space node item that carries this image's metadata.
    #[must_use]
    pub fn item_id(self) -> &'static str {
        match self {
            Self::Avatar => ITEM_SPACE_AVATAR_METADATA,
            Self::Banner => ITEM_SPACE_BANNER_METADATA,
        }
    }
}

/// Why the item of a Space image is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SpaceImageError {
    /// The item carries no payload element.
    #[error("the item carries no payload")]
    MissingPayload,
    /// The item carries more than one payload element. A PubSub item holds
    /// exactly one (XEP-0060 §7.1.3.5), so the several `<metadata/>`
    /// elements XEP-0503 0.3.0 allows are refused rather than cut to the
    /// first; one `<metadata/>` lists every format in its `<info/>`
    /// elements.
    #[error("the item carries more than one payload element")]
    SeveralPayloads,
    /// The payload is not a XEP-0084 `<metadata/>` element.
    #[error("the payload is not a XEP-0084 metadata element")]
    NotMetadata,
    /// The `<metadata/>` element names no image.
    #[error("the metadata names no image")]
    NoImage,
    /// The `<metadata/>` element holds more than
    /// [`MAX_SPACE_IMAGE_FORMATS`] `<info/>` elements.
    #[error("the metadata names more formats than are kept")]
    TooManyFormats,
    /// The `<metadata/>` element holds a `<pointer/>`, which names a
    /// third-party service rather than an image.
    #[error("the metadata holds a pointer")]
    Pointer,
    /// The `<metadata/>` element holds an attribute, text or a child that
    /// XEP-0084 does not define.
    #[error("the metadata holds content XEP-0084 does not define")]
    UnexpectedContent,
    /// An `<info/>` element is malformed in the part named.
    #[error("an info element is malformed: {0}")]
    MalformedInfo(SpaceImageInfoPart),
}

/// The part of a XEP-0084 `<info/>` element that is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SpaceImageInfoPart {
    /// The element holds text or a child; it must be empty.
    #[error("content")]
    Content,
    /// The element carries an attribute XEP-0084 does not define.
    #[error("undefined attribute")]
    Attribute,
    /// `bytes` is missing or no unsigned 32-bit integer.
    #[error("bytes")]
    Bytes,
    /// `height` is no unsigned 16-bit integer.
    #[error("height")]
    Height,
    /// `width` is no unsigned 16-bit integer.
    #[error("width")]
    Width,
    /// `id` is missing, empty, beyond [`MAX_SPACE_IMAGE_ID_LEN`] or holds a
    /// control character.
    #[error("id")]
    Id,
    /// `type` is missing or no `image/` or `video/` content type.
    #[error("type")]
    Type,
    /// `url` is no absolute `http:` or `https:` URL within
    /// [`MAX_SPACE_IMAGE_URL_LEN`].
    #[error("url")]
    Url,
}

/// The payload of the `<item/>` element `item`, published as one of a
/// Space's images, when it is what such an item may carry.
///
/// The item holds exactly one payload element (XEP-0060 §7.1.3.5), checked
/// by [`validate_space_image_metadata`]. Nothing the payload names is
/// fetched or resolved.
pub fn space_image_payload(item: &Element) -> Result<&Element, SpaceImageError> {
    let mut payloads = item.children();
    let payload = payloads.next().ok_or(SpaceImageError::MissingPayload)?;
    if payloads.next().is_some() {
        return Err(SpaceImageError::SeveralPayloads);
    }
    validate_space_image_metadata(payload)?;
    Ok(payload)
}

/// Check that `metadata` is the XEP-0084 `<metadata/>` element of a Space
/// image (XEP-0503 0.3.0, "Spaces images"):
///
/// - a `<metadata xmlns='urn:xmpp:avatar:metadata'/>` element without
///   attributes or text, holding one to [`MAX_SPACE_IMAGE_FORMATS`]
///   `<info/>` elements and nothing else. An empty element, which XEP-0084
///   uses to stop publishing an avatar, names no image; a Space's image is
///   removed by retracting its item. A `<pointer/>` is refused: its content
///   belongs to a third-party service and XEP-0503 does not use it.
/// - each `<info/>` is empty and carries the attributes XEP-0084 defines
///   and no others: `bytes` (an unsigned 32-bit integer), `id` and `type`
///   are required; `height` and `width` (unsigned 16-bit integers) and
///   `url` are optional. `id` is at most [`MAX_SPACE_IMAGE_ID_LEN`] bytes,
///   `type` an `image/` or `video/` content type, and `url` an absolute
///   `http:` or `https:` URL of at most [`MAX_SPACE_IMAGE_URL_LEN`] bytes.
///
/// XEP-0084 also requires one format to be `image/png` and `id` to be a
/// SHA-1 hash; XEP-0503's own example holds a JPEG only, under an `id`
/// that is no hash, so neither is required here.
pub fn validate_space_image_metadata(metadata: &Element) -> Result<(), SpaceImageError> {
    if !metadata.is("metadata", NS_AVATAR_METADATA) {
        return Err(SpaceImageError::NotMetadata);
    }
    if !metadata.attrs().is_empty() || !metadata.text().trim().is_empty() {
        return Err(SpaceImageError::UnexpectedContent);
    }
    let mut formats = 0_usize;
    for child in metadata.children() {
        if child.is("pointer", NS_AVATAR_METADATA) {
            return Err(SpaceImageError::Pointer);
        }
        if !child.is("info", NS_AVATAR_METADATA) {
            return Err(SpaceImageError::UnexpectedContent);
        }
        formats += 1;
        if formats > MAX_SPACE_IMAGE_FORMATS {
            return Err(SpaceImageError::TooManyFormats);
        }
        validate_space_image_info(child)?;
    }
    if formats == 0 {
        return Err(SpaceImageError::NoImage);
    }
    Ok(())
}

/// The attributes an `<info/>` element of XEP-0084 defines.
const INFO_ATTRIBUTES: [&str; 6] = ["bytes", "height", "id", "type", "url", "width"];

fn validate_space_image_info(info: &Element) -> Result<(), SpaceImageError> {
    let malformed = SpaceImageError::MalformedInfo;
    if info.children().next().is_some() || !info.text().trim().is_empty() {
        return Err(malformed(SpaceImageInfoPart::Content));
    }
    for ((namespace, name), _) in info.attrs().iter() {
        let name: &str = name.as_ref();
        if !namespace.is_none() || !INFO_ATTRIBUTES.contains(&name) {
            return Err(malformed(SpaceImageInfoPart::Attribute));
        }
    }
    if !info
        .attr("bytes")
        .is_some_and(|bytes| bytes.parse::<u32>().is_ok())
    {
        return Err(malformed(SpaceImageInfoPart::Bytes));
    }
    for (dimension, part) in [
        ("height", SpaceImageInfoPart::Height),
        ("width", SpaceImageInfoPart::Width),
    ] {
        if info
            .attr(dimension)
            .is_some_and(|value| value.parse::<u16>().is_err())
        {
            return Err(malformed(part));
        }
    }
    if !info.attr("id").is_some_and(|id| {
        !id.is_empty() && id.len() <= MAX_SPACE_IMAGE_ID_LEN && !id.chars().any(char::is_control)
    }) {
        return Err(malformed(SpaceImageInfoPart::Id));
    }
    if !info.attr("type").is_some_and(is_image_content_type) {
        return Err(malformed(SpaceImageInfoPart::Type));
    }
    if info.attr("url").is_some_and(|url| !is_http_url(url)) {
        return Err(malformed(SpaceImageInfoPart::Url));
    }
    Ok(())
}

/// Whether `content_type` is an `image/` or `video/` content type
/// (XEP-0084, "Info Element") whose subtype is an RFC 6838 §4.2
/// `restricted-name`. Parameters are not part of an image's type.
fn is_image_content_type(content_type: &str) -> bool {
    let Some((top, subtype)) = content_type.split_once('/') else {
        return false;
    };
    (top.eq_ignore_ascii_case("image") || top.eq_ignore_ascii_case("video"))
        && is_restricted_name(subtype)
}

/// RFC 6838 §4.2 `restricted-name`: one to 127 characters, the first
/// alphanumeric, the rest alphanumeric or one of `!#$&-^_.+`.
fn is_restricted_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 127
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '!' | '#' | '$' | '&' | '-' | '^' | '_' | '.' | '+')
        })
}

/// Whether `url` is an absolute `http:` or `https:` URL with a host, within
/// [`MAX_SPACE_IMAGE_URL_LEN`]. It is parsed, never resolved.
fn is_http_url(url: &str) -> bool {
    url.len() <= MAX_SPACE_IMAGE_URL_LEN
        && url::Url::parse(url)
            .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https") && parsed.has_host())
}

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

/// Build the `pubsub#meta-data` form of a space node as an asker at another
/// server is told it: the node's type, which is how XEP-0503 recognises a
/// Space, and its title.
///
/// The addresses of the node's owners and creator, its creation date, its
/// access model and the asker's affiliation are left out: they describe
/// accounts and policy of this deployment, and XEP-0503 requires none of
/// them of a Space.
pub fn build_spaces_metadata_form_for_remote(title: &str) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(
            "http://jabber.org/protocol/pubsub#meta-data",
        ))
        .add_field(Field::text_single("pubsub#type", NS_SPACES))
        .add_field(Field::text_single("pubsub#title", title))
        .to_element()
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

/// Build Waddle-specific room metadata for values not standardized by XEP-0045.
///
/// `pin_permission` carries the room's current
/// `{urn:waddle:roomconfig}pinpermission` value (#415, #422). Surfacing it
/// here on disco-info lets non-owner clients render the Pin action
/// correctly under the `anyone` policy without going through the
/// owner-config GET (which would require Owner affiliation).
pub fn build_room_metadata_form(channel_type: &str, pin_permission: &str) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NS_WADDLE_ROOM_METADATA))
        .add_field(Field::text_single("waddle#channel_type", channel_type))
        .add_field(Field::text_single(FIELD_PIN_PERMISSION, pin_permission))
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
    fn test_build_space_parent_form() {
        let form = build_space_parent_form("spaces.example.com", "general & ops");

        assert_eq!(form.ns(), "jabber:x:data");
        let parent = form
            .children()
            .find(|field| field.attr("var") == Some("parent"))
            .expect("parent field");
        let parent_value: String = parent.children().next().unwrap().texts().collect();
        assert_eq!(
            parent_value,
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
