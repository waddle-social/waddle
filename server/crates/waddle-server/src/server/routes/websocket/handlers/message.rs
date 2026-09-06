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
use crate::server::routes::interpret::{
    effects::{
        AuthorizationDeniedReason, Effect, ExternalEffect, PlanRejection, PlannedEffect,
        PolicyDeniedReason, SemanticMalformedReason,
    },
    Deps,
};
use crate::server::routes::websocket::ResolvedPrincipal;
use waddle_xmpp::protocol::ConnectionPhase;

pub mod dm_pin;
pub mod group_dm_invite;
mod link_preview_stamp;
pub mod muc_direct;
pub mod muc_invite;

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
    ingress_effect_capture: Option<crate::ingress_shadow::IngressEffectCapture>,
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

    let principal = authenticated_session.map(ResolvedPrincipal::from_authenticated_session);
    let deps = build_interpret_deps(state, principal)
        .with_ordered_relay_origin(ordered_relay_origin)
        .with_ingress_effect_capture(ingress_effect_capture);
    if let Some(frames) = dispatch_early_message(&mut incoming, &bound_jid, &deps).await {
        return frames
            .into_iter()
            .filter_map(|stanza| {
                stanza_to_string(match stanza {
                    Stanza::Message(message) => minidom::Element::from(message),
                    Stanza::Presence(presence) => presence.into(),
                    Stanza::Iq(iq) => (*iq).into(),
                })
                .map_err(|error| warn!(?error, "failed to serialize early message reply"))
                .ok()
            })
            .collect();
    }
    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(incoming),
    ))));
    drive_interpret_loop(events, sm, &deps).await.frames
}

/// The complete pre-interpreter path, shared by immediate transport dispatch
/// and Phase A. Every returned stanza crosses the same typed frame seam.
pub(crate) async fn dispatch_early_message(
    incoming: &mut xmpp_parsers::message::Message,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
) -> Option<Vec<Stanza>> {
    deps.effects.observe_sender(bound_jid);
    strip_client_authored_delay(incoming);
    if let Some(capture) = &deps.ingress_effect_capture {
        capture.record_sanitized_message(incoming);
    }
    let frames = dispatch_early_handlers(incoming, bound_jid, deps).await?;
    for stanza in &frames {
        crate::server::routes::interpret::capture_serialized_error_reply(deps, stanza);
        let _ = deps
            .effects
            .execute(
                PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
                    stanza.clone(),
                )))),
                deps,
            )
            .await;
    }
    Some(frames)
}

async fn dispatch_early_handlers(
    incoming: &mut xmpp_parsers::message::Message,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
) -> Option<Vec<Stanza>> {
    if let Some(state) = deps.web_socket_state {
        consume_link_preview_request(
            incoming,
            bound_jid,
            state.deps.occupant_id_secret.key(),
            chrono::Utc::now().timestamp(),
            state.deps.auth_state.base_url.as_str(),
            &state.deps.link_preview,
        );
        let session = deps.authenticated_principal.map(ResolvedPrincipal::session);
        if let Some(frames) =
            handle_group_dm_mediated_invite(incoming, state, bound_jid, session, deps).await
        {
            return Some(frames);
        }
        if let Some(frames) =
            handle_muc_mediated_invite(incoming, state, bound_jid, session, deps).await
        {
            return Some(frames);
        }
        if let Some(frames) = handle_dm_pin_message(incoming, state, bound_jid, deps).await {
            return Some(frames);
        }
        handle_dm_pin_retraction_cascade(incoming, state, bound_jid, deps).await;
    }
    if incoming.type_ != xmpp_parsers::message::MessageType::Error
        && message_has_inbox_payload(incoming)
    {
        let mut stamped = incoming.clone();
        stamped.from = Some(jid::Jid::from(bound_jid.clone()));
        strip_inbox_payloads(&mut stamped);
        record_rejected_envelope(deps, &stamped);
        let reply = bad_request_reply(&stamped, "Client-authored inbox payloads are not allowed.");
        return Some(reject_message(
            deps,
            Stanza::Message(reply),
            PlanRejection::SemanticMalformed(SemanticMalformedReason::ClientAuthoredInboxPayload),
        ));
    }
    if incoming.type_ != xmpp_parsers::message::MessageType::Groupchat
        && incoming.type_ != xmpp_parsers::message::MessageType::Error
        && waddle_extensions::message_has_framework_envelope(incoming)
    {
        let mut stamped = incoming.clone();
        stamped.from = Some(jid::Jid::from(bound_jid.clone()));
        remove_framework_envelopes(&mut stamped);
        record_rejected_envelope(deps, &stamped);
        let reply = bad_request_reply(
            &stamped,
            "Client-authored Waddle extension envelopes are not allowed.",
        );
        return Some(reject_message(
            deps,
            Stanza::Message(reply),
            PlanRejection::SemanticMalformed(
                SemanticMalformedReason::ClientAuthoredFrameworkEnvelope,
            ),
        ));
    }
    record_jmi_signal(incoming, &bound_jid.to_bare());
    if let Some(state) = deps.web_socket_state {
        return handle_muc_direct_message(incoming, state, bound_jid, deps).await;
    }
    None
}

