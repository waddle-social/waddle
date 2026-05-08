//! MUC Owner Operations (XEP-0045 §10.1-10.2)
//!
//! Implements IQ-based owner operations for Multi-User Chat rooms:
//! - Getting room configuration (§10.1)
//! - Setting room configuration (§10.2)
//! - Destroying rooms (§10.9)
//!
//! ## Namespaces
//! - `http://jabber.org/protocol/muc#owner` - Owner operations
//! - `jabber:x:data` - Data forms (XEP-0004)

use jid::{BareJid, Jid};
use minidom::Element;
use tracing::{debug, instrument};
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::presence::Presence;

use super::{MucRoom, RoomConfig, NS_MUC_OWNER};
use crate::xep::xep0004::{self, DataForm, Field, FormType, FromElement, IntoElement};
use crate::xep::FIELD_FORUM_MODE;
use crate::XmppError;

/// Namespace for XEP-0004 Data Forms (re-exported for backward compatibility).
pub const DATA_FORMS_NS: &str = xep0004::NS_DATA_FORMS;

/// Namespace for MUC roomconfig form type.
pub const MUC_ROOMCONFIG_NS: &str = "http://jabber.org/protocol/muc#roomconfig";

/// Parsed owner query request.
#[derive(Debug)]
pub struct OwnerQuery {
    /// The room JID being configured
    pub room_jid: BareJid,
    /// The IQ ID for response correlation
    pub iq_id: String,
    /// The sender's JID
    pub from: Jid,
    /// The action to perform
    pub action: OwnerAction,
}

/// Type of owner action requested.
#[derive(Debug)]
pub enum OwnerAction {
    /// Get room configuration form
    GetConfig,
    /// Set room configuration from submitted form
    SetConfig(ConfigFormData),
    /// Destroy the room
    Destroy(DestroyRequest),
}

/// Parsed room configuration form data.
#[derive(Debug, Default, Clone)]
pub struct ConfigFormData {
    /// Room name (muc#roomconfig_roomname)
    pub name: Option<String>,
    /// Room description (muc#roomconfig_roomdesc)
    pub description: Option<String>,
    /// Whether room is persistent (muc#roomconfig_persistentroom)
    pub persistent: Option<bool>,
    /// Whether room is members-only (muc#roomconfig_membersonly)
    pub members_only: Option<bool>,
    /// Whether room is moderated (muc#roomconfig_moderatedroom)
    pub moderated: Option<bool>,
    /// Maximum occupants (muc#roomconfig_maxusers)
    pub max_occupants: Option<u32>,
    /// Whether to enable logging (muc#roomconfig_enablelogging)
    pub enable_logging: Option<bool>,
    /// Whether forum mode is enabled (muc#roomconfig_forum)
    pub forum: Option<bool>,
}

/// Room destruction request.
#[derive(Debug, Default, Clone)]
pub struct DestroyRequest {
    /// Optional reason for destruction
    pub reason: Option<String>,
    /// Optional alternate venue JID
    pub alternate_venue: Option<BareJid>,
    /// Optional password for alternate venue
    pub password: Option<String>,
}

/// Parse a MUC owner IQ request.
///
/// Handles:
/// - GET requests: Return room configuration form
/// - SET requests with data form: Update room configuration
/// - SET requests with destroy element: Destroy the room
#[instrument(skip(iq), fields(iq_id = %iq.id))]
pub fn parse_owner_query(iq: &Iq, muc_domain: &str) -> Result<OwnerQuery, XmppError> {
    // Get the room JID from the 'to' attribute
    let room_jid = iq
        .to
        .as_ref()
        .ok_or_else(|| XmppError::bad_request(Some("Missing 'to' attribute".into())))?
        .to_bare();

    // Verify it's a MUC room JID
    if room_jid.domain().as_str() != muc_domain {
        return Err(XmppError::bad_request(Some(format!(
            "IQ to {} is not a MUC room",
            room_jid
        ))));
    }

    // Get the sender's JID
    let from = iq
        .from
        .clone()
        .ok_or_else(|| XmppError::bad_request(Some("Missing 'from' attribute".into())))?;

    // Determine the action based on IQ type and contents
    let action = match &iq.payload {
        IqType::Get(_) => {
            debug!(room = %room_jid, "Parsed owner config GET request");
            OwnerAction::GetConfig
        }
        IqType::Set(query_elem) => {
            // Check for destroy element first
            if let Some(destroy) = query_elem.get_child("destroy", NS_MUC_OWNER) {
                let request = parse_destroy_element(destroy)?;
                debug!(room = %room_jid, reason = ?request.reason, "Parsed owner destroy request");
                OwnerAction::Destroy(request)
            }
            // Check for data form
            else if let Some(form) = query_elem.get_child("x", DATA_FORMS_NS) {
                let config = parse_config_form(form)?;
                debug!(room = %room_jid, "Parsed owner config SET request");
                OwnerAction::SetConfig(config)
            }
            // Empty SET is a cancel (just return success)
            else {
                debug!(room = %room_jid, "Parsed owner empty SET (cancel)");
                OwnerAction::SetConfig(ConfigFormData::default())
            }
        }
        _ => {
            return Err(XmppError::bad_request(Some(
                "Expected get or set IQ".into(),
            )));
        }
    };

    Ok(OwnerQuery {
        room_jid,
        iq_id: iq.id.clone(),
        from,
        action,
    })
}

