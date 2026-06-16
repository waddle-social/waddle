use super::*;

fn wrap_response(node: &str, form_inner: &str) -> Element {
    let raw = format!(
        r#"<iq xmlns='jabber:client' type='result' id='test'><command xmlns='http://jabber.org/protocol/commands' node='{node}' status='completed'><x xmlns='jabber:x:data' type='result'><field type="hidden" var="FORM_TYPE"><value>{node}</value></field>{form_inner}</x></command></iq>"#
    );
    raw.parse().expect("parse iq")
}

#[test]
fn parse_spaces_list_handles_empty_form() {
    let iq = wrap_response(NS_ADMIN_SPACES_LIST, "");
    let result = parse_spaces_list_result(&iq).expect("ok");
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
    let iq = wrap_response(NS_ADMIN_SPACES_LIST, &format!("{item}{cursor}"));
    let result = parse_spaces_list_result(&iq).expect("ok");
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
    let iq = wrap_response(NS_ADMIN_SPACES_CREATE, inner);
    let space = parse_space_ref_result(&iq).expect("ok");
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
    let iq = wrap_response(NS_ADMIN_SPACES_MEMBERS, inner);
    let result = parse_spaces_members_result(&iq).expect("ok");
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
            <field var="is_public" type="boolean"><value>1</value></field>
            <field var="members_only" type="boolean"><value>0</value></field>
            <field var="occupant_count"><value>7</value></field>
            <field var="owner_count"><value>1</value></field>
            <field var="admin_count"><value>2</value></field>
            <field var="member_count"><value>3</value></field>
            <field var="outcast_count"><value>0</value></field>
        </item>"#;
    let iq = wrap_response(NS_ADMIN_CHANNELS_LIST, item);
    let result = parse_channels_list_result(&iq).expect("ok");
    assert_eq!(result.entries.len(), 1);
    let entry = &result.entries[0];
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
    let iq = wrap_response(NS_ADMIN_CHANNELS_LIST, item);
    let err = parse_channels_list_result(&iq).unwrap_err();
    assert!(err.contains("missing required field `name`"));
}

#[test]
fn parse_channels_list_rejects_malformed_counts_and_booleans() {
    let malformed_count = r#"<item>
            <field var="channel_jid"><value>general@muc.localhost</value></field>
            <field var="name"><value>General</value></field>
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
            <field var="is_public" type="boolean"><value>sometimes</value></field>
            <field var="members_only" type="boolean"><value>0</value></field>
            <field var="occupant_count"><value>7</value></field>
            <field var="owner_count"><value>1</value></field>
            <field var="admin_count"><value>2</value></field>
            <field var="member_count"><value>3</value></field>
            <field var="outcast_count"><value>0</value></field>
        </item>"#;

    let count_err =
        parse_channels_list_result(&wrap_response(NS_ADMIN_CHANNELS_LIST, malformed_count))
            .unwrap_err();
    assert!(count_err.contains("field `occupant_count` is not a u32"));

    let bool_err =
        parse_channels_list_result(&wrap_response(NS_ADMIN_CHANNELS_LIST, malformed_bool))
            .unwrap_err();
    assert!(bool_err.contains("field `is_public` is not a boolean"));
}

#[test]
fn parse_channels_occupants_extracts_role_and_affiliation() {
    let inner = r#"<item>
            <field var="nick"><value>alice</value></field>
            <field var="real_jid"><value>alice@localhost/web</value></field>
            <field var="role"><value>moderator</value></field>
            <field var="affiliation"><value>owner</value></field>
        </item>"#;
    let iq = wrap_response(NS_ADMIN_CHANNELS_OCCUPANTS, inner);
    let result = parse_channels_occupants_result(&iq).expect("ok");
    assert_eq!(result.entries.len(), 1);
    let entry = &result.entries[0];
    assert_eq!(entry.nick, "alice");
    assert_eq!(entry.role, "moderator");
    assert_eq!(entry.affiliation, "owner");
}

#[test]
fn build_spaces_list_iq_omits_unset_fields() {
    let args = WaddleAdminSpacesListArgs::default();
    let iq = build_spaces_list_iq("localhost", &args);
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("to"), Some("localhost"));
    let cmd = iq.get_child("command", NS_ADHOC_COMMANDS).expect("command");
    assert_eq!(cmd.attr("node"), Some(NS_ADMIN_SPACES_LIST));
    let form = cmd.get_child("x", NS_XDATA).expect("form");
    let var_names: Vec<&str> = form
        .children()
        .filter(|c| c.name() == "field")
        .filter_map(|f| f.attr("var"))
        .collect();
    assert_eq!(var_names, vec!["FORM_TYPE"]);
}

#[test]
fn build_channels_create_iq_includes_all_optional_fields() {
    let args = WaddleAdminChannelsCreateArgs {
        name: "general".to_string(),
        topic: Some("All things".to_string()),
        space_jid: Some("eng@spaces.localhost".to_string()),
        space_node: Some("eng".to_string()),
        is_public: Some(true),
        members_only: Some(true),
    };
    let iq = build_channels_create_iq("localhost", &args);
    let form = iq
        .get_child("command", NS_ADHOC_COMMANDS)
        .and_then(|c| c.get_child("x", NS_XDATA))
        .expect("form");
    let var_names: Vec<&str> = form
        .children()
        .filter(|c| c.name() == "field")
        .filter_map(|f| f.attr("var"))
        .collect();
    assert!(var_names.contains(&"name"));
    assert!(var_names.contains(&"topic"));
    assert!(var_names.contains(&"space_jid"));
    assert!(var_names.contains(&"space_node"));
    assert!(var_names.contains(&"is_public"));
    assert!(var_names.contains(&"members_only"));
}

#[test]
fn build_spaces_delete_iq_carries_confirm() {
    let args = WaddleAdminSpacesDeleteArgs {
        space_jid: "eng@spaces.localhost".to_string(),
        space_node: Some("eng".to_string()),
        confirm: "yes".to_string(),
    };
    let iq = build_spaces_delete_iq("localhost", &args);
    let form = iq
        .get_child("command", NS_ADHOC_COMMANDS)
        .and_then(|c| c.get_child("x", NS_XDATA))
        .expect("form");
    let confirm_value = form
        .children()
        .filter(|c| c.name() == "field" && c.attr("var") == Some("confirm"))
        .find_map(|f| f.get_child("value", NS_XDATA).map(|v| v.text()))
        .expect("confirm field");
    assert_eq!(confirm_value, "yes");
    let node_value = form
        .children()
        .filter(|c| c.name() == "field" && c.attr("var") == Some("space_node"))
        .find_map(|f| f.get_child("value", NS_XDATA).map(|v| v.text()))
        .expect("space_node field");
    assert_eq!(node_value, "eng");
}
