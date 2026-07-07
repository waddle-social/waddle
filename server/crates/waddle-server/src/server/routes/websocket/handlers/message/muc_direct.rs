use waddle_xmpp::{
    muc::room_registry_actor::GetRoom, parser::stanza_to_string,
    protocol::handlers::errors::message_error_reply, Stanza,
};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::server::routes::websocket::WebSocketState;

pub(super) async fn handle_muc_direct_message(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) -> Option<Vec<String>> {
    if let Some(frames) = handle_muc_private_message(incoming, state, bound_jid).await {
        return Some(frames);
    }
    handle_muc_mediated_decline(incoming, state, bound_jid).await
}

async fn handle_muc_private_message(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) -> Option<Vec<String>> {
    let target_occupant_jid = incoming.to.as_ref()?.clone().try_into_full().ok()?;
    let room_jid = target_occupant_jid.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }
    if incoming.type_ == MessageType::Groupchat {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "Groupchat messages must be addressed to the room bare JID.",
        )]);
    }
    if !matches!(incoming.type_, MessageType::Chat | MessageType::Normal) {
        return None;
    }
    let target_nick = target_occupant_jid.resource().to_string();

    let Some(room_actor) = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .ok()
        .flatten()
    else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested room not found.",
        )]);
    };
    let Ok(snapshot) = room_actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .await
    else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    };
    let Some(sender_nick) = snapshot.room.find_nick_by_real_jid(bound_jid) else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "Only room occupants may send private messages.",
        )]);
    };
    if snapshot.room.get_occupant(&target_nick).is_none() {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested occupant not found.",
        )]);
    }

    let from_room_jid = match room_jid.clone().with_resource_str(sender_nick) {
        Ok(jid) => jid,
        Err(_) => {
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    };
    for recipient in snapshot.room.get_occupant_sessions(&target_nick) {
        let mut routed = incoming.clone();
        routed.from = Some(jid::Jid::from(from_room_jid.clone()));
        routed.to = Some(jid::Jid::from(recipient.clone()));
        canonicalize_muc_private_payloads(&mut routed, &room_jid);
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&recipient, Stanza::Message(routed));
    }

    Some(Vec::new())
}

async fn handle_muc_mediated_decline(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) -> Option<Vec<String>> {
    if incoming.type_ != MessageType::Normal {
        return None;
    }
    let room_jid = incoming.to.as_ref()?.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }
    let Some(room_actor) = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .ok()
        .flatten()
    else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested room not found.",
        )]);
    };
    let Ok(snapshot) = room_actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .await
    else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    };
    let inbound_decline = mediated_decline(incoming)?;
    let Some(to_attr) = inbound_decline.attr("to") else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "Mediated decline missing target.",
        )]);
    };
    let Ok(to) = to_attr.parse::<jid::Jid>() else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "Mediated decline target is not a valid JID.",
        )]);
    };
    let recipients = decline_recipients(&room_jid, &snapshot.room, &to);

    let x = build_mediated_decline_payload(bound_jid, inbound_decline);
    for recipient in recipients {
        let mut mediated = Message::new(Some(jid::Jid::from(recipient.clone())));
        mediated.id = incoming.id.clone();
        mediated.from = Some(jid::Jid::from(room_jid.clone()));
        mediated.type_ = MessageType::Normal;
        mediated.payloads.push(x.clone());
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&recipient, Stanza::Message(mediated));
    }

    Some(Vec::new())
}

fn decline_recipients(
    room_jid: &jid::BareJid,
    room: &waddle_xmpp::muc::MucRoom,
    jid: &jid::Jid,
) -> Vec<jid::FullJid> {
    if let Ok(full) = jid.clone().try_into_full() {
        if full.to_bare() == *room_jid {
            return room.get_occupant_sessions(full.resource().as_ref());
        }
        if room.find_nick_by_real_jid(&full).is_some() {
            return vec![full];
        }
        return Vec::new();
    }
    let bare = jid.to_bare();
    if bare == *room_jid {
        return Vec::new();
    }
    room.occupants
        .values()
        .filter(|occupant| occupant.real_jid.to_bare() == bare)
        .flat_map(|occupant| room.get_occupant_sessions(&occupant.nick))
        .collect()
}

fn mediated_decline(message: &Message) -> Option<&minidom::Element> {
    message
        .payloads
        .iter()
        .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
        .and_then(|x| x.get_child("decline", waddle_xmpp::muc::presence::NS_MUC_USER))
}

fn build_mediated_decline_payload(
    decliner: &jid::FullJid,
    inbound_decline: &minidom::Element,
) -> minidom::Element {
    let mut decline = minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            decliner.to_bare().to_string(),
        );
    if let Some(reason) =
        inbound_decline.get_child("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
    {
        decline = decline.append(reason.clone());
    }
    minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(decline.build())
        .build()
}

fn canonicalize_muc_private_payloads(message: &mut Message, room_jid: &jid::BareJid) {
    message.payloads.retain(|payload| {
        if payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            || payload.is("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
        {
            return false;
        }
        if payload.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID) {
            return payload
                .attr("by")
                .and_then(|raw| raw.parse::<jid::BareJid>().ok())
                .is_none_or(|by| by != *room_jid);
        }
        true
    });
    message
        .payloads
        .push(minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER).build());
}

fn message_error_frame(
    incoming: &Message,
    bound_jid: &jid::FullJid,
    error_type: ErrorType,
    condition: DefinedCondition,
    text: &'static str,
) -> String {
    let mut stamped = incoming.clone();
    stamped.from = Some(jid::Jid::from(bound_jid.clone()));
    let reply = message_error_reply(
        &stamped,
        StanzaError::new(error_type, condition, "en", text),
    );
    stanza_to_string(reply).unwrap_or_default()
}
