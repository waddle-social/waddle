//! Dedicated test suite for the `urn:waddle:group-dm:*` command
//! surface (XEP-0050 execute requests + XEP-0004 form results) and
//! the XEP-0045 §7.8.2 mediated-invite extension.

use jid::BareJid;
use minidom::Element;

use crate::discovery::WADDLE_GROUP_DM_FEATURE_NS;
use crate::xep::xep0050::{NS_COMMANDS, NS_DATA_FORMS};

use super::*;

const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

fn room() -> BareJid {
    "gdm-abc@muc.localhost".parse().expect("room jid")
}

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

fn field_values(form: &Element, var: &str) -> Vec<String> {
    form.children()
        .filter(|child| child.name() == "field" && child.attr("var") == Some(var))
        .flat_map(|field| {
            field
                .children()
                .filter(|child| child.is("value", NS_DATA_FORMS))
                .map(Element::text)
        })
        .collect()
}

fn field_value(form: &Element, var: &str) -> Option<String> {
    field_values(form, var).into_iter().next()
}

// ── Create request (XEP-0050 §2.4 + XEP-0068 FORM_TYPE) ──────────────

#[test]
fn create_iq_executes_command_on_server_domain_with_mirrored_form_type() {
    let args = GroupDmCreateArgs {
        name: "Alice, Bob".to_string(),
        member_jids: vec![
            "alice@localhost".parse().expect("jid"),
            "bob@localhost".parse().expect("jid"),
        ],
    };
    let iq = build_group_dm_create_iq("localhost", &args, "iq-1");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("localhost"));
    assert_eq!(iq.attr("id"), Some("iq-1"));
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(command.attr("node"), Some(NODE_GROUP_DM_CREATE));
    assert_eq!(command.attr("action"), Some("execute"));
    let form = form_of(&iq);
    assert_eq!(form.attr("type"), Some("submit"));
    // XEP-0068: hidden FORM_TYPE mirrors the node and leads the form.
    assert_eq!(field_vars(form), vec!["FORM_TYPE", "name", "member_jids"]);
    let form_type = form
        .children()
        .find(|child| child.name() == "field" && child.attr("var") == Some("FORM_TYPE"))
        .expect("FORM_TYPE field");
    assert_eq!(form_type.attr("type"), Some("hidden"));
    assert_eq!(
        field_value(form, "FORM_TYPE").as_deref(),
        Some(NODE_GROUP_DM_CREATE)
    );
    assert_eq!(field_value(form, "name").as_deref(), Some("Alice, Bob"));
}

#[test]
fn create_iq_encodes_member_jids_as_jid_multi_values() {
    let args = GroupDmCreateArgs {
        name: "trio".to_string(),
        member_jids: vec![
            "alice@localhost".parse().expect("jid"),
            "bob@localhost".parse().expect("jid"),
            "charlie@localhost".parse().expect("jid"),
        ],
    };
    let iq = build_group_dm_create_iq("localhost", &args, "iq-2");
    let form = form_of(&iq);
    let members = form
        .children()
        .find(|child| child.name() == "field" && child.attr("var") == Some("member_jids"))
        .expect("member_jids field");
    assert_eq!(members.attr("type"), Some("jid-multi"));
    assert_eq!(
        field_values(form, "member_jids"),
        vec!["alice@localhost", "bob@localhost", "charlie@localhost"]
    );
}

// ── Rename / leave requests ──────────────────────────────────────────

#[test]
fn rename_iq_targets_room_jid_and_carries_new_name() {
    let iq = build_group_dm_rename_iq(&room(), Some("Weekend crew"), "iq-3");
    assert_eq!(iq.attr("to"), Some("gdm-abc@muc.localhost"));
    assert_eq!(iq.attr("id"), Some("iq-3"));
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(command.attr("node"), Some(NODE_GROUP_DM_RENAME));
    let form = form_of(&iq);
    assert_eq!(
        field_value(form, "room_jid").as_deref(),
        Some("gdm-abc@muc.localhost")
    );
    assert_eq!(field_value(form, "name").as_deref(), Some("Weekend crew"));
}

#[test]
fn rename_iq_clears_name_with_empty_value() {
    let iq = build_group_dm_rename_iq(&room(), None, "iq-4");
    let form = form_of(&iq);
    // The server treats an empty `name` as a clear.
    assert_eq!(field_value(form, "name").as_deref(), Some(""));
}

#[test]
fn leave_iq_targets_server_domain_and_names_the_room() {
    let iq = build_group_dm_leave_iq("localhost", &room(), "iq-5");
    assert_eq!(iq.attr("to"), Some("localhost"));
    assert_eq!(iq.attr("id"), Some("iq-5"));
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(command.attr("node"), Some(NODE_GROUP_DM_LEAVE));
    let form = form_of(&iq);
    assert_eq!(
        field_value(form, "room_jid").as_deref(),
        Some("gdm-abc@muc.localhost")
    );
}

// ── Result parsing ───────────────────────────────────────────────────

#[test]
fn parse_create_result_extracts_room_jid_and_room_flags() {
    let iq = wrap_response(
        NODE_GROUP_DM_CREATE,
        r#"<field var='room_jid' type='jid-single'><value>gdm-abc@muc.localhost</value></field>
           <field var='name'><value>Alice, Bob</value></field>
           <field var='is_public'><value>0</value></field>
           <field var='members_only'><value>1</value></field>
           <field var='persistent'><value>1</value></field>"#,
    );
    let created = parse_group_dm_create_result(&iq).expect("parses");
    assert_eq!(created.room_jid, room());
    assert_eq!(created.is_public, Some(false));
    assert_eq!(created.members_only, Some(true));
    assert_eq!(created.persistent, Some(true));
}

