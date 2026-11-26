//! XEP-0045 §7.13 voice requests and §8.6 voice approval forms.
//!
//! A visitor asks for voice with a `muc#request` data form sent to the room
//! (§7.13); the room turns it into an approval form for the moderators and a
//! moderator's submitted approval grants voice (§8.6.. "Approving Voice
//! Requests"). This module is the typed wire shape only; the server applies
//! the role change through the ordinary `muc#admin` role path.

use jid::{BareJid, FullJid, Jid};
use xmpp_parsers::message::{Message, MessageType};

use crate::xep::xep0004::{
    is_data_form, DataForm, Field, FieldOption, FieldType, FormType, FromElement, ToElement,
};

/// FORM_TYPE of both the voice request and the voice approval form.
pub const FORM_TYPE_MUC_REQUEST: &str = "http://jabber.org/protocol/muc#request";
/// The role being requested; the only value defined is `participant`.
pub const FIELD_ROLE: &str = "muc#role";
/// The requester's JID in an approval form.
pub const FIELD_JID: &str = "muc#jid";
/// The requester's room nickname in an approval form.
pub const FIELD_ROOMNICK: &str = "muc#roomnick";
/// The moderator's decision in an approval form.
pub const FIELD_REQUEST_ALLOW: &str = "muc#request_allow";
const ROLE_PARTICIPANT: &str = "participant";

/// A `muc#request` form received by the room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceForm {
    /// A visitor's request for voice (§7.13).
    Request,
    /// A moderator's answer to an approval form (§8.6).
    Approval {
        /// The requester's JID, as the approval form carried it.
        jid: Jid,
        /// The requester's room nickname.
        nick: String,
        /// Whether the moderator granted voice.
        allow: bool,
    },
}

/// A `muc#request` form whose content cannot be acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VoiceFormError {
    #[error("the requested role must be participant")]
    UnsupportedRole,
    #[error("the approval form lacks a usable muc#jid, muc#roomnick or muc#request_allow")]
    IncompleteApproval,
}

/// Whether `message` is a voice approval form (§8.6) as a room puts it to a
/// moderator: a normal message carrying a `muc#request` form of type `form`.
pub fn is_voice_approval_form(message: &Message) -> bool {
    matches!(message.type_, MessageType::Normal)
        && message
            .payloads
            .iter()
            .filter(|payload| is_data_form(payload))
            .filter_map(|payload| DataForm::from_element(payload).ok())
            .any(|form| {
                form.form_type == FormType::Form
                    && form.form_type_value() == Some(FORM_TYPE_MUC_REQUEST)
            })
}

/// Classify a message addressed to a room as a voice request or approval.
///
/// `Ok(None)` is any other message. A `muc#request` submit that cannot be
/// acted on is an error, so the sender hears about it.
pub fn parse_voice_form(message: &Message) -> Result<Option<VoiceForm>, VoiceFormError> {
    if !matches!(message.type_, MessageType::Normal) {
        return Ok(None);
    }
    let Some(form) = message
        .payloads
        .iter()
        .filter(|payload| is_data_form(payload))
        .find_map(|payload| DataForm::from_element(payload).ok())
    else {
        return Ok(None);
    };
    if form.form_type != FormType::Submit || form.form_type_value() != Some(FORM_TYPE_MUC_REQUEST) {
        return Ok(None);
    }
    if let Some(role) = form.value(FIELD_ROLE) {
        if role != ROLE_PARTICIPANT {
            return Err(VoiceFormError::UnsupportedRole);
        }
    }
    let Some(allow) = form.field(FIELD_REQUEST_ALLOW) else {
        return if form.field(FIELD_ROLE).is_some() {
            Ok(Some(VoiceForm::Request))
        } else {
            Err(VoiceFormError::UnsupportedRole)
        };
    };
    let allow = match allow.value() {
        Some("1" | "true") => true,
        Some("0" | "false") => false,
        _ => return Err(VoiceFormError::IncompleteApproval),
    };
    let jid = form
        .value(FIELD_JID)
        .and_then(|value| value.trim().parse::<Jid>().ok())
        .ok_or(VoiceFormError::IncompleteApproval)?;
    let nick = form
        .value(FIELD_ROOMNICK)
        .map(str::trim)
        .filter(|nick| !nick.is_empty())
        .ok_or(VoiceFormError::IncompleteApproval)?
        .to_owned();
    Ok(Some(VoiceForm::Approval { jid, nick, allow }))
}