/// Parse the destroy element from a room destruction request.
fn parse_destroy_element(destroy: &Element) -> Result<DestroyRequest, XmppError> {
    let mut request = DestroyRequest::default();

    // Parse alternate venue JID from 'jid' attribute
    if let Some(jid_str) = destroy.attr("jid") {
        request.alternate_venue = jid_str.parse().ok();
    }

    // Parse child elements
    for child in destroy.children() {
        match child.name() {
            "reason" => {
                let text = child.text();
                if !text.is_empty() {
                    request.reason = Some(text);
                }
            }
            "password" => {
                let text = child.text();
                if !text.is_empty() {
                    request.password = Some(text);
                }
            }
            _ => {} // Ignore unknown elements
        }
    }

    Ok(request)
}

/// Parse a room configuration data form (XEP-0004).
fn parse_config_form(form_elem: &Element) -> Result<ConfigFormData, XmppError> {
    let form = DataForm::from_element(form_elem)
        .map_err(|e| XmppError::bad_request(Some(format!("Invalid data form: {}", e))))?;

    let mut config = ConfigFormData::default();

    for field in &form.fields {
        let var = match field.var.as_deref() {
            Some(v) => v,
            None => continue,
        };

        match var {
            "muc#roomconfig_roomname" => {
                config.name = field
                    .value()
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string());
            }
            "muc#roomconfig_roomdesc" => {
                config.description = field
                    .value()
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string());
            }
            "muc#roomconfig_persistentroom" => {
                config.persistent = field.value_as_bool();
            }
            "muc#roomconfig_membersonly" => {
                config.members_only = field.value_as_bool();
            }
            "muc#roomconfig_moderatedroom" => {
                config.moderated = field.value_as_bool();
            }
            "muc#roomconfig_maxusers" => {
                config.max_occupants = field.value().and_then(|v| v.parse().ok());
            }
            "muc#roomconfig_enablelogging" => {
                config.enable_logging = field.value_as_bool();
            }
            FIELD_FORUM_MODE => {
                config.forum = field.value_as_bool();
            }
            "FORM_TYPE" => {
                // Ignore the FORM_TYPE field
            }
            _ => {
                debug!(field = var, "Ignoring unknown room config field");
            }
        }
    }

    Ok(config)
}

/// Build a room configuration form (XEP-0004) for GET requests.
///
/// Creates a data form with the current room settings for the owner to modify.
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
            Field::boolean("muc#roomconfig_persistentroom", room.config.persistent)
                .with_label("Make Room Persistent"),
        )
        .add_field(
            Field::boolean("muc#roomconfig_membersonly", room.config.members_only)
                .with_label("Make Room Members-Only"),
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
        .add_field(Field::boolean(FIELD_FORUM_MODE, room.config.forum).with_label("Forum Mode"))
        .into_element()
}

/// Build a hidden field for data forms (test helper).
#[cfg(test)]
fn build_field_hidden(var: &str, value: &str) -> Element {
    Field::hidden(var, value).into_element()
}

/// Build a text-single field for data forms (test helper).
#[cfg(test)]
fn build_field_text_single(var: &str, label: &str, value: &str) -> Element {
    Field::text_single(var, value)
        .with_label(label)
        .into_element()
}

/// Build a boolean field for data forms (test helper).
#[cfg(test)]
fn build_field_boolean(var: &str, label: &str, value: bool) -> Element {
    Field::boolean(var, value).with_label(label).into_element()
}

