//! XEP-0045 §10 Owner Use Cases — room configuration and destruction.
//!
//! Client-side builders and parsers for the `muc#owner` IQ surface:
//!
//! - §10.2 "Subsequent Room Configuration": IQ-get of the config form,
//!   typed parse of the returned XEP-0004 form, patch-merge, and the
//!   IQ-set submit carrying the full merged form back. The same
//!   GET-merge-SET sequence doubles as the §10.1.3 reserved-room
//!   creation flow when run right after the creating join.
//! - §10.9 "Destroying a Room": IQ-set with `<destroy/>`.
//!
//! The web client drives this flow with raw XML over the wasm
//! `send_raw_iq` escape hatch; this module is the typed Rust
//! equivalent shared by the FFI (Android/Apple) surface.

use jid::BareJid;
use minidom::Element;

use crate::discovery::ids::next_id;
use crate::discovery::CLIENT_NS;

use super::xep0050::NS_DATA_FORMS;

/// XEP-0045 §10 owner IQ namespace.
pub const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
/// XEP-0045 registrar FORM_TYPE of the room configuration form
/// (xep-0045.xml examples 164/173).
pub const NS_MUC_ROOMCONFIG: &str = "http://jabber.org/protocol/muc#roomconfig";

/// XEP-0004 §3.2 hidden form-type discriminator field.
pub const FIELD_FORM_TYPE: &str = "FORM_TYPE";
/// XEP-0045 registrar: natural-language room name.
pub const FIELD_ROOM_NAME: &str = "muc#roomconfig_roomname";
/// XEP-0045 registrar: short description.
pub const FIELD_ROOM_DESC: &str = "muc#roomconfig_roomdesc";
/// XEP-0045 registrar: members-only room flag.
pub const FIELD_MEMBERS_ONLY: &str = "muc#roomconfig_membersonly";
/// XEP-0045 registrar: publicly searchable room flag.
pub const FIELD_PUBLIC_ROOM: &str = "muc#roomconfig_publicroom";
/// XEP-0045 registrar: moderated room flag.
pub const FIELD_MODERATED_ROOM: &str = "muc#roomconfig_moderatedroom";
/// Waddle forum-mode flag inside the official roomconfig form.
/// Pre-existing namespace debt: the field squats the `muc#roomconfig_`
/// registrar prefix for a Waddle-specific semantic (the server reads
/// it in `muc_owner_config.rs`). Carried verbatim for parity — do not
/// extend the pattern to new fields.
pub const FIELD_FORUM_MODE: &str = "muc#roomconfig_forum";
/// Waddle-private pin-permission policy field (correct custom-field
/// pattern: custom `urn:waddle:*` var inside the official form).
pub const FIELD_PIN_PERMISSION: &str = "urn:waddle:roomconfig:pinpermission";

/// `urn:waddle:roomconfig:pinpermission` values. Wire values mirror
/// `waddle_xmpp::muc::PinPermission` server-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomPinPermission {
    AdminsOnly,
    Anyone,
}

impl RoomPinPermission {
    pub fn as_form_value(self) -> &'static str {
        match self {
            Self::AdminsOnly => "admins-only",
            Self::Anyone => "anyone",
        }
    }

    pub fn from_form_value(value: &str) -> Option<Self> {
        match value {
            "admins-only" => Some(Self::AdminsOnly),
            "anyone" => Some(Self::Anyone),
            _ => None,
        }
    }
}

/// One `<field/>` of the owner configuration form: the registrar var
/// plus the field's own current `<value/>` children. `<option/>`
/// values are deliberately NOT captured — a `list-single` field
/// carries its current value as a direct `<value/>` child and each
/// selectable option as `<option><value/></option>`, and merging the
/// option values into the submit would corrupt the round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomConfigField {
    pub var: String,
    pub values: Vec<String>,
}

/// Typed projection of a `muc#roomconfig` XEP-0004 form, preserving
/// every field the service offered so a §10.2 submit round-trips
/// unmodified settings losslessly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomConfigForm {
    pub fields: Vec<RoomConfigField>,
}

