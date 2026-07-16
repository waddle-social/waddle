//! Dedicated test suite for the `urn:waddle:admin:*` command surface
//! (XEP-0050 execute requests + XEP-0004 form results). Ported from
//! the wasm crate when the implementation moved into shared core.

use minidom::Element;

use crate::xep::xep0050::{NS_COMMANDS, NS_DATA_FORMS};

use super::*;

fn wrap_response(node: &str, form_inner: &str) -> Element {
    let raw = format!(
        r#"<iq xmlns='jabber:client' type='result' id='test'><command xmlns='http://jabber.org/protocol/commands' node='{node}' status='completed'><x xmlns='jabber:x:data' type='result'><field type="hidden" var="FORM_TYPE"><value>{node}</value></field>{form_inner}</x></command></iq>"#
    );
    raw.parse().expect("parse iq")
}

fn form_of(iq: &Element) -> &Element {
    iq.get_child("command", NS_COMMANDS)
        .and_then(|command| command.get_child("x", NS_DATA_FORMS))
        .expect("form")
}

fn field_vars(form: &Element) -> Vec<&str> {
    form.children()
        .filter(|child| child.name() == "field")
        .filter_map(|field| field.attr("var"))
        .collect()
}

fn field_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .filter(|child| child.name() == "field" && child.attr("var") == Some(var))
        .find_map(|field| field.get_child("value", NS_DATA_FORMS).map(Element::text))
}

// ── Request shape (XEP-0050 §2.4 + XEP-0068 FORM_TYPE) ───────────────

#[test]
fn users_list_iq_executes_command_with_mirrored_form_type() {
    let iq = build_admin_users_list_iq(
        "localhost",
        &AdminUsersListArgs {
            prefix: Some("al".to_string()),
            page_size: Some(50),
            after_cursor: None,
        },
    );
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("localhost"));
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(command.attr("node"), Some(NODE_ADMIN_USERS_LIST));
    assert_eq!(command.attr("action"), Some("execute"));
    let form = form_of(&iq);
    assert_eq!(form.attr("type"), Some("submit"));
    assert_eq!(
        field_value(form, "FORM_TYPE").as_deref(),
        Some(NODE_ADMIN_USERS_LIST)
    );
    assert_eq!(field_value(form, "prefix").as_deref(), Some("al"));
    assert_eq!(field_value(form, "page_size").as_deref(), Some("50"));
    assert_eq!(field_value(form, "after_cursor"), None);
}

#[test]
fn build_spaces_list_iq_omits_unset_fields() {
    let iq = build_admin_spaces_list_iq("localhost", &AdminSpacesListArgs::default());
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("to"), Some("localhost"));
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(command.attr("node"), Some(NODE_ADMIN_SPACES_LIST));
    assert_eq!(field_vars(form_of(&iq)), vec!["FORM_TYPE"]);
}

#[test]
fn build_channels_create_iq_includes_all_optional_fields() {
    let args = AdminChannelsCreateArgs {
        name: "general".to_string(),
        topic: Some("All things".to_string()),
        channel_type: Some("forum".to_string()),
        space_jid: Some("eng@spaces.localhost".to_string()),
        space_node: Some("eng".to_string()),
        is_public: Some(true),
        members_only: Some(true),
    };
    let iq = build_admin_channels_create_iq("localhost", &args);
    let vars = field_vars(form_of(&iq));
    for var in [
        "name",
        "topic",
        "channel_type",
        "space_jid",
        "space_node",
        "is_public",
        "members_only",
    ] {
        assert!(vars.contains(&var), "missing {var}");
    }
    assert_eq!(field_value(form_of(&iq), "is_public").as_deref(), Some("1"));
}

#[test]
fn build_spaces_delete_iq_carries_confirm() {
    let args = AdminSpacesDeleteArgs {
        space_jid: "eng@spaces.localhost".to_string(),
        space_node: Some("eng".to_string()),
        confirm: CONFIRM_YES.to_string(),
    };
    let iq = build_admin_spaces_delete_iq("localhost", &args);
    let form = form_of(&iq);
    assert_eq!(field_value(form, "confirm").as_deref(), Some("yes"));
    assert_eq!(field_value(form, "space_node").as_deref(), Some("eng"));
}

