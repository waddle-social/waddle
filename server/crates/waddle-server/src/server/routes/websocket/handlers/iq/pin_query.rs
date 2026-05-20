//! IQ handler for `<query xmlns='urn:waddle:pin:0'/>` (#414).
//!
//! Returns the room's current pinned-message list, populated from the
//! `MucRoom.pinned_entries` actor state via the `GetPinList` actor
//! message.
//!
//! ## Authorization
//!
//! The query is gated on the requester being a current occupant of
//! the target room. Non-occupants get `<forbidden type='auth'/>` —
//! pin lists carry message previews and authors, which is room-scoped
//! information that members-only rooms must not leak. Mirrors the
//! affiliation-style check `muc_admin` performs.

use super::*;
use jid::FullJid;
use std::str::FromStr;
use waddle_xmpp::muc::room_actor::{GetPinList, GetRoomSnapshot};
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

/// Detects `<iq type='get'><query xmlns='urn:waddle:pin:0'/></iq>`
/// targeting a MUC room JID.
pub(super) fn is_pin_query_iq(iq: &Iq, muc_domain: &str) -> bool {
    let Iq::Get { payload, .. } = iq else {
        return false;
    };
    if payload.name() != "query" || payload.ns() != NS_WADDLE_PIN_V0 {
        return false;
    }
    iq.to().is_some_and(|to| to.domain().as_str() == muc_domain)
}

pub(super) async fn handle_pin_query_iq(
    iq: &Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let Some(target) = iq.to() else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Pin query requires a room JID in 'to'."),
        )];
    };
    if target.resource().is_some() {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Pin query 'to' must be a bare room JID."),
        )];
    }
    let room_jid = target.to_bare();

    let room_actor = match state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                item_not_found_iq_error("Room not found."),
            )];
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                ?error,
                "Pin query: room registry lookup failed"
            );
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };

    // Membership gate: the snapshot returned for `sender` carries the
    // sender's affiliation/role only when the actor recognises them as
    // a current occupant. We treat any of the public affiliations
    // (Owner / Admin / Member) as access-granting, plus `Outcast` is
    // explicitly denied; anyone else (None, with no occupancy) gets
    // `<forbidden/>`.
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: sender.clone(),
        })
        .await
    {
        Ok(snap) => snap,
        Err(error) => {
            warn!(
                room = %room_jid,
                ?error,
                "Pin query: GetRoomSnapshot failed"
            );
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    let sender_bare = sender.to_bare();
    let is_occupant = snapshot
        .occupants
        .iter()
        .any(|occ| occ.full_jid.to_bare() == sender_bare);
    if !is_occupant {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            forbidden_iq_error("Pin list is visible only to current room occupants."),
        )];
    }

    let entries = match room_actor.ask(GetPinList).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                room = %room_jid,
                ?error,
                "Pin query: GetPinList ask failed"
            );
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };

    let mut query = Element::builder("query", NS_WADDLE_PIN_V0).build();
    for entry in entries {
        let mut pin_elem = Element::builder("pin", NS_WADDLE_PIN_V0)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                entry.target_stanza_id.id.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("by").to_owned(),
                entry.pinner_jid.to_string().as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("at").to_owned(),
                entry.pinned_at.to_rfc3339().as_str(),
            )
            .build();
        let mut preview = Element::builder("preview", NS_WADDLE_PIN_V0).build();
        let mut author = Element::builder("author", NS_WADDLE_PIN_V0)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                entry.preview.author_jid.to_string().as_str(),
            )
            .build();
        if let Some(ref nick) = entry.preview.author_nick {
            author.set_attr(
                minidom::rxml::Namespace::NONE,
                minidom::rxml::xml_ncname!("nick").to_owned(),
                nick,
            );
        }
        preview.append_child(author);
        let mut text = Element::builder("text", NS_WADDLE_PIN_V0).build();
        text.append_text_node(&entry.preview.text);
        preview.append_child(text);
        let mut ts = Element::builder("ts", NS_WADDLE_PIN_V0).build();
        ts.append_text_node(entry.preview.message_timestamp.to_rfc3339());
        preview.append_child(ts);
        pin_elem.append_child(preview);
        query.append_child(pin_elem);
    }

    let response = Iq::Result {
        from: response_from.and_then(|s| jid::Jid::from_str(s).ok()),
        to: response_to.and_then(|s| jid::Jid::from_str(s).ok()),
        id: iq.id().to_string(),
        payload: Some(query),
    };
    vec![iq_to_xml(response)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iq_get_with_payload(payload: Element, to: Option<&str>) -> Iq {
        Iq::Get {
            from: None,
            to: to.and_then(|s| jid::Jid::from_str(s).ok()),
            id: "test-iq".into(),
            payload,
        }
    }

    #[test]
    fn is_pin_query_iq_accepts_well_formed_get() {
        let payload = Element::builder("query", NS_WADDLE_PIN_V0).build();
        let iq = iq_get_with_payload(payload, Some("room@conf.example"));
        assert!(is_pin_query_iq(&iq, "conf.example"));
    }

    #[test]
    fn is_pin_query_iq_rejects_wrong_namespace() {
        let payload = Element::builder("query", "wrong:ns").build();
        let iq = iq_get_with_payload(payload, Some("room@conf.example"));
        assert!(!is_pin_query_iq(&iq, "conf.example"));
    }

    #[test]
    fn is_pin_query_iq_rejects_wrong_element_name() {
        let payload = Element::builder("items", NS_WADDLE_PIN_V0).build();
        let iq = iq_get_with_payload(payload, Some("room@conf.example"));
        assert!(!is_pin_query_iq(&iq, "conf.example"));
    }

    #[test]
    fn is_pin_query_iq_rejects_set() {
        let payload = Element::builder("query", NS_WADDLE_PIN_V0).build();
        let iq = Iq::Set {
            from: None,
            to: Some(jid::Jid::from_str("room@conf.example").expect("valid jid")),
            id: "test-iq".into(),
            payload,
        };
        assert!(!is_pin_query_iq(&iq, "conf.example"));
    }

    #[test]
    fn is_pin_query_iq_rejects_missing_to() {
        let payload = Element::builder("query", NS_WADDLE_PIN_V0).build();
        let iq = iq_get_with_payload(payload, None);
        assert!(!is_pin_query_iq(&iq, "conf.example"));
    }

    #[test]
    fn is_pin_query_iq_rejects_wrong_domain() {
        let payload = Element::builder("query", NS_WADDLE_PIN_V0).build();
        let iq = iq_get_with_payload(payload, Some("room@other.example"));
        assert!(!is_pin_query_iq(&iq, "conf.example"));
    }
}