impl RoomConfigForm {
    /// First value of `var`, if the field is present and non-empty.
    pub fn value(&self, var: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.var == var)
            .and_then(|field| field.values.first())
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// XEP-0004 boolean interpretation of `var`.
    pub fn bool_value(&self, var: &str) -> Option<bool> {
        match self.value(var)? {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        }
    }

    /// Replace the value of `var`, appending the field when the
    /// service did not offer it (custom Waddle fields are accepted by
    /// the server even when unoffered).
    pub fn set_value(&mut self, var: &str, value: String) {
        if let Some(field) = self.fields.iter_mut().find(|field| field.var == var) {
            field.values = vec![value];
        } else {
            self.fields.push(RoomConfigField {
                var: var.to_string(),
                values: vec![value],
            });
        }
    }
}

/// Owner-editable settings patch applied over a fetched
/// [`RoomConfigForm`] before submit. `None` keeps the fetched value.
/// `description: Some("")` clears the description (the server treats
/// a present-but-empty roomdesc as authoritative).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomConfigPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub forum: Option<bool>,
    pub pin_permission: Option<RoomPinPermission>,
}

/// Merge an edit patch into a fetched form (§10.2 step 2 of
/// GET-merge-SET). Untouched fields keep the values the service
/// reported, so submitting the merged form never resets settings the
/// caller did not edit.
pub fn apply_room_config_patch(form: &mut RoomConfigForm, patch: &RoomConfigPatch) {
    if let Some(name) = patch.name.as_ref() {
        form.set_value(FIELD_ROOM_NAME, name.clone());
    }
    if let Some(description) = patch.description.as_ref() {
        form.set_value(FIELD_ROOM_DESC, description.clone());
    }
    if let Some(forum) = patch.forum {
        form.set_value(FIELD_FORUM_MODE, bool_form_value(forum).to_string());
    }
    if let Some(pin_permission) = patch.pin_permission {
        form.set_value(
            FIELD_PIN_PERMISSION,
            pin_permission.as_form_value().to_string(),
        );
    }
}

