use tracing::warn;
use waddle_xmpp::{
    parser::stanza_to_string,
    protocol::handlers::errors::bad_request_reply,
    protocol::{frame::InboundFrame, InboundEvent, XmppStateMachine},
    telemetry::attributes::CallSignalEvent,
    xep::xep0353::NS_JINGLE_MESSAGE,
    xep::{has_hint, Hint, NS_DELAY, NS_INBOX, NS_WADDLE_INBOX},
    Stanza,
};

use super::super::{
    call_signaling_telemetry::{record_call_signal, CallSignalTarget},
    interpret_loop::build_interpret_deps,
    replay::drive_interpret_loop,
    WebSocketState,
};
use crate::auth::Session;
use crate::server::routes::websocket::ResolvedPrincipal;
use waddle_xmpp::protocol::ConnectionPhase;

mod dm_pin;
mod group_dm_invite;
mod link_preview_stamp;
mod muc_direct;
mod muc_invite;

use dm_pin::{handle_dm_pin_message, handle_dm_pin_retraction_cascade};
use group_dm_invite::handle_group_dm_mediated_invite;
use link_preview_stamp::consume_link_preview_request;
use muc_direct::handle_muc_direct_message;
use muc_invite::handle_muc_mediated_invite;

/// Thin transport adapter that drives the sans-I/O dispatcher
/// (#229 PR16 + PR18). Every `<message/>` stanza arriving on the
/// WebSocket transport flows through here:
///
/// 1. Wrap the typed [`xmpp_parsers::message::Message`] in
///    [`InboundEvent::FrameReceived`] and feed it to the per-connection
///    [`XmppStateMachine`]. The state machine runs the locked-Q2(a)
///    chain (`BlockingFilter → RichTargetValidation → Canonicalize →
///    EnrichmentDispatch → Archive → CarbonsMessage → Inbox → Route`)
///    for `<message type='chat'>` and emits typed
///    [`waddle_xmpp::protocol::OutboundEvent`]s.
/// 2. For `<message type='groupchat'>` traffic, the chain emits
///    [`waddle_xmpp::protocol::OutboundEvent::DispatchToRoom`]; the
///    interpreter resolves it against the room handler chain
///    (`OccupancyValidation → MucCanonicalize → MucArchive →
///    MucInbox → Reflector`) and recursively interprets the chain's
///    own emitted events.
/// 3. The interpreter ([`crate::server::routes::interpret::interpret`])
///    executes the I/O side effects (route to peer, persist to MAM,
///    project inbox, fan XEP-0280 carbons, etc.) and returns the
///    serialized wire frames the caller writes back to the sender's
///    transport.
///
/// `authenticated_session` is threaded through so the
/// [`OutboundEvent::DispatchToRoom`] arm can perform the managed-room
/// owner check (announcements room admits server owners only).
///
/// [`OutboundEvent::DispatchToRoom`]: waddle_xmpp::protocol::OutboundEvent::DispatchToRoom
pub async fn handle_message(
    mut incoming: xmpp_parsers::message::Message,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    state_machine: Option<&mut XmppStateMachine>,
    authenticated_session: Option<&Session>,
    ordered_relay_origin: Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> Vec<String> {
    let Some(bound_jid) = phase.bound_jid().cloned() else {
        warn!("Message received without authenticated session");
        return vec![];
    };
    let Some(sm) = state_machine else {
        warn!(
            "Message received before per-connection state machine was initialized; \
             dropping. This indicates a stanza arrived before bind completed."
        );
        return vec![];
    };

    strip_client_authored_delay(&mut incoming);
    consume_link_preview_request(
        &mut incoming,
        &bound_jid,
        state.deps.occupant_id_secret.key(),
        chrono::Utc::now().timestamp(),
        state.deps.auth_state.base_url.as_str(),
        &state.deps.link_preview,
    );

    if let Some(frames) =
        handle_group_dm_mediated_invite(&incoming, state, &bound_jid, authenticated_session).await
    {
        return frames;
    }
    // XEP-0045 §7.8 (#1248): mediated invitations for every non-group-DM
    // room — previously these fell through and were silently dropped.
    if let Some(frames) =
        handle_muc_mediated_invite(&incoming, state, &bound_jid, authenticated_session).await
    {
        return frames;
    }
    if let Some(frames) = handle_dm_pin_message(&incoming, state, &bound_jid).await {
        return frames;
    }
    handle_dm_pin_retraction_cascade(&incoming, state, &bound_jid).await;

    if incoming.type_ != xmpp_parsers::message::MessageType::Error
        && message_has_inbox_payload(&incoming)
    {
        let mut stamped = incoming.clone();
        stamped.from = Some(jid::Jid::from(bound_jid));
        strip_inbox_payloads(&mut stamped);
        let reply = bad_request_reply(&stamped, "Client-authored inbox payloads are not allowed.");
        return match stanza_to_string(reply) {
            Ok(frame) => vec![frame],
            Err(error) => {
                warn!(error = ?error, "failed to serialize inbox rejection");
                vec![]
            }
        };
    }

    if incoming.type_ != xmpp_parsers::message::MessageType::Groupchat
        && incoming.type_ != xmpp_parsers::message::MessageType::Error
        && waddle_extensions::message_has_framework_envelope(&incoming)
    {
        let mut stamped = incoming.clone();
        stamped.from = Some(jid::Jid::from(bound_jid));
        remove_framework_envelopes(&mut stamped);
        let reply = bad_request_reply(
            &stamped,
            "Client-authored Waddle extension envelopes are not allowed.",
        );
        return match stanza_to_string(reply) {
            Ok(frame) => vec![frame],
            Err(error) => {
                warn!(error = ?error, "failed to serialize framework-envelope rejection");
                vec![]
            }
        };
    }
    record_jmi_signal(&incoming, &bound_jid.to_bare());
    if let Some(frames) = handle_muc_direct_message(&incoming, state, &bound_jid).await {
        return frames;
    }

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(incoming),
    ))));
    let principal = authenticated_session.map(ResolvedPrincipal::from_authenticated_session);
    let deps =
        build_interpret_deps(state, principal).with_ordered_relay_origin(ordered_relay_origin);
    // Stanza dispatch never emits keepalive/timer effects (those come
    // only from TransportReady/Tick in the connection loop), and
    // `close` was already ignored on this path — only frames matter.
    drive_interpret_loop(events, sm, &deps).await.frames
}

