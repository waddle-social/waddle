//! IQ handler for `<query xmlns='urn:waddle:pin:0'/>` (#414).
//!
//! Returns the room's current pinned-message list, populated from the
//! `MucRoom.pinned_entries` actor state via the `GetPinList` actor
//! message. Responses carry one `<pin/>` element per entry, in
//! pin-time-desc order, mirroring the projection-list shape that
//! BroadcastRoomSystemMessage emits inline on each pin/unpin event.

use super::*;
use jid::BareJid;
use std::str::FromStr;
use waddle_xmpp::muc::room_actor::GetPinList;
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::xep::xep0470::NS_WADDLE_PIN_V0;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::minidom::Element;

/// Detects `<iq type='get'><query xmlns='urn:waddle:pin:0'/></iq>`
/// targeting a MUC room JID.
pub(super) fn is_pin_query_iq(iq: &Iq, muc_domain: &str) -> bool {
    if !matches!(iq.payload, IqType::Get(_)) {
        return false;
    }
    let IqType::Get(ref payload) = iq.payload else {
        return false;
    };
    if payload.name() != "query" || payload.ns() != NS_WADDLE_PIN_V0 {
        return false;
    }
    iq.to
        .as_ref()
        .is_some_and(|to| to.domain().as_str() == muc_domain)
}

pub(super) async fn handle_pin_query_iq(
    iq: &Iq,
    state: &WebSocketState,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(target) = iq.to.as_ref() else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            bad_request_iq_error("Pin query requires a room JID in 'to'."),
        )];
    };
    let Ok(room_jid) = BareJid::from_str(&target.to_string()) else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            bad_request_iq_error("Pin query 'to' must be a bare room JID."),
        )];
    };

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
                &iq.id,
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
                &iq.id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };

    let entries = match room_actor.ask(GetPinList).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                room = %room_jid,
                ?error,
                "Pin query: GetPinList ask failed"
            );
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };

    let mut query = Element::builder("query", NS_WADDLE_PIN_V0).build();
    for entry in entries {
        let mut pin_elem = Element::builder("pin", NS_WADDLE_PIN_V0)
            .attr("id", entry.target_stanza_id.as_str())
            .attr("by", entry.pinner_jid.to_string().as_str())
            .attr("at", entry.pinned_at.to_rfc3339().as_str())
            .build();
        let mut preview = Element::builder("preview", NS_WADDLE_PIN_V0).build();
        let mut author = Element::builder("author", NS_WADDLE_PIN_V0)
            .attr("jid", entry.preview.author_jid.to_string().as_str())
            .build();
        if let Some(ref nick) = entry.preview.author_nick {
            author.set_attr("nick", nick);
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

    let response = Iq {
        from: response_from.and_then(|s| jid::Jid::from_str(s).ok()),
        to: response_to.and_then(|s| jid::Jid::from_str(s).ok()),
        id: iq.id.clone(),
        payload: IqType::Result(Some(query)),
    };
    vec![iq_to_xml(response)]
}