#[test]
fn build_channels_kick_iq_carries_occupant_jid_and_reason() {
    let args = AdminChannelsKickArgs {
        channel_jid: "general@muc.localhost".to_string(),
        occupant_jid: "general@muc.localhost/alice".to_string(),
        reason: Some("spam".to_string()),
    };
    let iq = build_admin_channels_kick_iq("localhost", &args);
    let form = form_of(&iq);
    assert_eq!(
        field_value(form, "occupant_jid").as_deref(),
        Some("general@muc.localhost/alice")
    );
    assert_eq!(field_value(form, "reason").as_deref(), Some("spam"));
}

// ── V1 users result parsing ──────────────────────────────────────────

#[test]
fn parse_users_empty_form_returns_empty_page() {
    let iq = wrap_response(NODE_ADMIN_USERS_LIST, "");
    let page = parse_admin_users_list_result(&iq).expect("ok");
    assert!(page.entries.is_empty());
    assert!(page.next_cursor.is_none());
}

#[test]
fn parse_users_single_entry_with_cursor() {
    let item = r#"<item>
            <field var="jid"><value>alice@localhost</value></field>
            <field var="display_name"><value>Alice</value></field>
            <field var="has_owner_hat"><value>0</value></field>
        </item>
        <field var="next_cursor" type="text-single"><value>alice</value></field>"#;
    let iq = wrap_response(NODE_ADMIN_USERS_LIST, item);
    let page = parse_admin_users_list_result(&iq).expect("ok");
    assert_eq!(page.entries.len(), 1);
    let entry = &page.entries[0];
    assert_eq!(entry.jid, "alice@localhost");
    assert_eq!(entry.display_name.as_deref(), Some("Alice"));
    assert!(!entry.has_owner_hat);
    assert_eq!(page.next_cursor.as_deref(), Some("alice"));
}

#[test]
fn parse_users_owner_flag_recognizes_boolean_variants() {
    for raw in ["1", "true"] {
        let item = format!(
            r#"<item>
                <field var="jid"><value>admin@localhost</value></field>
                <field var="display_name"><value></value></field>
                <field var="has_owner_hat"><value>{raw}</value></field>
            </item>"#
        );
        let iq = wrap_response(NODE_ADMIN_USERS_LIST, &item);
        let page = parse_admin_users_list_result(&iq).expect("ok");
        assert!(page.entries[0].has_owner_hat, "{raw} → owner");
        assert!(page.entries[0].display_name.is_none());
    }
}

#[test]
fn parse_users_missing_command_is_typed_error() {
    let iq: Element = r#"<iq xmlns='jabber:client' type='result' id='x'/>"#
        .parse()
        .expect("parse");
    assert_eq!(
        parse_admin_users_list_result(&iq).unwrap_err(),
        AdminResponseError::MissingCommand
    );
}

// ── V2 result parsing ────────────────────────────────────────────────

#[test]
fn parse_spaces_list_handles_empty_form() {
    let iq = wrap_response(NODE_ADMIN_SPACES_LIST, "");
    let result = parse_admin_spaces_list_result(&iq).expect("ok");
    assert!(result.entries.is_empty());
    assert!(result.next_cursor.is_none());
}

#[test]
fn parse_spaces_list_extracts_counts_and_cursor() {
    let item = r#"<item>
            <field var="space_jid"><value>eng@spaces.localhost</value></field>
            <field var="space_node"><value>eng</value></field>
            <field var="name"><value>Engineering</value></field>
            <field var="description"><value>Hack stuff</value></field>
            <field var="icon_url"><value></value></field>
            <field var="channel_count"><value>3</value></field>
            <field var="member_count"><value>5</value></field>
        </item>"#;
    let cursor = r#"<field var="next_cursor" type="text-single"><value>eng@spaces.localhost</value></field>"#;
    let iq = wrap_response(NODE_ADMIN_SPACES_LIST, &format!("{item}{cursor}"));
    let result = parse_admin_spaces_list_result(&iq).expect("ok");
    assert_eq!(result.entries.len(), 1);
    let entry = &result.entries[0];
    assert_eq!(entry.space_jid, "eng@spaces.localhost");
    assert_eq!(entry.space_node, "eng");
    assert_eq!(entry.name, "Engineering");
    assert_eq!(entry.description.as_deref(), Some("Hack stuff"));
    assert!(entry.icon_url.is_none());
    assert_eq!(entry.channel_count, 3);
    assert_eq!(entry.member_count, 5);
    assert_eq!(result.next_cursor.as_deref(), Some("eng@spaces.localhost"));
}