fn record_jmi_signal(message: &xmpp_parsers::message::Message, user: &jid::BareJid) {
    let Some(event) = classify_jmi_signal(message) else {
        return;
    };
    let peer = message.to.as_ref().map(jid::Jid::to_bare);
    record_call_signal(event, user, peer.as_ref().map(CallSignalTarget::Peer));
}

fn classify_jmi_signal(message: &xmpp_parsers::message::Message) -> Option<CallSignalEvent> {
    if message.type_ != xmpp_parsers::message::MessageType::Chat || !has_hint(message, Hint::Store)
    {
        return None;
    }

    let event = message.payloads.iter().find_map(|payload| {
        if payload.ns() != NS_JINGLE_MESSAGE || payload.attr("id").is_none_or(str::is_empty) {
            return None;
        }
        match payload.name() {
            "propose" => Some(CallSignalEvent::JmiPropose),
            "proceed" => Some(CallSignalEvent::JmiProceed),
            "reject" => Some(CallSignalEvent::JmiReject),
            "retract" => Some(CallSignalEvent::JmiRetract),
            _ => None,
        }
    });
    event
}

fn strip_client_authored_delay(message: &mut xmpp_parsers::message::Message) {
    message
        .payloads
        .retain(|payload| !(payload.name() == "delay" && payload.ns() == NS_DELAY));
}

fn message_has_inbox_payload(message: &xmpp_parsers::message::Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| payload.ns() == NS_INBOX || payload.ns() == NS_WADDLE_INBOX)
}

fn strip_inbox_payloads(message: &mut xmpp_parsers::message::Message) {
    message
        .payloads
        .retain(|payload| payload.ns() != NS_INBOX && payload.ns() != NS_WADDLE_INBOX);
}

fn remove_framework_envelopes(message: &mut xmpp_parsers::message::Message) {
    message
        .payloads
        .retain(|payload| !payload.ns().starts_with("urn:waddle:"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Message, MessageType};
    use xmpp_parsers::minidom::Element;

    #[test]
    fn strips_client_supplied_delay_without_touching_other_payloads() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hello</body>\
                    <delay xmlns='urn:xmpp:delay' from='evil.example' stamp='2024-06-01T09:30:00Z'>forged</delay>\
                    <envelope xmlns='urn:waddle:test'/>\
                    </message>";
        let mut message =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("message");

        strip_client_authored_delay(&mut message);

        assert!(message
            .payloads
            .iter()
            .all(|payload| payload.ns() != NS_DELAY));
        assert!(message
            .payloads
            .iter()
            .any(|payload| payload.ns().starts_with("urn:waddle:")));
    }

    #[test]
    fn classifies_conformant_jmi_chat_signal() {
        let mut message = Message::new(Some(
            "bob@example.com/phone"
                .parse::<jid::Jid>()
                .expect("valid JID"),
        ));
        message.payloads.push(
            Element::builder("proceed", NS_JINGLE_MESSAGE)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call-1")
                .build(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_hint_element(Hint::Store));

        assert_eq!(
            classify_jmi_signal(&message),
            Some(CallSignalEvent::JmiProceed)
        );
    }

    #[test]
    fn ignores_non_chat_or_malformed_jmi_signal() {
        let mut message = Message::new(Some(
            "bob@example.com/phone"
                .parse::<jid::Jid>()
                .expect("valid JID"),
        ));
        message.type_ = MessageType::Groupchat;
        message.payloads.push(
            Element::builder("proceed", NS_JINGLE_MESSAGE)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call-1")
                .build(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_hint_element(Hint::Store));
        assert_eq!(classify_jmi_signal(&message), None);

        message.type_ = MessageType::Chat;
        message.payloads[0] = Element::builder("proceed", NS_JINGLE_MESSAGE).build();
        assert_eq!(classify_jmi_signal(&message), None);
    }
}