fn bool_form_value(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

/// §10.2 example 163: `<iq type='get'><query xmlns='muc#owner'/></iq>`
/// requesting the current configuration form.
pub fn build_muc_owner_config_get_iq(room_jid: &BareJid) -> Element {
    let id = format!("muc-owner-get-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            room_jid.to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(Element::builder("query", NS_MUC_OWNER).build())
        .build()
}

/// Parse the §10.2 example 164 config-form result into a typed
/// [`RoomConfigForm`]. Reads only each field's own direct `<value/>`
/// children (never `<option><value/>`). Returns `None` when the IQ
/// carries no `muc#owner` query or no data form.
pub fn parse_muc_owner_config_form(iq: &Element) -> Option<RoomConfigForm> {
    let query = iq.get_child("query", NS_MUC_OWNER)?;
    let form = query.get_child("x", NS_DATA_FORMS)?;
    let fields = form
        .children()
        .filter(|child| child.name() == "field" && child.ns() == NS_DATA_FORMS)
        .filter_map(|field| {
            let var = field.attr("var")?.to_string();
            let values = field
                .children()
                .filter(|child| child.name() == "value" && child.ns() == NS_DATA_FORMS)
                .map(Element::text)
                .collect();
            Some(RoomConfigField { var, values })
        })
        .collect();
    Some(RoomConfigForm { fields })
}

/// §10.2 example 165: submit the merged configuration form. The
/// FORM_TYPE hidden field is always emitted first with the registrar
/// value `http://jabber.org/protocol/muc#roomconfig`; any FORM_TYPE
/// field inside `form` is skipped so the discriminator can never
/// drift or duplicate.
pub fn build_muc_owner_config_submit_iq(room_jid: &BareJid, form: &RoomConfigForm) -> Element {
    let id = format!("muc-owner-set-{}", next_id());
    let mut x = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(hidden_form_type_field());
    for field in &form.fields {
        if field.var == FIELD_FORM_TYPE {
            continue;
        }
        let mut builder = Element::builder("field", NS_DATA_FORMS).attr(
            minidom::rxml::xml_ncname!("var").to_owned(),
            field.var.as_str(),
        );
        for value in &field.values {
            builder = builder.append(
                Element::builder("value", NS_DATA_FORMS)
                    .append(value.as_str())
                    .build(),
            );
        }
        x = x.append(builder.build());
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            room_jid.to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("query", NS_MUC_OWNER)
                .append(x.build())
                .build(),
        )
        .build()
}

fn hidden_form_type_field() -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(
            minidom::rxml::xml_ncname!("var").to_owned(),
            FIELD_FORM_TYPE,
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(NS_MUC_ROOMCONFIG)
                .build(),
        )
        .build()
}

/// §10.9 example 200: `<iq type='set'><query xmlns='muc#owner'>
/// <destroy><reason/></destroy></query></iq>`. The alternate-venue
/// `jid` attribute is not offered — Waddle rooms have no successor
/// venue semantics.
pub fn build_muc_owner_destroy_iq(room_jid: &BareJid, reason: Option<&str>) -> Element {
    let id = format!("muc-owner-destroy-{}", next_id());
    let mut destroy = Element::builder("destroy", NS_MUC_OWNER);
    if let Some(reason) = reason.filter(|reason| !reason.is_empty()) {
        destroy = destroy.append(
            Element::builder("reason", NS_MUC_OWNER)
                .append(reason)
                .build(),
        );
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            room_jid.to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("query", NS_MUC_OWNER)
                .append(destroy.build())
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> BareJid {
        "general@muc.waddle.test".parse().expect("room JID")
    }

    #[test]
    fn config_get_iq_targets_room_with_owner_query() {
        let iq = build_muc_owner_config_get_iq(&room());
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("general@muc.waddle.test"));
        let query = iq.get_child("query", NS_MUC_OWNER).expect("query");
        assert_eq!(query.children().count(), 0);
    }

    #[test]
    fn parse_config_form_reads_only_direct_field_values() {
        // A list-single field carries its current value as a direct
        // <value/> and each selectable option under <option/>; the
        // parse must never pick up option values (§10.2 / web parity
        // rule in protocol-helpers.ts).
        let raw = r#"<iq xmlns='jabber:client' type='result' id='c1'>
            <query xmlns='http://jabber.org/protocol/muc#owner'>
              <x xmlns='jabber:x:data' type='form'>
                <field var='FORM_TYPE' type='hidden'>
                  <value>http://jabber.org/protocol/muc#roomconfig</value>
                </field>
                <field var='muc#roomconfig_roomname' type='text-single'>
                  <value>General</value>
                </field>
                <field var='urn:waddle:roomconfig:pinpermission' type='list-single'>
                  <value>admins-only</value>
                  <option label='Admins only'><value>admins-only</value></option>
                  <option label='Anyone'><value>anyone</value></option>
                </field>
              </x>
            </query>
          </iq>"#;
        let iq: Element = raw.parse().expect("parse fixture");
        let form = parse_muc_owner_config_form(&iq).expect("form");
        assert_eq!(form.value(FIELD_ROOM_NAME), Some("General"));
        let pin = form
            .fields
            .iter()
            .find(|field| field.var == FIELD_PIN_PERMISSION)
            .expect("pin field");
        assert_eq!(pin.values, vec!["admins-only".to_string()]);
        assert_eq!(
            RoomPinPermission::from_form_value(form.value(FIELD_PIN_PERMISSION).expect("value")),
            Some(RoomPinPermission::AdminsOnly)
        );
    }

    #[test]
    fn parse_config_form_returns_none_without_form() {
        let raw = r#"<iq xmlns='jabber:client' type='result' id='c2'>
            <query xmlns='http://jabber.org/protocol/muc#owner'/>
          </iq>"#;
        let iq: Element = raw.parse().expect("parse fixture");
        assert!(parse_muc_owner_config_form(&iq).is_none());
    }

    #[test]
    fn patch_merge_overrides_only_patched_fields() {
        let mut form = RoomConfigForm {
            fields: vec![
                RoomConfigField {
                    var: FIELD_ROOM_NAME.to_string(),
                    values: vec!["Old".to_string()],
                },
                RoomConfigField {
                    var: FIELD_MEMBERS_ONLY.to_string(),
                    values: vec!["1".to_string()],
                },
            ],
        };
        apply_room_config_patch(
            &mut form,
            &RoomConfigPatch {
                name: Some("New".to_string()),
                description: Some(String::new()),
                forum: Some(true),
                pin_permission: Some(RoomPinPermission::Anyone),
            },
        );
        assert_eq!(form.value(FIELD_ROOM_NAME), Some("New"));
        // Present-but-empty description clears server-side; the field
        // must exist with an empty value.
        let desc = form
            .fields
            .iter()
            .find(|field| field.var == FIELD_ROOM_DESC)
            .expect("desc field");
        assert_eq!(desc.values, vec![String::new()]);
        assert_eq!(form.bool_value(FIELD_FORUM_MODE), Some(true));
        assert_eq!(form.value(FIELD_PIN_PERMISSION), Some("anyone"));
        // Unpatched field kept verbatim.
        assert_eq!(form.bool_value(FIELD_MEMBERS_ONLY), Some(true));
    }

    #[test]
    fn submit_iq_carries_form_type_first_and_round_trips_fields() {
        let mut form = RoomConfigForm::default();
        // A stale FORM_TYPE inside the fetched form must not duplicate.
        form.set_value(FIELD_FORM_TYPE, "bogus".to_string());
        form.set_value(FIELD_ROOM_NAME, "General".to_string());
        form.set_value(FIELD_PUBLIC_ROOM, "0".to_string());
        let iq = build_muc_owner_config_submit_iq(&room(), &form);
        assert_eq!(iq.attr("type"), Some("set"));
        let x = iq
            .get_child("query", NS_MUC_OWNER)
            .and_then(|query| query.get_child("x", NS_DATA_FORMS))
            .expect("form");
        assert_eq!(x.attr("type"), Some("submit"));
        let fields: Vec<&Element> = x.children().filter(|c| c.name() == "field").collect();
        assert_eq!(fields[0].attr("var"), Some(FIELD_FORM_TYPE));
        assert_eq!(
            fields[0]
                .get_child("value", NS_DATA_FORMS)
                .map(Element::text),
            Some(NS_MUC_ROOMCONFIG.to_string())
        );
        let form_type_count = fields
            .iter()
            .filter(|field| field.attr("var") == Some(FIELD_FORM_TYPE))
            .count();
        assert_eq!(form_type_count, 1);
        assert_eq!(fields[1].attr("var"), Some(FIELD_ROOM_NAME));
        assert_eq!(fields[2].attr("var"), Some(FIELD_PUBLIC_ROOM));
    }

    #[test]
    fn destroy_iq_matches_section_10_9_shape() {
        let iq = build_muc_owner_destroy_iq(&room(), Some("archived"));
        assert_eq!(iq.attr("type"), Some("set"));
        let destroy = iq
            .get_child("query", NS_MUC_OWNER)
            .and_then(|query| query.get_child("destroy", NS_MUC_OWNER))
            .expect("destroy");
        assert_eq!(destroy.attr("jid"), None);
        assert_eq!(
            destroy.get_child("reason", NS_MUC_OWNER).map(Element::text),
            Some("archived".to_string())
        );
    }

    #[test]
    fn destroy_iq_omits_empty_reason() {
        let iq = build_muc_owner_destroy_iq(&room(), None);
        let destroy = iq
            .get_child("query", NS_MUC_OWNER)
            .and_then(|query| query.get_child("destroy", NS_MUC_OWNER))
            .expect("destroy");
        assert!(destroy.get_child("reason", NS_MUC_OWNER).is_none());
    }
}
