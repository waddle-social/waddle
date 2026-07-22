use jid::BareJid;
use minidom::Element;

use crate::messaging::{MucAffiliation, MucRole};

use super::parsing::resolve_component_services;
use super::*;

#[test]
fn parse_disco_info_result_extracts_features() {
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    Element::builder("feature", DISCO_INFO_NS)
                        .attr(minidom::rxml::xml_ncname!("var").to_owned(), UPLOAD_NS)
                        .build(),
                )
                .append(
                    Element::builder("feature", DISCO_INFO_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("var").to_owned(),
                            "jabber:iq:version",
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let result = parse_disco_info_result(&iq, "upload.example.com").unwrap();
    assert_eq!(result.jid, "upload.example.com");
    assert!(result.has_feature(UPLOAD_NS));
    assert!(result.has_feature("jabber:iq:version"));
    assert!(!result.has_feature("urn:xmpp:nonexistent"));
}

#[test]
fn parse_disco_info_result_extracts_data_form_metadata() {
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append(PUBSUB_METADATA_FORM_TYPE)
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "pubsub#type")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append(SPACES_NS)
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "pubsub#title")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("Engineering")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("http://jabber.org/protocol/muc#roominfo")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(
                                    minidom::rxml::xml_ncname!("var").to_owned(),
                                    "muc#roominfo_description",
                                )
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("Project discussion")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let result = parse_disco_info_result(&iq, "spaces.example.com").unwrap();

    assert!(result.has_form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#type", SPACES_NS));
    assert_eq!(
        result.form_value(PUBSUB_METADATA_FORM_TYPE, "pubsub#title"),
        Some("Engineering")
    );
    assert_eq!(
        result.form_value(
            "http://jabber.org/protocol/muc#roominfo",
            "muc#roominfo_description"
        ),
        Some("Project discussion")
    );
}

#[test]
fn parse_disco_info_result_extracts_identities() {
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    Element::builder("identity", DISCO_INFO_NS)
                        .attr(minidom::rxml::xml_ncname!("category").to_owned(), "store")
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "file")
                        .attr(
                            minidom::rxml::xml_ncname!("name").to_owned(),
                            "HTTP File Upload",
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let result = parse_disco_info_result(&iq, "upload.example.com").unwrap();
    assert_eq!(result.identities.len(), 1);
    let id = &result.identities[0];
    assert_eq!(id.category, "store");
    assert_eq!(id.identity_type, "file");
    assert_eq!(id.name.as_deref(), Some("HTTP File Upload"));
}

#[test]
fn parse_disco_items_result_extracts_items() {
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", DISCO_ITEMS_NS)
                .append(
                    Element::builder("item", DISCO_ITEMS_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("jid").to_owned(),
                            "upload.example.com",
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("name").to_owned(),
                            "Upload Service",
                        )
                        .build(),
                )
                .append(
                    Element::builder("item", DISCO_ITEMS_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("jid").to_owned(),
                            "muc.example.com",
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let items = parse_disco_items_result(&iq).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].jid, "upload.example.com");
    assert_eq!(items[0].name.as_deref(), Some("Upload Service"));
    assert_eq!(items[1].jid, "muc.example.com");
    assert!(items[1].name.is_none());
}

#[test]
fn space_from_disco_item_requires_spaces_metadata() {
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    let item = DiscoItem {
        jid: "spaces.example.com".to_string(),
        name: Some("Node Name".to_string()),
        node: Some("engineering".to_string()),
    };
    let space_info = DiscoInfoResult {
        jid: "spaces.example.com".to_string(),
        node: Some("engineering".to_string()),
        identities: vec![],
        features: vec![],
        forms: vec![DiscoDataForm {
            form_type: Some(PUBSUB_METADATA_FORM_TYPE.to_string()),
            fields: vec![
                DiscoDataField {
                    var: "FORM_TYPE".to_string(),
                    values: vec![PUBSUB_METADATA_FORM_TYPE.to_string()],
                },
                DiscoDataField {
                    var: "pubsub#type".to_string(),
                    values: vec![SPACES_NS.to_string()],
                },
                DiscoDataField {
                    var: "pubsub#title".to_string(),
                    values: vec!["Engineering".to_string()],
                },
                DiscoDataField {
                    var: "pubsub#description".to_string(),
                    values: vec!["Build systems".to_string()],
                },
            ],
        }],
    };
    let other_info = DiscoInfoResult {
        forms: vec![],
        ..space_info.clone()
    };
    let missing_node_info = DiscoInfoResult {
        node: None,
        ..space_info.clone()
    };

    let space = space_from_disco_item(&spaces_jid, item.clone(), &space_info).unwrap();

    assert_eq!(space.id.as_str(), "engineering");
    assert_eq!(space.name, "Engineering");
    assert_eq!(space.description.as_deref(), Some("Build systems"));

    assert!(space_from_disco_item(&spaces_jid, item.clone(), &other_info).is_none());
    assert!(space_from_disco_item(&spaces_jid, item, &missing_node_info).is_none());
}