fn record_rejected_envelope(deps: &Deps<'_>, message: &xmpp_parsers::message::Message) {
    if let Some(capture) = &deps.ingress_effect_capture {
        capture.record_sanitized_message(message);
    }
}

pub(super) fn reject_message(
    deps: &Deps<'_>,
    stanza: Stanza,
    rejection: PlanRejection,
) -> Vec<Stanza> {
    deps.effects.set_rejection(rejection);
    vec![stanza]
}

pub(super) fn classify_rejection(error: &xmpp_parsers::stanza_error::StanzaError) -> PlanRejection {
    use xmpp_parsers::stanza_error::DefinedCondition;
    match error.defined_condition {
        DefinedCondition::Forbidden | DefinedCondition::NotAuthorized => {
            PlanRejection::AuthorizationDenied(AuthorizationDeniedReason::Forbidden)
        }
        DefinedCondition::BadRequest => {
            PlanRejection::SemanticMalformed(SemanticMalformedReason::MalformedPayload)
        }
        _ => PlanRejection::PolicyDenied(PolicyDeniedReason::StanzaError(
            waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(error)
                .expect("server-built stanza error"),
        )),
    }
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
    use crate::ingress_shadow::IngressEffectCapture;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::protocol::XmppStateMachine;
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

    #[tokio::test]
    async fn fast_path_inbox_payload_rejection_emits_no_shadow_marker() {
        let state = create_test_websocket_state().await;
        let bound: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let phase = ConnectionPhase::ready(bound.clone(), false);
        let capture = IngressEffectCapture::new(None);
        let mut sm = XmppStateMachine::new("example.com", Default::default());
        let mut incoming = Message::new(Some("bob@example.com".parse::<jid::Jid>().expect("jid")));
        incoming.type_ = MessageType::Chat;
        incoming.from = Some(jid::Jid::from(bound.clone()));
        incoming
            .payloads
            .push(Element::builder("result", NS_INBOX).build());

        let frames = handle_message(
            incoming,
            state.as_ref(),
            &phase,
            Some(&mut sm),
            None,
            None,
            Some(capture.clone()),
        )
        .await;

        assert!(capture.snapshot().markers.is_empty());
        assert_eq!(frames.len(), 1);
    }

    #[tokio::test]
    async fn fast_path_framework_envelope_rejection_emits_no_shadow_marker() {
        let state = create_test_websocket_state().await;
        let bound: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let phase = ConnectionPhase::ready(bound.clone(), false);
        let capture = IngressEffectCapture::new(None);
        let mut sm = XmppStateMachine::new("example.com", Default::default());
        let mut incoming = Message::new(Some("bob@example.com".parse::<jid::Jid>().expect("jid")));
        incoming.type_ = MessageType::Chat;
        incoming.from = Some(jid::Jid::from(bound));
        incoming
            .payloads
            .push(Element::builder("extensions", waddle_extensions::FRAMEWORK_NAMESPACE).build());

        let frames = handle_message(
            incoming,
            state.as_ref(),
            &phase,
            Some(&mut sm),
            None,
            None,
            Some(capture.clone()),
        )
        .await;

        assert!(capture.snapshot().markers.is_empty());
        assert_eq!(frames.len(), 1);
    }

    #[tokio::test]
    async fn sanitized_snapshot_keeps_pre_enrichment_link_preview_request() {
        let state = create_test_websocket_state().await;
        let bound: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let phase = ConnectionPhase::ready(bound.clone(), false);
        let capture = IngressEffectCapture::new(None);
        let mut sm = XmppStateMachine::new("example.com", Default::default());
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: bound.to_bare(),
            scope_jid: "bob@example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://the.link.example.com/what-was-linked-to")
                .expect("url"),
            normalized_url: url::Url::parse(
                "https://example.com/canonical-url/for/what-was-linked-to",
            )
            .expect("url"),
            title: Some("The Best Webpage".to_string()),
            description: Some("This is a great webpage and you will really like it".to_string()),
            image: None,
            video: None,
            player: None,
            native_video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(
            &preview,
            state.deps.occupant_id_secret.key(),
        );
        let mut incoming = Message::new(Some("bob@example.com".parse::<jid::Jid>().expect("jid")));
        incoming.type_ = MessageType::Chat;
        incoming.from = Some(jid::Jid::from(bound));
        incoming.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://the.link.example.com/what-was-linked-to".to_string(),
        );
        incoming
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        let _ = handle_message(
            incoming,
            state.as_ref(),
            &phase,
            Some(&mut sm),
            None,
            None,
            Some(capture.clone()),
        )
        .await;

        let snapshot = capture.snapshot();
        let sanitized = snapshot
            .sanitized_message
            .expect("capture should keep a sanitized message snapshot");
        assert!(
            waddle_xmpp::xep::extract_link_preview_request_from_message(&sanitized).is_some(),
            "the shadow snapshot must retain the pre-enrichment request token"
        );
        assert!(
            sanitized.payloads.iter().all(|payload| {
                !waddle_xmpp::xep::xep0511::is_link_metadata_element(payload)
                    && !waddle_xmpp::xep::xep0447::is_file_sharing_element(payload)
            }),
            "the shadow snapshot must not include server-stamped link preview metadata"
        );
    }
}

#[cfg(test)]
#[path = "../../../../ingress/invitation_replay_tests.rs"]
mod ingress_invitation_replay_tests;