#[test]
fn parse_space_ref_round_trip() {
    let inner = r#"<field var="space_jid"><value>eng@spaces.localhost</value></field>
            <field var="space_node"><value>eng</value></field>
            <field var="name"><value>Engineering</value></field>
            <field var="description"><value>Hack stuff</value></field>"#;
    let iq = wrap_response(NODE_ADMIN_SPACES_CREATE, inner);
    let space = parse_admin_space_ref_result(&iq).expect("ok");
    assert_eq!(space.space_jid, "eng@spaces.localhost");
    assert_eq!(space.space_node, "eng");
    assert_eq!(space.name, "Engineering");
    assert_eq!(space.description.as_deref(), Some("Hack stuff"));
    assert!(space.icon_url.is_none());
}

#[test]
fn parse_spaces_members_extracts_roles() {
    let inner = r#"<item>
            <field var="jid"><value>alice@localhost</value></field>
            <field var="role"><value>owner</value></field>
        </item>
        <item>
            <field var="jid"><value>bob@localhost</value></field>
            <field var="role"><value>member</value></field>
        </item>"#;
    let iq = wrap_response(NODE_ADMIN_SPACES_MEMBERS, inner);
    let result = parse_admin_spaces_members_result(&iq).expect("ok");
    assert_eq!(result.entries.len(), 2);
    assert_eq!(result.entries[0].role, "owner");
    assert_eq!(result.entries[1].jid, "bob@localhost");
}

#[test]
fn parse_channels_list_extracts_booleans_and_counts() {
    let item = r#"<item>
            <field var="channel_jid"><value>general@muc.localhost</value></field>
            <field var="name"><value>General</value></field>
            <field var="topic"><value>All things</value></field>
            <field var="channel_type"><value>text</value></field>
            <field var="is_public" type="boolean"><value>1</value></field>
            <field var="members_only" type="boolean"><value>0</value></field>
            <field var="occupant_count"><value>7</value></field>
            <field var="owner_count"><value>1</value></field>
            <field var="admin_count"><value>2</value></field>
            <field var="member_count"><value>3</value></field>
            <field var="outcast_count"><value>0</value></field>
        </item>"#;
    let iq = wrap_response(NODE_ADMIN_CHANNELS_LIST, item);
    let result = parse_admin_channels_list_result(&iq).expect("ok");
    assert_eq!(result.entries.len(), 1);
    let entry = &result.entries[0];
    assert_eq!(entry.channel_type, "text");
    assert!(entry.is_public);
    assert!(!entry.members_only);
    assert_eq!(entry.occupant_count, 7);
    assert_eq!(entry.owner_count, 1);
    assert_eq!(entry.admin_count, 2);
    assert_eq!(entry.member_count, 3);
}

#[test]
fn parse_channels_list_rejects_missing_required_fields() {
    let item = r#"<item>
            <field var="channel_jid"><value>general@muc.localhost</value></field>
            <field var="is_public" type="boolean"><value>1</value></field>
            <field var="members_only" type="boolean"><value>0</value></field>
            <field var="occupant_count"><value>7</value></field>
            <field var="owner_count"><value>1</value></field>
            <field var="admin_count"><value>2</value></field>
            <field var="member_count"><value>3</value></field>
            <field var="outcast_count"><value>0</value></field>
        </item>"#;
    let iq = wrap_response(NODE_ADMIN_CHANNELS_LIST, item);
    assert_eq!(
        parse_admin_channels_list_result(&iq).unwrap_err(),
        AdminResponseError::MissingField {
            var: "name".to_string()
        }
    );
}