#[test]
fn pubsub_items_parse_bookmark_channels_and_ignore_non_conference_payloads() {
    let space_id = SpaceNode::new("general").expect("space node");
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("pubsub", PUBSUB_NS)
                .append(
                    Element::builder("items", PUBSUB_NS)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                        .append(
                            Element::builder("item", PUBSUB_NS)
                                .attr(
                                    minidom::rxml::xml_ncname!("id").to_owned(),
                                    "urn:xmpp:spaces:avatar:metadata:0",
                                )
                                .append(
                                    Element::builder("metadata", "urn:xmpp:avatar:metadata")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("item", PUBSUB_NS)
                                .attr(
                                    minidom::rxml::xml_ncname!("id").to_owned(),
                                    "chat@muc.example.com",
                                )
                                .append(
                                    Element::builder("conference", BOOKMARKS_NS)
                                        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Chat")
                                        .attr(
                                            minidom::rxml::xml_ncname!("autojoin").to_owned(),
                                            "true",
                                        )
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("item", PUBSUB_NS)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "not-a-room")
                                .append(
                                    Element::builder("note", "urn:example:note")
                                        .append("ignore me")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let channels = parse_space_channels_result(&iq, &space_id).expect("channels");

    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].id, "general::chat@muc.example.com");
    assert_eq!(
        channels[0].room_jid,
        "chat@muc.example.com".parse::<BareJid>().expect("room jid")
    );
    assert_eq!(channels[0].name, "Chat");
    assert_eq!(channels[0].channel_type, DiscoveredChannelType::Text);
    assert_eq!(channels[0].position, 0);
    assert_eq!(channels[0].space_id.as_str(), "general");
    // The space bookmark's own autojoin attr is honoured (web parity);
    // user-PEP stamping and group-DM detection happen later.
    assert!(channels[0].autojoin);
    assert!(channels[0].bookmark_name.is_none());
    assert!(!channels[0].is_group_dm);
}

#[test]
fn space_channel_autojoin_defaults_to_false_when_attr_absent() {
    // XEP-0402 §2.2 default (web parity: `bookmark.autojoin ?? false`).
    let space_id = SpaceNode::new("general").expect("space node");
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("pubsub", PUBSUB_NS)
                .append(
                    Element::builder("items", PUBSUB_NS)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                        .append(
                            Element::builder("item", PUBSUB_NS)
                                .attr(
                                    minidom::rxml::xml_ncname!("id").to_owned(),
                                    "quiet@muc.example.com",
                                )
                                .append(Element::builder("conference", BOOKMARKS_NS).build())
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let channels = parse_space_channels_result(&iq, &space_id).expect("channels");
    assert_eq!(channels.len(), 1);
    assert!(!channels[0].autojoin);
}

#[test]
fn discovered_channel_type_parses_waddle_metadata_values() {
    assert_eq!(
        DiscoveredChannelType::from_metadata("text"),
        Some(DiscoveredChannelType::Text)
    );
    assert_eq!(
        DiscoveredChannelType::from_metadata("announcement"),
        Some(DiscoveredChannelType::Announcement)
    );
    assert_eq!(
        DiscoveredChannelType::from_metadata("forum"),
        Some(DiscoveredChannelType::Forum)
    );
    assert_eq!(DiscoveredChannelType::from_metadata("unknown"), None);
}

#[test]
fn parse_upload_slot_extracts_urls_and_headers() {
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("slot", UPLOAD_NS)
                .append(
                    Element::builder("put", UPLOAD_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("url").to_owned(),
                            "https://example.com/upload/file.jpg",
                        )
                        .append(
                            Element::builder("header", UPLOAD_NS)
                                .attr(
                                    minidom::rxml::xml_ncname!("name").to_owned(),
                                    "Authorization",
                                )
                                .append("Bearer token123")
                                .build(),
                        )
                        .append(
                            Element::builder("header", UPLOAD_NS)
                                .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Cookie")
                                .append("session=abc")
                                .build(),
                        )
                        .build(),
                )
                .append(
                    Element::builder("get", UPLOAD_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("url").to_owned(),
                            "https://cdn.example.com/file.jpg",
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let slot = parse_upload_slot(&iq).unwrap();
    assert_eq!(slot.put_url, "https://example.com/upload/file.jpg");
    assert_eq!(slot.get_url, "https://cdn.example.com/file.jpg");
    assert_eq!(slot.put_headers.len(), 2);
    assert_eq!(
        slot.put_headers[0],
        ("Authorization".to_string(), "Bearer token123".to_string())
    );
    assert_eq!(
        slot.put_headers[1],
        ("Cookie".to_string(), "session=abc".to_string())
    );
}

#[test]
fn parse_upload_slot_no_headers_ok() {
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("slot", UPLOAD_NS)
                .append(
                    Element::builder("put", UPLOAD_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("url").to_owned(),
                            "https://example.com/upload/file.jpg",
                        )
                        .build(),
                )
                .append(
                    Element::builder("get", UPLOAD_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("url").to_owned(),
                            "https://cdn.example.com/file.jpg",
                        )
                        .build(),
                )
                .build(),
        )
        .build();

    let slot = parse_upload_slot(&iq).unwrap();
    assert_eq!(slot.put_url, "https://example.com/upload/file.jpg");
    assert_eq!(slot.get_url, "https://cdn.example.com/file.jpg");
    assert!(slot.put_headers.is_empty());
}

#[test]
fn disco_info_result_has_feature_check() {
    let result = DiscoInfoResult {
        jid: "example.com".to_string(),
        node: None,
        identities: vec![],
        features: vec![UPLOAD_NS.to_string(), "jabber:iq:ping".to_string()],
        forms: vec![],
    };
    assert!(result.has_feature(UPLOAD_NS));
    assert!(result.has_feature("jabber:iq:ping"));
    assert!(!result.has_feature("urn:xmpp:nonexistent"));
}

#[test]
fn build_waddle_inbox_mark_read_supports_thread() {
    let iq = build_waddle_inbox_mark_read_iq(
        "me@example.com",
        &WaddleInboxMarkRead {
            partner: "room@muc.example.com".to_string(),
            thread: Some("thread-42".to_string()),
        },
    );
    let mark_read = iq
        .get_child("mark-read", WADDLE_INBOX_NS)
        .expect("mark-read");
    assert_eq!(mark_read.attr("partner"), Some("room@muc.example.com"));
    assert_eq!(mark_read.attr("thread"), Some("thread-42"));
}

#[test]
fn build_and_parse_roster_result() {
    let get = build_roster_get_iq(None, Some("ver-1"));
    let query = get.get_child("query", ROSTER_NS).expect("roster query");
    assert_eq!(query.attr("ver"), Some("ver-1"));

    let result = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", ROSTER_NS)
                .attr(minidom::rxml::xml_ncname!("ver").to_owned(), "ver-2")
                .append(
                    Element::builder("item", ROSTER_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("jid").to_owned(),
                            "alice@example.com",
                        )
                        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Alice")
                        .attr(
                            minidom::rxml::xml_ncname!("subscription").to_owned(),
                            "both",
                        )
                        .append(
                            Element::builder("group", ROSTER_NS)
                                .append("Friends")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let parsed = parse_roster_result(&result).expect("parse roster");
    assert_eq!(parsed.ver.as_deref(), Some("ver-2"));
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.items[0].jid.to_string(), "alice@example.com");
    assert_eq!(parsed.items[0].name.as_deref(), Some("Alice"));
}

#[test]
fn build_and_parse_user_search_queries() {
    let form_iq = build_user_search_form_iq("localhost");
    assert!(form_iq.get_child("query", USER_SEARCH_NS).is_some());

    let search_iq = build_user_search_iq(
        "localhost",
        &UserSearchQuery {
            nick: Some("admin".to_string()),
            email: None,
            first: None,
            last: None,
        },
    );
    assert_eq!(
        search_iq
            .get_child("query", USER_SEARCH_NS)
            .and_then(|query| query.get_child("nick", USER_SEARCH_NS))
            .map(|child| child.text()),
        Some("admin".to_string())
    );

    let form_result = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", USER_SEARCH_NS)
                .append(
                    Element::builder("instructions", USER_SEARCH_NS)
                        .append("Search users")
                        .build(),
                )
                .append(Element::builder("nick", USER_SEARCH_NS).build())
                .append(Element::builder("email", USER_SEARCH_NS).build())
                .build(),
        )
        .build();
    let parsed_form = parse_user_search_form(&form_result).expect("parse form");
    assert_eq!(parsed_form.instructions.as_deref(), Some("Search users"));
    assert_eq!(parsed_form.fields, vec!["nick", "email"]);

    let result = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", USER_SEARCH_NS)
                .append(
                    Element::builder("item", USER_SEARCH_NS)
                        .attr(
                            minidom::rxml::xml_ncname!("jid").to_owned(),
                            "admin@localhost",
                        )
                        .append(
                            Element::builder("nick", USER_SEARCH_NS)
                                .append("admin")
                                .build(),
                        )
                        .append(
                            Element::builder("email", USER_SEARCH_NS)
                                .append("admin@localhost")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let parsed = parse_user_search_result(&result).expect("parse search result");
    assert_eq!(parsed.items[0].jid, "admin@localhost");
    assert_eq!(parsed.items[0].nick.as_deref(), Some("admin"));
}

#[test]
fn build_and_parse_muc_admin_affiliation_queries() {
    let list = build_muc_admin_affiliation_list_iq("room@muc.example.com", MucAffiliation::Member);
    assert_eq!(list.attr("type"), Some("get"));
    assert_eq!(
        list.get_child("query", MUC_ADMIN_NS)
            .and_then(|query| query.get_child("item", MUC_ADMIN_NS))
            .and_then(|item| item.attr("affiliation")),
        Some("member")
    );

    let set = build_muc_admin_affiliation_set_iq(
        "room@muc.example.com",
        &[MucAdminAffiliationItem {
            jid: Some("alice@example.com".to_string()),
            nick: None,
            affiliation: Some(MucAffiliation::Admin),
            reason: Some("promoted".to_string()),
        }],
    );
    let parsed = parse_muc_admin_affiliation_query(&set).expect("parse admin set");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].jid.as_deref(), Some("alice@example.com"));
    assert_eq!(parsed[0].affiliation, Some(MucAffiliation::Admin));
    assert_eq!(parsed[0].reason.as_deref(), Some("promoted"));
}

#[test]
fn build_muc_admin_role_set_iq_matches_xep0045_kick_shape() {
    // XEP-0045 §8.2 example 91: kick = <item nick='…' role='none'>
    // with an optional <reason/>, sent as IQ-set to the room.
    let kick = build_muc_admin_role_set_iq(
        "room@muc.example.com",
        "pistol",
        MucRole::None,
        Some("Avaunt, you cullion!"),
    );
    assert_eq!(kick.attr("type"), Some("set"));
    assert_eq!(kick.attr("to"), Some("room@muc.example.com"));
    let item = kick
        .get_child("query", MUC_ADMIN_NS)
        .and_then(|query| query.get_child("item", MUC_ADMIN_NS))
        .expect("item");
    assert_eq!(item.attr("nick"), Some("pistol"));
    assert_eq!(item.attr("role"), Some("none"));
    assert_eq!(item.attr("jid"), None);
    assert_eq!(item.attr("affiliation"), None);
    assert_eq!(
        item.get_child("reason", MUC_ADMIN_NS).map(Element::text),
        Some("Avaunt, you cullion!".to_string())
    );
}

#[test]
fn build_muc_admin_role_set_iq_omits_empty_reason() {
    let kick = build_muc_admin_role_set_iq("room@muc.example.com", "pistol", MucRole::None, None);
    let item = kick
        .get_child("query", MUC_ADMIN_NS)
        .and_then(|query| query.get_child("item", MUC_ADMIN_NS))
        .expect("item");
    assert!(item.get_child("reason", MUC_ADMIN_NS).is_none());
}

// ── Component service resolution ─────────────────────────────────────

fn info(jid: &str, features: &[&str], identities: &[(&str, &str)]) -> DiscoInfoResult {
    DiscoInfoResult {
        jid: jid.to_string(),
        node: None,
        identities: identities
            .iter()
            .map(|(category, identity_type)| DiscoIdentity {
                category: (*category).to_string(),
                identity_type: (*identity_type).to_string(),
                name: None,
            })
            .collect(),
        features: features.iter().map(|f| (*f).to_string()).collect(),
        forms: Vec::new(),
    }
}

fn bare(jid: &str) -> BareJid {
    jid.parse().expect("test JID parses")
}

#[test]
fn resolve_component_services_picks_explicitly_advertised_jids() {
    // Standard waddle deployment: the bare server domain advertises
    // both component children via disco#items + disco#info; resolver
    // returns those, ignoring the fallback values.
    let items = vec![
        (
            bare("muc.waddle.test"),
            Some(info("muc.waddle.test", &[MUC_NS], &[])),
        ),
        (
            bare("spaces.waddle.test"),
            Some(info("spaces.waddle.test", &[SPACES_NS], &[])),
        ),
    ];
    let resolved = resolve_component_services(
        &items,
        bare("muc-fallback.waddle.test"),
        bare("spaces-fallback.waddle.test"),
    );
    assert_eq!(resolved.muc, bare("muc.waddle.test"));
    assert_eq!(resolved.spaces, bare("spaces.waddle.test"));
}

#[test]
fn resolve_component_services_falls_back_for_unadvertised_components() {
    // Minimally configured server that only advertises MUC. The
    // resolver must still produce a Spaces fallback so
    // discover_topology can attempt the pubsub query (and
    // gracefully return zero spaces if the fallback JID doesn't
    // exist either).
    let items = vec![(
        bare("muc.waddle.test"),
        Some(info("muc.waddle.test", &[MUC_NS], &[])),
    )];
    let resolved = resolve_component_services(
        &items,
        bare("muc-fallback.waddle.test"),
        bare("spaces.waddle.test"),
    );
    assert_eq!(resolved.muc, bare("muc.waddle.test"));
    assert_eq!(resolved.spaces, bare("spaces.waddle.test"));
}

#[test]
fn resolve_component_services_accepts_conference_identity_as_muc_fallback() {
    // Legacy XEP-0045 deployments often advertise the MUC component
    // only by identity category, not feature URI. The resolver
    // honours either.
    let items = vec![(
        bare("rooms.legacy.test"),
        Some(info("rooms.legacy.test", &[], &[("conference", "text")])),
    )];
    let resolved = resolve_component_services(
        &items,
        bare("muc-fallback.legacy.test"),
        bare("spaces-fallback.legacy.test"),
    );
    assert_eq!(resolved.muc, bare("rooms.legacy.test"));
}

#[test]
fn resolve_component_services_does_not_pick_generic_pubsub_as_spaces() {
    // Waddle's extensions component is also pubsub. Selecting it
    // for "Spaces" would route every space query at the wrong
    // service. Only the explicit XEP-0503 feature counts.
    let items = vec![(
        bare("pubsub.waddle.test"),
        Some(info(
            "pubsub.waddle.test",
            &["http://jabber.org/protocol/pubsub"],
            &[],
        )),
    )];
    let resolved = resolve_component_services(
        &items,
        bare("muc-fallback.waddle.test"),
        bare("spaces-fallback.waddle.test"),
    );
    // Spaces falls back — extensions component is not eligible.
    assert_eq!(resolved.spaces, bare("spaces-fallback.waddle.test"));
}

#[test]
fn resolve_component_services_skips_entries_without_info() {
    // disco#info on the component can fail (timeout, error iq).
    // Entries with `None` info are skipped so the fallback wins
    // rather than the resolver mis-classifying an unknown
    // component.
    let items = vec![(bare("unknown.waddle.test"), None)];
    let resolved = resolve_component_services(
        &items,
        bare("muc-fallback.waddle.test"),
        bare("spaces-fallback.waddle.test"),
    );
    assert_eq!(resolved.muc, bare("muc-fallback.waddle.test"));
    assert_eq!(resolved.spaces, bare("spaces-fallback.waddle.test"));
}

#[test]
fn resolve_component_services_first_match_wins() {
    // Two MUC components — resolver picks the first encountered
    // and ignores subsequent matches (server presents components
    // in a stable order, so this is deterministic on the wire).
    let items = vec![
        (
            bare("muc1.waddle.test"),
            Some(info("muc1.waddle.test", &[MUC_NS], &[])),
        ),
        (
            bare("muc2.waddle.test"),
            Some(info("muc2.waddle.test", &[MUC_NS], &[])),
        ),
    ];
    let resolved = resolve_component_services(
        &items,
        bare("muc-fallback.waddle.test"),
        bare("spaces-fallback.waddle.test"),
    );
    assert_eq!(resolved.muc, bare("muc1.waddle.test"));
}

// XEP-0068 form-type discrimination — these tests pin the disco#info
// parser's refusal to honor a smuggled FORM_TYPE in a non-disco form.
// Push command wire-shape coverage lives alongside the typed builders
// in `xep/xep0050.rs`, `xep/xep0357.rs`, and the integration test at
// `tests/register_push_device_against_fake_service.rs`.

#[test]
fn parse_disco_data_form_ignores_form_type_in_submit_form() {
    // XEP-0068 §4.3: a FORM_TYPE field is only a context indicator
    // when (a) the field itself carries `type='hidden'` AND (b) the
    // wrapping `<x/>` is `type='form'` or `type='result'`. A
    // `<x type='submit'>` form is a user-supplied payload, NOT a
    // disco-style advertisement — accepting its FORM_TYPE would let a
    // malicious server smuggle an attacker-claimed context indicator
    // past downstream callers that key off `form.form_type`.
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    // The hostile shape: `<x type='submit'>` with a
                    // well-formed hidden FORM_TYPE field. The parser
                    // MUST drop form_type to None even though the
                    // field itself is hidden.
                    Element::builder("x", DATA_FORMS_NS)
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("urn:waddle:push:vapid:0")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "public-key")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("BNcRdreALRFXTkOOUHK1")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let result = parse_disco_info_result(&iq, "push.example.com").expect("parses");
    assert_eq!(
        result.forms.len(),
        1,
        "submit form is still surfaced — only its form_type is ignored"
    );
    assert_eq!(
        result.forms[0].form_type, None,
        "FORM_TYPE in a `submit` form MUST NOT act as a context indicator"
    );
    // Fields themselves are preserved verbatim so a downstream caller
    // that knows the form is a submit payload (e.g. publish-options)
    // can still consume them.
    assert!(result.forms[0].fields.iter().any(|f| f.var == "FORM_TYPE"
        && f.values.first().map(|v| v.as_str()) == Some("urn:waddle:push:vapid:0")));
}

#[test]
fn parse_disco_data_form_ignores_form_type_when_x_type_missing() {
    // `<x/>` without a `type=` attribute is malformed per XEP-0004 §3.1.
    // We refuse to honor any FORM_TYPE inside it.
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("urn:example:something:0")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let result = parse_disco_info_result(&iq, "push.example.com").expect("parses");
    assert_eq!(result.forms[0].form_type, None);
}

// XEP-0357 wire-shape coverage moved to
// `crate::xep::xep0357::tests::enable_iq_with_no_publish_options_carries_no_form_or_provider_fields`
// where it lives alongside the typed builder.

// ── XEP-0402 bookmark merge (discover_topology step 3) ───────────────

mod bookmark_merge {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use futures::executor::block_on;

    use super::super::bookmarks::{
        bookmark_only_channel, fetch_user_bookmarks, merge_bookmarks_into_channels, stamp_bookmark,
    };
    use super::*;
    use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
    use crate::push::CommandDriver;
    use crate::xep::xep0402::BookmarkItem;

    struct MockDriver {
        responses: RefCell<VecDeque<ClientResult<Element>>>,
        sent: RefCell<Vec<Element>>,
    }

    impl MockDriver {
        fn new(responses: Vec<ClientResult<Element>>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                sent: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandDriver for MockDriver {
        async fn send_iq(&self, iq: Element) -> ClientResult<Element> {
            self.sent.borrow_mut().push(iq);
            self.responses
                .borrow_mut()
                .pop_front()
                .expect("mock response queued")
        }
    }

    fn stanza_error() -> ClientError {
        ClientError::StanzaError(StanzaError {
            error_type: StanzaErrorType::Cancel,
            condition: "item-not-found".to_string(),
            text: None,
            application_condition: None,
        })
    }

    fn bookmark(jid: &str, name: Option<&str>, autojoin: bool) -> BookmarkItem {
        BookmarkItem {
            jid: jid.parse().expect("bookmark jid"),
            name: name.map(str::to_string),
            autojoin,
            nick: None,
            password: None,
            extensions: Vec::new(),
        }
    }

    fn catalog_channel(jid: &str) -> DiscoveredChannel {
        let room_jid: BareJid = jid.parse().expect("room jid");
        DiscoveredChannel {
            id: format!("{}::{}", STANDALONE_SPACE_ID, room_jid),
            room_jid,
            name: "Catalog".to_string(),
            description: None,
            channel_type: DiscoveredChannelType::Text,
            position: 0,
            space_id: standalone(),
            autojoin: true,
            bookmark_name: None,
            is_group_dm: false,
        }
    }

    fn standalone() -> SpaceNode {
        SpaceNode::new(STANDALONE_SPACE_ID).expect("standalone id")
    }

    fn bookmarks_response(items: Vec<BookmarkItem>) -> Element {
        let mut items_builder = Element::builder("items", PUBSUB_NS)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), BOOKMARKS_NS);
        for item in items {
            items_builder = items_builder.append(
                Element::builder("item", PUBSUB_NS)
                    .attr(
                        minidom::rxml::xml_ncname!("id").to_owned(),
                        item.jid.to_string(),
                    )
                    .append(item.build_conference_element())
                    .build(),
            );
        }
        Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("pubsub", PUBSUB_NS)
                    .append(items_builder.build())
                    .build(),
            )
            .build()
    }

    fn disco_info_response(features: &[&str]) -> Element {
        let mut query = Element::builder("query", DISCO_INFO_NS);
        for feature in features {
            query = query.append(
                Element::builder("feature", DISCO_INFO_NS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), *feature)
                    .build(),
            );
        }
        Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(query.build())
            .build()
    }

    fn muc_info() -> DiscoInfoResult {
        DiscoInfoResult {
            jid: "room@muc.waddle.test".to_string(),
            node: None,
            identities: vec![],
            features: vec![MUC_NS.to_string()],
            forms: vec![],
        }
    }

    #[test]
    fn failed_bookmarks_fetch_degrades_to_empty_list() {
        // Web parity: the whole pubsub fetch sits in a bare
        // `try/catch` — any failure means "no bookmarks", never a
        // failed topology load.
        let driver = MockDriver::new(vec![Err(stanza_error())]);
        assert!(block_on(fetch_user_bookmarks(&driver)).is_empty());
    }

    #[test]
    fn fetch_parses_own_pep_bookmark_items() {
        let driver = MockDriver::new(vec![Ok(bookmarks_response(vec![bookmark(
            "chat@muc.waddle.test",
            Some("Chat"),
            true,
        )]))]);
        let items = block_on(fetch_user_bookmarks(&driver));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].jid.to_string(), "chat@muc.waddle.test");
        // The fetch IQ must omit `to=` so the server routes it to the
        // account's own PEP service (XEP-0163 §3.5).
        assert!(driver.sent.borrow()[0].attr("to").is_none());
    }

    #[test]
    fn bookmark_stamps_autojoin_and_name_onto_catalog_channel() {
        let mut channels = vec![catalog_channel("chat@muc.waddle.test")];
        let driver = MockDriver::new(vec![]);
        let appended = block_on(merge_bookmarks_into_channels(
            &driver,
            &mut channels,
            vec![bookmark("chat@muc.waddle.test", Some("Named"), false)],
            &standalone(),
        ));
        assert_eq!(appended, 0);
        // The bookmark's explicit autojoin=false wins over the
        // catalog default of true.
        assert!(!channels[0].autojoin);
        assert_eq!(channels[0].bookmark_name.as_deref(), Some("Named"));
        // No disco#info round-trip for a room the catalog already has.
        assert!(driver.sent.borrow().is_empty());
    }

    #[test]
    fn non_bookmarked_catalog_channel_keeps_autojoin_default() {
        let mut channels = vec![catalog_channel("chat@muc.waddle.test")];
        let driver = MockDriver::new(vec![]);
        block_on(merge_bookmarks_into_channels(
            &driver,
            &mut channels,
            Vec::new(),
            &standalone(),
        ));
        assert!(channels[0].autojoin);
        assert!(channels[0].bookmark_name.is_none());
    }

    #[test]
    fn bookmark_only_room_kept_when_disco_confirms_muc() {
        let mut channels = vec![catalog_channel("chat@muc.waddle.test")];
        let driver = MockDriver::new(vec![Ok(disco_info_response(&[MUC_NS]))]);
        let appended = block_on(merge_bookmarks_into_channels(
            &driver,
            &mut channels,
            vec![bookmark("secret@muc.waddle.test", Some("Secret"), true)],
            &standalone(),
        ));
        assert_eq!(appended, 1);
        assert_eq!(channels.len(), 2);
        let added = &channels[1];
        assert_eq!(added.room_jid.to_string(), "secret@muc.waddle.test");
        assert_eq!(added.name, "Secret");
        assert_eq!(added.bookmark_name.as_deref(), Some("Secret"));
        assert!(added.autojoin);
        assert!(!added.is_group_dm);
        assert_eq!(added.space_id.as_str(), STANDALONE_SPACE_ID);
        assert_eq!(added.channel_type, DiscoveredChannelType::Text);
    }

    #[test]
    fn bookmark_only_room_dropped_without_muc_feature() {
        // A stale bookmark for a destroyed or non-MUC entity must not
        // surface a phantom channel.
        let mut channels = Vec::new();
        let driver = MockDriver::new(vec![Ok(disco_info_response(&["urn:xmpp:mam:2"]))]);
        let appended = block_on(merge_bookmarks_into_channels(
            &driver,
            &mut channels,
            vec![bookmark("gone@muc.waddle.test", None, true)],
            &standalone(),
        ));
        assert_eq!(appended, 0);
        assert!(channels.is_empty());
    }

    #[test]
    fn bookmark_only_room_dropped_when_disco_info_fails() {
        let mut channels = Vec::new();
        let driver = MockDriver::new(vec![Err(stanza_error())]);
        let appended = block_on(merge_bookmarks_into_channels(
            &driver,
            &mut channels,
            vec![bookmark("gone@muc.waddle.test", None, true)],
            &standalone(),
        ));
        assert_eq!(appended, 0);
        assert!(channels.is_empty());
    }

    #[test]
    fn bookmark_only_group_dm_carries_the_flag() {
        let mut channels = Vec::new();
        let driver = MockDriver::new(vec![Ok(disco_info_response(&[
            MUC_NS,
            WADDLE_GROUP_DM_FEATURE_NS,
        ]))]);
        let appended = block_on(merge_bookmarks_into_channels(
            &driver,
            &mut channels,
            vec![bookmark("gdm@muc.waddle.test", Some("Trio"), true)],
            &standalone(),
        ));
        assert_eq!(appended, 1);
        assert!(channels[0].is_group_dm);
        assert!(channels[0].autojoin);
    }

    #[test]
    fn bookmark_only_channel_reads_waddle_channel_type_metadata() {
        let info = DiscoInfoResult {
            forms: vec![DiscoDataForm {
                form_type: Some(WADDLE_ROOM_METADATA_FORM_TYPE.to_string()),
                fields: vec![DiscoDataField {
                    var: "waddle#channel_type".to_string(),
                    values: vec!["forum".to_string()],
                }],
            }],
            ..muc_info()
        };
        let channel = bookmark_only_channel(
            &bookmark("forum@muc.waddle.test", None, false),
            &info,
            &standalone(),
            3,
        )
        .expect("kept");
        assert_eq!(channel.channel_type, DiscoveredChannelType::Forum);
        assert_eq!(channel.position, 3);
        // No bookmark name: fall back to the room localpart.
        assert_eq!(channel.name, "forum");
        assert!(!channel.autojoin);
    }

    #[test]
    fn stamp_bookmark_overwrites_previous_bookmark_state() {
        let mut channel = catalog_channel("chat@muc.waddle.test");
        channel.autojoin = false;
        channel.bookmark_name = Some("Old".to_string());
        stamp_bookmark(&mut channel, &bookmark("chat@muc.waddle.test", None, true));
        assert!(channel.autojoin);
        assert!(channel.bookmark_name.is_none());
    }
}
