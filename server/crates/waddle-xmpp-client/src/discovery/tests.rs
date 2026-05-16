use jid::BareJid;
use minidom::Element;

use crate::messaging::MucAffiliation;

use super::*;

#[test]
fn parse_disco_info_result_extracts_features() {
    let iq = Element::builder("iq", CLIENT_NS)
        .attr("type", "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    Element::builder("feature", DISCO_INFO_NS)
                        .attr("var", UPLOAD_NS)
                        .build(),
                )
                .append(
                    Element::builder("feature", DISCO_INFO_NS)
                        .attr("var", "jabber:iq:version")
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
        .attr("type", "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .attr("type", "result")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "FORM_TYPE")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append(PUBSUB_METADATA_FORM_TYPE)
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "pubsub#type")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append(SPACES_NS)
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "pubsub#title")
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
                        .attr("type", "result")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "FORM_TYPE")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("http://jabber.org/protocol/muc#roominfo")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "muc#roominfo_description")
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
        .attr("type", "result")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .append(
                    Element::builder("identity", DISCO_INFO_NS)
                        .attr("category", "store")
                        .attr("type", "file")
                        .attr("name", "HTTP File Upload")
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
        .attr("type", "result")
        .append(
            Element::builder("query", DISCO_ITEMS_NS)
                .append(
                    Element::builder("item", DISCO_ITEMS_NS)
                        .attr("jid", "upload.example.com")
                        .attr("name", "Upload Service")
                        .build(),
                )
                .append(
                    Element::builder("item", DISCO_ITEMS_NS)
                        .attr("jid", "muc.example.com")
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
fn root_service_items_are_not_spaces() {
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    let items = vec![
        DiscoItem {
            jid: "muc.example.com".to_string(),
            name: Some("Chatrooms".to_string()),
            node: None,
        },
        DiscoItem {
            jid: "spaces.example.com".to_string(),
            name: Some("Spaces".to_string()),
            node: None,
        },
        DiscoItem {
            jid: "extensions.example.com".to_string(),
            name: Some("Extensions".to_string()),
            node: None,
        },
    ];

    assert!(parse_spaces_from_disco_items(&spaces_jid, items).is_empty());
}

#[test]
fn spaces_service_items_parse_node_backed_spaces() {
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    let items = vec![DiscoItem {
        jid: "spaces.example.com".to_string(),
        name: Some("General".to_string()),
        node: Some("general".to_string()),
    }];

    let spaces = parse_spaces_from_disco_items(&spaces_jid, items);

    assert_eq!(spaces.len(), 1);
    assert_eq!(spaces[0].id.as_str(), "general");
    assert_eq!(spaces[0].service_jid, spaces_jid);
    assert_eq!(spaces[0].name, "General");
}

#[test]
fn space_from_disco_item_requires_spaces_metadata_type() {
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    let item = DiscoItem {
        jid: "spaces.example.com".to_string(),
        name: Some("Ignored Node Name".to_string()),
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

    let space = space_from_disco_item(&spaces_jid, item.clone(), &space_info).unwrap();

    assert_eq!(space.id.as_str(), "engineering");
    assert_eq!(space.name, "Engineering");
    assert_eq!(space.description.as_deref(), Some("Build systems"));
    assert!(space_from_disco_item(&spaces_jid, item, &other_info).is_none());
}

#[test]
fn pubsub_items_parse_bookmark_channels_and_ignore_non_conference_payloads() {
    let space_id = SpaceNode::new("general").expect("space node");
    let iq = Element::builder("iq", CLIENT_NS)
        .attr("type", "result")
        .append(
            Element::builder("pubsub", PUBSUB_NS)
                .append(
                    Element::builder("items", PUBSUB_NS)
                        .attr("node", "general")
                        .append(
                            Element::builder("item", PUBSUB_NS)
                                .attr("id", "urn:xmpp:spaces:avatar:metadata:0")
                                .append(
                                    Element::builder("metadata", "urn:xmpp:avatar:metadata")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("item", PUBSUB_NS)
                                .attr("id", "chat@muc.example.com")
                                .append(
                                    Element::builder("conference", BOOKMARKS_NS)
                                        .attr("name", "Chat")
                                        .attr("autojoin", "true")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("item", PUBSUB_NS)
                                .attr("id", "not-a-room")
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
        .attr("type", "result")
        .append(
            Element::builder("slot", UPLOAD_NS)
                .append(
                    Element::builder("put", UPLOAD_NS)
                        .attr("url", "https://example.com/upload/file.jpg")
                        .append(
                            Element::builder("header", UPLOAD_NS)
                                .attr("name", "Authorization")
                                .append("Bearer token123")
                                .build(),
                        )
                        .append(
                            Element::builder("header", UPLOAD_NS)
                                .attr("name", "Cookie")
                                .append("session=abc")
                                .build(),
                        )
                        .build(),
                )
                .append(
                    Element::builder("get", UPLOAD_NS)
                        .attr("url", "https://cdn.example.com/file.jpg")
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
        .attr("type", "result")
        .append(
            Element::builder("slot", UPLOAD_NS)
                .append(
                    Element::builder("put", UPLOAD_NS)
                        .attr("url", "https://example.com/upload/file.jpg")
                        .build(),
                )
                .append(
                    Element::builder("get", UPLOAD_NS)
                        .attr("url", "https://cdn.example.com/file.jpg")
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
fn build_enable_push_iq_omits_publish_options_for_empty_token() {
    let iq = build_enable_push_iq("push.example.com", "web-push", "");
    let enable = iq.get_child("enable", PUSH_NS).expect("enable");

    assert_eq!(enable.attr("jid"), Some("push.example.com"));
    assert_eq!(enable.attr("node"), Some("web-push"));
    assert!(enable.get_child("x", DATA_FORMS_NS).is_none());
}

#[test]
fn build_enable_push_iq_includes_secret_publish_options_for_non_empty_token() {
    let iq = build_enable_push_iq("push.example.com", "web-push", "opaque-secret");
    let enable = iq.get_child("enable", PUSH_NS).expect("enable");
    let form = enable
        .get_child("x", DATA_FORMS_NS)
        .expect("publish options");
    let fields = form
        .children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .filter_map(|field| {
            Some((
                field.attr("var")?.to_string(),
                field.get_child("value", DATA_FORMS_NS)?.text(),
            ))
        })
        .collect::<Vec<_>>();

    assert!(fields.iter().any(|(var, value)| {
        var == "FORM_TYPE" && value == "http://jabber.org/protocol/pubsub#publish-options"
    }));
    assert!(fields
        .iter()
        .any(|(var, value)| var == "secret" && value == "opaque-secret"));
}

#[test]
fn build_and_parse_waddle_inbox_round_trip() {
    let iq = build_waddle_inbox_query_iq(
        "me@example.com",
        &WaddleInboxQuery {
            since: Some(1700000),
            only_unread: true,
            room: Some("room@muc.example.com".to_string()),
            threads: true,
        },
    );
    let query = iq.get_child("query", WADDLE_INBOX_NS).expect("inbox query");
    assert_eq!(query.attr("since"), Some("1700000"));
    assert_eq!(query.attr("only-unread"), Some("true"));
    assert_eq!(query.attr("room"), Some("room@muc.example.com"));
    assert_eq!(query.attr("threads"), Some("true"));

    let result = Element::builder("iq", CLIENT_NS)
        .attr("type", "result")
        .append(
            Element::builder("query", WADDLE_INBOX_NS)
                .attr("total-unread", "3")
                .append(
                    Element::builder("conversation", WADDLE_INBOX_NS)
                        .attr("partner", "alice@example.com")
                        .attr("kind", "direct")
                        .attr("last-stanza-id", "sid-1")
                        .attr("last-updated", "1700001")
                        .attr("unread", "2")
                        .append(
                            Element::builder("preview", WADDLE_INBOX_NS)
                                .append("hi there")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let parsed = parse_waddle_inbox_result(&result).expect("parse inbox result");
    assert_eq!(parsed.total_unread, Some(3));
    assert_eq!(parsed.conversations.len(), 1);
    assert_eq!(parsed.conversations[0].partner, "alice@example.com");
    assert_eq!(parsed.conversations[0].preview.as_deref(), Some("hi there"));
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
        .attr("type", "result")
        .append(
            Element::builder("query", ROSTER_NS)
                .attr("ver", "ver-2")
                .append(
                    Element::builder("item", ROSTER_NS)
                        .attr("jid", "alice@example.com")
                        .attr("name", "Alice")
                        .attr("subscription", "both")
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
        .attr("type", "result")
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
        .attr("type", "result")
        .append(
            Element::builder("query", USER_SEARCH_NS)
                .append(
                    Element::builder("item", USER_SEARCH_NS)
                        .attr("jid", "admin@localhost")
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
fn parse_inbox_result_returns_none_for_plain_message() {
    let message = Element::builder("message", CLIENT_NS)
        .attr("from", "alice@example.com")
        .attr("to", "me@example.com")
        .append(Element::builder("body", CLIENT_NS).append("Hello!").build())
        .build();

    assert!(parse_inbox_result(&message).is_none());
}