/// The voice approval form the room sends a moderator (§8.6), built from the
/// requester's `muc#request` form.
pub fn build_voice_approval_message(
    room: &BareJid,
    moderator: &FullJid,
    id: &str,
    requester: &FullJid,
    requester_nick: &str,
) -> Message {
    let form = DataForm::new(FormType::Form)
        .with_title("Voice request")
        .add_instructions(
            "To approve this request for voice, select the \"Grant voice to this person?\" \
             checkbox and click OK. To skip this request, click the cancel button.",
        )
        .add_field(Field::form_type(FORM_TYPE_MUC_REQUEST))
        .add_field(
            Field::new(FIELD_ROLE, FieldType::ListSingle)
                .with_label("Requested role")
                .with_value(ROLE_PARTICIPANT)
                .add_option(FieldOption::new(ROLE_PARTICIPANT)),
        )
        .add_field(
            Field::new(FIELD_JID, FieldType::JidSingle)
                .with_label("User ID")
                .with_value(requester.to_string()),
        )
        .add_field(Field::text_single(FIELD_ROOMNICK, requester_nick).with_label("Room Nickname"))
        .add_field(
            Field::boolean(FIELD_REQUEST_ALLOW, false).with_label("Grant voice to this person?"),
        );
    let mut message = Message::new(Some(Jid::from(moderator.clone())));
    message.from = Some(Jid::from(room.clone()));
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message.type_ = MessageType::Normal;
    message.payloads.push(form.to_element());
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit(fields: Vec<Field>) -> Message {
        let mut form =
            DataForm::new(FormType::Submit).add_field(Field::form_type(FORM_TYPE_MUC_REQUEST));
        for field in fields {
            form = form.add_field(field);
        }
        let mut message = Message::new(Some(Jid::from(
            "coven@chat.shakespeare.lit".parse::<BareJid>().unwrap(),
        )));
        message.type_ = MessageType::Normal;
        message.payloads.push(form.to_element());
        message
    }

    #[test]
    fn the_rooms_approval_form_is_recognised_and_a_submit_is_not() {
        let room: BareJid = "coven@chat.shakespeare.lit".parse().unwrap();
        let moderator: FullJid = "crone@shakespeare.lit/desk".parse().unwrap();
        let requester: FullJid = "hag66@shakespeare.lit/pda".parse().unwrap();
        let form = build_voice_approval_message(&room, &moderator, "id-1", &requester, "witch");
        assert!(is_voice_approval_form(&form));
        assert!(!is_voice_approval_form(&submit(vec![Field::boolean(
            FIELD_REQUEST_ALLOW,
            true
        )])));
    }

    #[test]
    fn visitor_request_with_participant_role_is_a_request() {
        let message = submit(vec![
            Field::new(FIELD_ROLE, FieldType::ListSingle).with_value("participant")
        ]);
        assert_eq!(parse_voice_form(&message), Ok(Some(VoiceForm::Request)));
    }

    #[test]
    fn request_for_another_role_is_refused() {
        let message = submit(vec![
            Field::new(FIELD_ROLE, FieldType::ListSingle).with_value("moderator")
        ]);
        assert_eq!(
            parse_voice_form(&message),
            Err(VoiceFormError::UnsupportedRole)
        );
    }

    #[test]
    fn approval_carries_the_requester_and_the_decision() {
        let message = submit(vec![
            Field::new(FIELD_ROLE, FieldType::ListSingle).with_value("participant"),
            Field::new(FIELD_JID, FieldType::JidSingle).with_value("hag66@shakespeare.lit/pda"),
            Field::text_single(FIELD_ROOMNICK, "thirdwitch"),
            Field::boolean(FIELD_REQUEST_ALLOW, true),
        ]);
        assert_eq!(
            parse_voice_form(&message),
            Ok(Some(VoiceForm::Approval {
                jid: "hag66@shakespeare.lit/pda".parse().unwrap(),
                nick: "thirdwitch".to_owned(),
                allow: true,
            }))
        );
    }

    #[test]
    fn approval_without_a_nick_is_incomplete() {
        let message = submit(vec![
            Field::new(FIELD_JID, FieldType::JidSingle).with_value("hag66@shakespeare.lit/pda"),
            Field::boolean(FIELD_REQUEST_ALLOW, true),
        ]);
        assert_eq!(
            parse_voice_form(&message),
            Err(VoiceFormError::IncompleteApproval)
        );
    }

    #[test]
    fn other_forms_and_chat_messages_are_not_voice_forms() {
        let mut other =
            DataForm::new(FormType::Submit).add_field(Field::form_type("urn:example:other"));
        other = other.add_field(Field::text_single("x", "y"));
        let mut message = Message::new(None);
        message.type_ = MessageType::Normal;
        message.payloads.push(other.to_element());
        assert_eq!(parse_voice_form(&message), Ok(None));
        assert_eq!(parse_voice_form(&Message::new(None)), Ok(None));
    }

    #[test]
    fn approval_form_matches_the_specification_example() {
        let room: BareJid = "coven@chat.shakespeare.lit".parse().unwrap();
        let moderator: FullJid = "crone1@shakespeare.lit/pda".parse().unwrap();
        let requester: FullJid = "hag66@shakespeare.lit/pda".parse().unwrap();
        let message =
            build_voice_approval_message(&room, &moderator, "approve", &requester, "thirdwitch");
        assert_eq!(message.from, Some(Jid::from(room)));
        assert_eq!(message.to, Some(Jid::from(moderator)));
        let form = DataForm::from_element(&message.payloads[0]).unwrap();
        assert_eq!(form.form_type, FormType::Form);
        assert_eq!(form.form_type_value(), Some(FORM_TYPE_MUC_REQUEST));
        assert_eq!(form.value(FIELD_ROLE), Some("participant"));
        assert_eq!(form.value(FIELD_JID), Some("hag66@shakespeare.lit/pda"));
        assert_eq!(form.value(FIELD_ROOMNICK), Some("thirdwitch"));
        assert_eq!(form.bool(FIELD_REQUEST_ALLOW), Some(false));
    }
}
