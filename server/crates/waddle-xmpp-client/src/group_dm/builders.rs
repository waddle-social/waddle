//! Stanza builders for the `urn:waddle:group-dm:*` lifecycle.
//!
//! The three commands assemble a XEP-0004 submit form (hidden
//! `FORM_TYPE` mirroring the command node, per XEP-0068) wrapped in a
//! XEP-0050 §2.4 execute request. The add-member flow is a XEP-0045
//! §7.8.2 mediated invite message, not a command.

use jid::BareJid;
use minidom::Element;
use uuid::Uuid;
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

use crate::discovery::{CLIENT_NS, WADDLE_GROUP_DM_FEATURE_NS};
use crate::xep::xep0050::{build_xep0050_command_request_with_id, AdHocAction};

use super::types::{GroupDmCreateArgs, GroupDmHistoryAccess};
use super::{NODE_GROUP_DM_CREATE, NODE_GROUP_DM_LEAVE, NODE_GROUP_DM_RENAME};

/// XEP-0045 §7.8.2 mediated-invite namespace.
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

fn field(var: &str, field_type: FieldType, values: Vec<String>) -> Field {
    Field {
        var: Some(var.to_string()),
        type_: field_type,
        label: None,
        required: false,
        desc: None,
        options: vec![],
        values,
        media: vec![],
        validate: None,
    }
}

fn text_single(var: &str, value: &str) -> Field {
    field(var, FieldType::TextSingle, vec![value.to_string()])
}

fn jid_single(var: &str, value: &BareJid) -> Field {
    field(var, FieldType::JidSingle, vec![value.to_string()])
}

fn jid_multi(var: &str, values: &[BareJid]) -> Field {
    field(
        var,
        FieldType::JidMulti,
        values.iter().map(BareJid::to_string).collect(),
    )
}

fn command_iq(to: &str, node: &str, iq_id: &str, fields: Vec<Field>) -> Element {
    let form = DataForm::new(DataFormType::Submit, node, fields);
    build_xep0050_command_request_with_id(to, node, iq_id, AdHocAction::Execute, Some(form))
}

/// `urn:waddle:group-dm:create:0` — IQ-set to the bare SERVER domain.
/// `member_jids` must include the creator; the server dedups and
/// requires at least two distinct JIDs.
pub fn build_group_dm_create_iq(
    server_domain: &str,
    args: &GroupDmCreateArgs,
    iq_id: &str,
) -> Element {
    let fields = vec![
        text_single("name", &args.name),
        jid_multi("member_jids", &args.member_jids),
    ];
    command_iq(server_domain, NODE_GROUP_DM_CREATE, iq_id, fields)
}

/// `urn:waddle:group-dm:rename:0` — IQ-set to the ROOM jid. `None`
/// clears the custom name (the server treats an empty `name` value as
/// a clear).
pub fn build_group_dm_rename_iq(room: &BareJid, name: Option<&str>, iq_id: &str) -> Element {
    let fields = vec![
        jid_single("room_jid", room),
        text_single("name", name.unwrap_or("")),
    ];
    command_iq(&room.to_string(), NODE_GROUP_DM_RENAME, iq_id, fields)
}

/// `urn:waddle:group-dm:leave:0` — IQ-set to the bare SERVER domain;
/// the server retracts the leaver's bookmark.
pub fn build_group_dm_leave_iq(server_domain: &str, room: &BareJid, iq_id: &str) -> Element {
    let fields = vec![jid_single("room_jid", room)];
    command_iq(server_domain, NODE_GROUP_DM_LEAVE, iq_id, fields)
}

/// XEP-0045 §7.8.2 mediated invite adding `invitee` to a group DM,
/// optionally extended with the Waddle
/// `<history-access xmlns='urn:waddle:group-dm:0'/>` child. The child
/// is omitted for [`GroupDmHistoryAccess::FromJoin`] — the server
/// applies exactly that default when the extension is absent, and it
/// downgrades any `full` request the inviter is not entitled to make.
pub fn build_group_dm_invite_message(
    room: &BareJid,
    invitee: &BareJid,
    access: GroupDmHistoryAccess,
) -> Element {
    let mut invite = Element::builder("invite", NS_MUC_USER).attr(
        minidom::rxml::xml_ncname!("to").to_owned(),
        invitee.to_string(),
    );
    if access == GroupDmHistoryAccess::Full {
        invite = invite.append(
            Element::builder("history-access", WADDLE_GROUP_DM_FEATURE_NS)
                .attr(
                    minidom::rxml::xml_ncname!("mode").to_owned(),
                    access.as_str(),
                )
                .build(),
        );
    }
    Element::builder("message", CLIENT_NS)
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            room.to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "normal")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            Uuid::new_v4().to_string(),
        )
        .append(
            Element::builder("x", NS_MUC_USER)
                .append(invite.build())
                .build(),
        )
        .build()
}
