//! MUC Owner Operations (XEP-0045 §10.1)
//!
//! Implements the owner-config form rendering used by the GET path
//! (`<iq type='get'><query xmlns='http://jabber.org/protocol/muc#owner'/></iq>`).
//!
//! The SET path is parsed inline by the websocket IQ interpreter
//! (`server/.../handlers/iq/muc_owner_config.rs`) and writes through
//! to the `RoomActor` via `UpdateConfig` directly, so this module no
//! longer carries a parser.

use minidom::Element;

use super::pin::PinPermission;
use super::room::AllowPm;
use super::MucRoom;
#[cfg(test)]
use super::RoomConfig;

/// XEP-0045 owner-config form field var for the per-room pin
/// permission policy (#415).
pub const FIELD_PIN_PERMISSION: &str = "urn:waddle:roomconfig:pinpermission";

use crate::xep::xep0004::{self, DataForm, Field, FieldOption, FieldType, FormType, ToElement};
use crate::xep::FIELD_FORUM_MODE;

/// Namespace for XEP-0004 Data Forms (re-exported for backward compatibility).
pub const DATA_FORMS_NS: &str = xep0004::NS_DATA_FORMS;

/// Namespace for MUC roomconfig form type.
pub const MUC_ROOMCONFIG_NS: &str = "http://jabber.org/protocol/muc#roomconfig";

/// Build a room configuration form (XEP-0004) for GET requests.
///
/// Creates a data form with the current room settings for the owner to modify.
///
/// `muc#roomconfig_persistentroom` is deliberately NOT offered
/// (#1265 item 7): configuring a room through this form persists it
/// into the channel catalog, so every configured room is persistent by
/// construction. Offering a knob the service force-overwrites would be
/// untruthful; per XEP-0045 §10.1.2 a service only offers the fields
/// it supports.
pub fn build_config_form(room: &MucRoom) -> Element {
    DataForm::new(FormType::Form)
        .add_field(Field::form_type(MUC_ROOMCONFIG_NS))
        .add_field(
            Field::text_single("muc#roomconfig_roomname", &room.config.name)
                .with_label("Room Name"),
        )
        .add_field(
            Field::text_single(
                "muc#roomconfig_roomdesc",
                room.config.description.as_deref().unwrap_or(""),
            )
            .with_label("Room Description"),
        )
        .add_field(
            Field::boolean("muc#roomconfig_membersonly", room.config.members_only)
                .with_label("Make Room Members-Only"),
        )
        .add_field(
            Field::boolean("muc#roomconfig_publicroom", room.config.public_room)
                .with_label("List Room in Directory"),
        )
        .add_field(
            Field::boolean("muc#roomconfig_moderatedroom", room.config.moderated)
                .with_label("Make Room Moderated"),
        )
        .add_field(
            Field::text_single(
                "muc#roomconfig_maxusers",
                room.config.max_occupants.to_string(),
            )
            .with_label("Maximum Number of Occupants"),
        )
        .add_field(
            Field::boolean("muc#roomconfig_enablelogging", room.config.enable_logging)
                .with_label("Enable Room Logging"),
        )
        .add_field(
            Field::boolean(
                "muc#roomconfig_changesubject",
                room.config.occupants_may_change_subject,
            )
            .with_label("Allow Occupants to Change Subject"),
        )
        .add_field(Field::boolean(FIELD_FORUM_MODE, room.config.forum).with_label("Forum Mode"))
        .add_field(
            Field::new("muc#roomconfig_allowpm", FieldType::ListSingle)
                .with_label("Roles that May Send Private Messages")
                .with_value(room.config.allow_pm.as_form_value())
                .add_option(FieldOption::with_label(
                    "Anyone",
                    AllowPm::Anyone.as_form_value(),
                ))
                .add_option(FieldOption::with_label(
                    "Anyone with Voice",
                    AllowPm::Participants.as_form_value(),
                ))
                .add_option(FieldOption::with_label(
                    "Moderators Only",
                    AllowPm::Moderators.as_form_value(),
                ))
                .add_option(FieldOption::with_label(
                    "Nobody",
                    AllowPm::None.as_form_value(),
                )),
        )
        .add_field(
            Field::new(FIELD_PIN_PERMISSION, FieldType::ListSingle)
                .with_label("Who can pin messages")
                .with_value(room.config.pin_permission.as_form_value())
                .add_option(FieldOption::with_label(
                    "Admins only",
                    PinPermission::AdminsOnly.as_form_value(),
                ))
                .add_option(FieldOption::with_label(
                    "Anyone",
                    PinPermission::Anyone.as_form_value(),
                )),
        )
        .to_element()
}

#[cfg(test)]
mod tests;
