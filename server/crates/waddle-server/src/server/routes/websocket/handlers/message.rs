use tracing::warn;
use waddle_xmpp::{
    parser::stanza_to_string,
    protocol::handlers::errors::bad_request_reply,
    protocol::{frame::InboundFrame, InboundEvent, XmppStateMachine},
    xep::{NS_DELAY, NS_INBOX, NS_WADDLE_INBOX},
    Stanza,
};

use super::super::{
    interpret_loop::build_interpret_deps, replay::drive_interpret_loop, WebSocketState,
};
use crate::auth::Session;
use waddle_xmpp::protocol::ConnectionPhase;

mod dm_pin;
mod group_dm_invite;
mod link_preview_stamp;
mod muc_direct;

use dm_pin::{handle_dm_pin_message, handle_dm_pin_retraction_cascade};
use group_dm_invite::handle_group_dm_mediated_invite;
use link_preview_stamp::consume_link_preview_request;
use muc_direct::handle_muc_direct_message;

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
    if let Some(frames) = handle_muc_direct_message(&incoming, state, &bound_jid).await {
        return frames;
    }

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(incoming),
    ))));
    let deps = build_interpret_deps(state, authenticated_session);
    // Stanza dispatch never emits keepalive/timer effects (those come
    // only from TransportReady/Tick in the connection loop), and
    // `close` was already ignored on this path — only frames matter.
    drive_interpret_loop(events, sm, &deps).await.frames
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
    use xmpp_parsers::message::Message;
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
}