#[test]
fn parse_create_result_tolerates_absent_room_flags() {
    let iq = wrap_response(
        NODE_GROUP_DM_CREATE,
        r#"<field var='room_jid'><value>gdm-abc@muc.localhost</value></field>"#,
    );
    let created = parse_group_dm_create_result(&iq).expect("parses");
    assert_eq!(created.room_jid, room());
    assert_eq!(created.is_public, None);
    assert_eq!(created.members_only, None);
    assert_eq!(created.persistent, None);
}

#[test]
fn parse_create_result_requires_room_jid() {
    let iq = wrap_response(NODE_GROUP_DM_CREATE, "");
    assert_eq!(
        parse_group_dm_create_result(&iq).unwrap_err(),
        GroupDmResponseError::MissingField {
            var: "room_jid".to_string()
        }
    );
}

#[test]
fn parse_create_result_rejects_invalid_room_jid() {
    let iq = wrap_response(
        NODE_GROUP_DM_CREATE,
        r#"<field var='room_jid'><value>not a jid</value></field>"#,
    );
    assert_eq!(
        parse_group_dm_create_result(&iq).unwrap_err(),
        GroupDmResponseError::InvalidJid {
            var: "room_jid".to_string()
        }
    );
}

#[test]
fn parse_create_result_rejects_non_command_iq() {
    let iq: Element = "<iq xmlns='jabber:client' type='result' id='t'/>"
        .parse()
        .expect("iq");
    assert_eq!(
        parse_group_dm_create_result(&iq).unwrap_err(),
        GroupDmResponseError::MissingCommand
    );
}

#[test]
fn parse_create_result_rejects_non_completed_status() {
    let raw = format!(
        r#"<iq xmlns='jabber:client' type='result' id='t'><command xmlns='http://jabber.org/protocol/commands' node='{NODE_GROUP_DM_CREATE}' status='executing'/></iq>"#
    );
    let iq: Element = raw.parse().expect("iq");
    assert_eq!(
        parse_group_dm_create_result(&iq).unwrap_err(),
        GroupDmResponseError::NotCompleted
    );
}

#[test]
fn parse_rename_result_reads_new_name() {
    let iq = wrap_response(
        NODE_GROUP_DM_RENAME,
        r#"<field var='room_jid'><value>gdm-abc@muc.localhost</value></field>
           <field var='name'><value>Weekend crew</value></field>"#,
    );
    let renamed = parse_group_dm_rename_result(&iq).expect("parses");
    assert_eq!(renamed.room_jid, room());
    assert_eq!(renamed.name.as_deref(), Some("Weekend crew"));
}

#[test]
fn parse_rename_result_maps_empty_echoed_name_to_cleared() {
    let iq = wrap_response(
        NODE_GROUP_DM_RENAME,
        r#"<field var='room_jid'><value>gdm-abc@muc.localhost</value></field>
           <field var='name'><value></value></field>"#,
    );
    let renamed = parse_group_dm_rename_result(&iq).expect("parses");
    assert_eq!(renamed.name, None);
}

#[test]
fn parse_leave_result_reads_left_flag() {
    let iq = wrap_response(
        NODE_GROUP_DM_LEAVE,
        r#"<field var='room_jid'><value>gdm-abc@muc.localhost</value></field>
           <field var='left'><value>1</value></field>"#,
    );
    let left = parse_group_dm_leave_result(&iq).expect("parses");
    assert_eq!(left.room_jid, room());
    assert!(left.left);
}

// ── Mediated invite (XEP-0045 §7.8.2 + history-access) ───────────────

fn invite_of(message: &Element) -> &Element {
    message
        .get_child("x", NS_MUC_USER)
        .and_then(|x| x.get_child("invite", NS_MUC_USER))
        .expect("mediated invite")
}

#[test]
fn invite_message_is_a_normal_message_to_the_room() {
    let invitee: BareJid = "charlie@localhost".parse().expect("jid");
    let message = build_group_dm_invite_message(&room(), &invitee, GroupDmHistoryAccess::FromJoin);
    assert_eq!(message.name(), "message");
    assert_eq!(message.attr("type"), Some("normal"));
    assert_eq!(message.attr("to"), Some("gdm-abc@muc.localhost"));
    assert!(!message.attr("id").unwrap_or("").is_empty());
    assert_eq!(invite_of(&message).attr("to"), Some("charlie@localhost"));
}

#[test]
fn invite_message_omits_history_access_for_from_join_default() {
    // The server treats an absent history-access child as from-join;
    // mirroring the default keeps the wire minimal.
    let invitee: BareJid = "charlie@localhost".parse().expect("jid");
    let message = build_group_dm_invite_message(&room(), &invitee, GroupDmHistoryAccess::FromJoin);
    assert!(invite_of(&message)
        .get_child("history-access", WADDLE_GROUP_DM_FEATURE_NS)
        .is_none());
}

#[test]
fn invite_message_carries_full_history_access_mode() {
    let invitee: BareJid = "charlie@localhost".parse().expect("jid");
    let message = build_group_dm_invite_message(&room(), &invitee, GroupDmHistoryAccess::Full);
    let access = invite_of(&message)
        .get_child("history-access", WADDLE_GROUP_DM_FEATURE_NS)
        .expect("history-access child");
    assert_eq!(access.attr("mode"), Some("full"));
}

#[test]
fn history_access_wire_strings_match_the_server_domain() {
    assert_eq!(GroupDmHistoryAccess::FromJoin.as_str(), "from-join");
    assert_eq!(GroupDmHistoryAccess::Full.as_str(), "full");
}