#[test]
fn parse_channels_list_rejects_malformed_counts_and_booleans() {
    let malformed_count = r#"<item>
            <field var="channel_jid"><value>general@muc.localhost</value></field>
            <field var="name"><value>General</value></field>
            <field var="channel_type"><value>text</value></field>
            <field var="is_public" type="boolean"><value>1</value></field>
            <field var="members_only" type="boolean"><value>0</value></field>
            <field var="occupant_count"><value>many</value></field>
            <field var="owner_count"><value>1</value></field>
            <field var="admin_count"><value>2</value></field>
            <field var="member_count"><value>3</value></field>
            <field var="outcast_count"><value>0</value></field>
        </item>"#;
    let malformed_bool = r#"<item>
            <field var="channel_jid"><value>general@muc.localhost</value></field>
            <field var="name"><value>General</value></field>
            <field var="channel_type"><value>text</value></field>
            <field var="is_public" type="boolean"><value>sometimes</value></field>
            <field var="members_only" type="boolean"><value>0</value></field>
            <field var="occupant_count"><value>7</value></field>
            <field var="owner_count"><value>1</value></field>
            <field var="admin_count"><value>2</value></field>
            <field var="member_count"><value>3</value></field>
            <field var="outcast_count"><value>0</value></field>
        </item>"#;

    assert_eq!(
        parse_admin_channels_list_result(&wrap_response(NODE_ADMIN_CHANNELS_LIST, malformed_count))
            .unwrap_err(),
        AdminResponseError::InvalidCount {
            var: "occupant_count".to_string()
        }
    );
    assert_eq!(
        parse_admin_channels_list_result(&wrap_response(NODE_ADMIN_CHANNELS_LIST, malformed_bool))
            .unwrap_err(),
        AdminResponseError::InvalidBool {
            var: "is_public".to_string()
        }
    );
}

#[test]
fn parse_channels_occupants_extracts_role_and_affiliation() {
    let inner = r#"<item>
            <field var="nick"><value>alice</value></field>
            <field var="real_jid"><value>alice@localhost/web</value></field>
            <field var="role"><value>moderator</value></field>
            <field var="affiliation"><value>owner</value></field>
        </item>"#;
    let iq = wrap_response(NODE_ADMIN_CHANNELS_OCCUPANTS, inner);
    let result = parse_admin_channels_occupants_result(&iq).expect("ok");
    assert_eq!(result.entries.len(), 1);
    let entry = &result.entries[0];
    assert_eq!(entry.nick, "alice");
    assert_eq!(entry.role, "moderator");
    assert_eq!(entry.affiliation, "owner");
}

#[test]
fn parse_channels_affiliations_and_kick_results() {
    let inner = r#"<item>
            <field var="jid"><value>mallory@localhost</value></field>
            <field var="affiliation"><value>outcast</value></field>
            <field var="reason"><value>spam</value></field>
        </item>"#;
    let iq = wrap_response(NODE_ADMIN_CHANNELS_AFFILIATIONS, inner);
    let result = parse_admin_channels_affiliations_result(&iq).expect("ok");
    assert_eq!(result.entries[0].affiliation, "outcast");
    assert_eq!(result.entries[0].reason.as_deref(), Some("spam"));

    let kick = wrap_response(
        NODE_ADMIN_CHANNELS_KICK,
        r#"<field var="occupant_jid"><value>general@muc.localhost/mallory</value></field>"#,
    );
    let kicked = parse_admin_channels_kick_result(&kick).expect("ok");
    assert_eq!(kicked.occupant_jid, "general@muc.localhost/mallory");
}

#[test]
fn parse_set_affiliation_and_set_role_results() {
    let set_affiliation = wrap_response(
        NODE_ADMIN_CHANNELS_SET_AFFILIATION,
        r#"<field var="member_jid"><value>bob@localhost</value></field>
           <field var="affiliation"><value>admin</value></field>"#,
    );
    let result = parse_admin_channels_set_affiliation_result(&set_affiliation).expect("ok");
    assert_eq!(result.member_jid, "bob@localhost");
    assert_eq!(result.affiliation, "admin");

    let set_role = wrap_response(
        NODE_ADMIN_SPACES_SET_ROLE,
        r#"<field var="member_jid"><value>bob@localhost</value></field>
           <field var="role"><value>admin</value></field>"#,
    );
    let result = parse_admin_spaces_set_role_result(&set_role).expect("ok");
    assert_eq!(result.role, "admin");
}
