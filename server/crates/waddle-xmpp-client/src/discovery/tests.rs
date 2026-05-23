use jid::BareJid;
use minidom::Element;

use crate::messaging::MucAffiliation;

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

// ── urn:waddle:push-service:0 builders (issue #528) ──────────────────────────

#[test]
fn build_ensure_push_node_carries_app_id_and_target_jid() {
    let iq = build_ensure_push_node_iq("push.example.com", "web");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("push.example.com"));
    let ensure = iq
        .get_child("ensure-node", WADDLE_PUSH_SERVICE_NS)
        .expect("ensure-node child");
    assert_eq!(ensure.attr("app-id"), Some("web"));
}

#[test]
fn build_register_push_device_carries_web_push_fields() {
    let registration = PushDeviceRegistration {
        node: "node-abc",
        device_id: "web-1234",
        environment: "prod",
        provider_endpoint: Some("https://fcm.googleapis.com/wp/abcdef"),
        provider_token: Some("vapid-auth-secret"),
        provider_key_material: Some("p256dh-public-key"),
    };
    let iq = build_register_push_device_iq("push.example.com", "web", &registration);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("push.example.com"));
    let register = iq
        .get_child("register-device", WADDLE_PUSH_SERVICE_NS)
        .expect("register-device child");
    assert_eq!(register.attr("node"), Some("node-abc"));
    assert_eq!(register.attr("device-id"), Some("web-1234"));
    assert_eq!(register.attr("platform"), Some("web"));
    assert_eq!(register.attr("environment"), Some("prod"));
    let endpoint = register
        .get_child("provider-endpoint", WADDLE_PUSH_SERVICE_NS)
        .expect("provider-endpoint child");
    assert_eq!(endpoint.text(), "https://fcm.googleapis.com/wp/abcdef");
    let token = register
        .get_child("provider-token", WADDLE_PUSH_SERVICE_NS)
        .expect("provider-token child");
    assert_eq!(token.text(), "vapid-auth-secret");
    let key = register
        .get_child("provider-key-material", WADDLE_PUSH_SERVICE_NS)
        .expect("provider-key-material child");
    assert_eq!(key.text(), "p256dh-public-key");
}

#[test]
fn build_register_push_device_omits_missing_provider_fields() {
    // APNs / FCM (later PRs) populate a subset of the provider fields.
    // Verify the builder omits the elements rather than emitting them
    // empty, so the server's text-extractor returns None.
    let registration = PushDeviceRegistration {
        node: "node-abc",
        device_id: "ios-1234",
        environment: "prod",
        provider_endpoint: None,
        provider_token: Some("apns-device-token"),
        provider_key_material: None,
    };
    // Server-side `PushDevicePlatform::parse` accepts exactly
    // `"web" | "apns" | "fcm"` — use the canonical vocabulary
    // (NOT `"ios"`) so the test exercises an IQ shape the server
    // will actually accept when APNs lands in #529.
    let iq = build_register_push_device_iq("push.example.com", "apns", &registration);
    let register = iq
        .get_child("register-device", WADDLE_PUSH_SERVICE_NS)
        .expect("register-device child");
    assert!(register
        .get_child("provider-endpoint", WADDLE_PUSH_SERVICE_NS)
        .is_none());
    assert!(register
        .get_child("provider-key-material", WADDLE_PUSH_SERVICE_NS)
        .is_none());
    let token = register
        .get_child("provider-token", WADDLE_PUSH_SERVICE_NS)
        .expect("provider-token child");
    assert_eq!(token.text(), "apns-device-token");
}

#[test]
fn build_disable_push_device_carries_node_and_device_id() {
    let iq = build_disable_push_device_iq("push.example.com", "node-abc", "web-1234");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("push.example.com"));
    let disable = iq
        .get_child("disable-device", WADDLE_PUSH_SERVICE_NS)
        .expect("disable-device child");
    assert_eq!(disable.attr("node"), Some("node-abc"));
    assert_eq!(disable.attr("device-id"), Some("web-1234"));
}

/// Acceptance criterion #2 verification — XEP-0357 enable IQ carries
/// the Push Service JID + node and NO provider fields. The existing
/// `build_enable_push_iq_omits_publish_options_for_empty_token` test
/// covers this for the empty-token branch; this test pins the
/// EXACT shape #528 promises to land.
#[test]
fn xep0357_enable_for_web_push_carries_no_provider_fields() {
    let iq = build_enable_push_iq("push.example.com", "node-abc", "");
    let enable = iq.get_child("enable", PUSH_NS).expect("enable");
    assert_eq!(enable.attr("jid"), Some("push.example.com"));
    assert_eq!(enable.attr("node"), Some("node-abc"));
    // No XEP-0004 form, no provider-endpoint, no provider-token, no
    // provider-key-material. The Push Service is the only consumer
    // of those values (registered separately via build_register_push_device_iq).
    assert!(enable.get_child("x", DATA_FORMS_NS).is_none());
    assert!(enable
        .get_child("provider-endpoint", WADDLE_PUSH_SERVICE_NS)
        .is_none());
    assert!(enable
        .get_child("provider-token", WADDLE_PUSH_SERVICE_NS)
        .is_none());
    assert!(enable
        .get_child("provider-key-material", WADDLE_PUSH_SERVICE_NS)
        .is_none());
}