/// Build an owner query result response with the config form.
///
/// Creates an IQ result containing the room configuration form.
pub fn build_config_result(
    iq_id: &str,
    from_room_jid: &BareJid,
    to_jid: &Jid,
    config_form: Element,
) -> Iq {
    let query = Element::builder("query", NS_MUC_OWNER)
        .append(config_form)
        .build();

    Iq {
        from: Some(Jid::from(from_room_jid.clone())),
        to: Some(to_jid.clone()),
        id: iq_id.to_string(),
        payload: IqType::Result(Some(query)),
    }
}

/// Build an empty owner set result (success).
///
/// Used when room configuration is successfully updated.
pub fn build_owner_set_result(iq_id: &str, from_room_jid: &BareJid, to_jid: &Jid) -> Iq {
    Iq {
        from: Some(Jid::from(from_room_jid.clone())),
        to: Some(to_jid.clone()),
        id: iq_id.to_string(),
        payload: IqType::Result(None),
    }
}

/// Build a room destruction notification presence.
///
/// Per XEP-0045 §10.9, when a room is destroyed, all occupants receive
/// an unavailable presence with a <destroy/> element containing:
/// - Optional alternate venue JID
/// - Optional reason for destruction
pub fn build_destroy_notification(
    room_jid: &BareJid,
    occupant_nick: &str,
    occupant_jid: &jid::FullJid,
    destroy_request: &DestroyRequest,
    is_self: bool,
) -> Presence {
    // Build the room JID with occupant's nick
    let from_room_jid = room_jid
        .with_resource_str(occupant_nick)
        .unwrap_or_else(|_| {
            room_jid
                .with_resource_str("unknown")
                .expect("literal 'unknown' is always a valid resource")
        });

    let mut presence = Presence::new(xmpp_parsers::presence::Type::Unavailable);
    presence.from = Some(Jid::from(from_room_jid));
    presence.to = Some(Jid::from(occupant_jid.clone()));

    // Build the MUC user element with destroy child
    let mut destroy_elem = Element::builder("destroy", "http://jabber.org/protocol/muc#user");

    // Add alternate venue if present
    if let Some(ref venue) = destroy_request.alternate_venue {
        destroy_elem = destroy_elem.attr("jid", venue.to_string());
    }

    // Add reason if present
    if let Some(ref reason) = destroy_request.reason {
        destroy_elem = destroy_elem.append(
            Element::builder("reason", "http://jabber.org/protocol/muc#user")
                .append(reason.as_str())
                .build(),
        );
    }

    // Add password if present (for alternate venue)
    if let Some(ref password) = destroy_request.password {
        destroy_elem = destroy_elem.append(
            Element::builder("password", "http://jabber.org/protocol/muc#user")
                .append(password.as_str())
                .build(),
        );
    }

    // Build the x element
    let mut x_elem = Element::builder("x", "http://jabber.org/protocol/muc#user")
        .append(
            Element::builder("item", "http://jabber.org/protocol/muc#user")
                .attr("affiliation", "none")
                .attr("role", "none")
                .build(),
        )
        .append(destroy_elem.build());

    // Add self-presence status code if this is for the occupant themselves
    if is_self {
        x_elem = x_elem.append(
            Element::builder("status", "http://jabber.org/protocol/muc#user")
                .attr("code", "110")
                .build(),
        );
    }

    presence.payloads.push(x_elem.build());

    presence
}

/// Apply configuration form data to a room config.
///
/// Only updates fields that are present in the form data.
pub fn apply_config_form(config: &mut RoomConfig, form_data: &ConfigFormData) {
    if let Some(ref name) = form_data.name {
        config.name = name.clone();
    }
    if let Some(ref desc) = form_data.description {
        config.description = Some(desc.clone());
    } else if form_data.description.is_none() {
        // Don't clear description unless explicitly set to empty
    }
    if let Some(persistent) = form_data.persistent {
        config.persistent = persistent;
    }
    if let Some(members_only) = form_data.members_only {
        config.members_only = members_only;
    }
    if let Some(moderated) = form_data.moderated {
        config.moderated = moderated;
    }
    if let Some(max_occupants) = form_data.max_occupants {
        config.max_occupants = max_occupants;
    }
    if let Some(enable_logging) = form_data.enable_logging {
        config.enable_logging = enable_logging;
    }
    if let Some(forum) = form_data.forum {
        config.forum = forum;
    }
}

#[cfg(test)]
mod tests;
