//! Owner-side executor for a Muji `session-initiate` or
//! `session-terminate` relayed from the replica that received it
//! (#1445).
//!
//! The producing replica had no local room actor, so it relayed the IQ
//! over the ordered MUC proxy to the node holding the room's claim —
//! this node. Here the membership gate re-runs against the local room
//! actor (which holds the occupancy), the LiveKit token is minted with
//! gate-derived capabilities, and the SFU participant is registered in
//! THIS process's registry — consolidating call state on the room
//! owner. The returned frames (the empty IQ ack and the
//! server-initiated `session-accept` carrying the token) ride back to
//! the origin node on the relay ACK and are written to the client's
//! socket there.

use super::super::super::interpret_loop::build_interpret_deps;
use super::super::super::transport_xml::build_iq_error_xml_typed;
use super::super::super::WebSocketState;
use super::jingle_muji_gate::{self, GateInvocation, GateOutcome};
use super::sans_io::events_contain_iq_error;
use super::ProtocolStanzaContext;

/// Execute a relayed Muji `session-initiate` or `session-terminate`
/// on the room-owning node.
///
/// Returns `None` only on a wire-shape failure (the IQ carries no full
/// sender JID) — the caller NACKs the envelope as a parse failure.
/// Every authorization outcome, including a denial, is `Some(frames)`:
/// a delivered IQ-error is the correct reply for the client, not a
/// relay failure, and returning it as frames prevents any re-relay
/// loop — this executor is terminal.
pub(crate) async fn handle_relayed_muji_initiate(
    state: &WebSocketState,
    iq: &xmpp_parsers::iq::Iq,
) -> Option<Vec<String>> {
    let sender = iq.from()?.clone().try_into_full().ok()?;
    let id = iq.id();
    let response_from = iq.to().map(|to| to.to_string());
    let response_to = jid::Jid::from(sender.clone()).to_string();

    let is_terminate = jingle_muji_gate::muji_session_terminate_room(iq).is_some();
    let media_capabilities = match jingle_muji_gate::verify_muji_jingle_request(
        state,
        &sender,
        iq,
        GateInvocation::RelayedReplay,
    )
    .await
    {
        GateOutcome::Allow { media_capabilities } => media_capabilities,
        GateOutcome::Deny(error) => {
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from.as_deref(),
                Some(response_to.as_str()),
                *error,
            )]);
        }
        // We hold (or held) the room's claim yet no actor lives here:
        // every occupant has left and the actor is gone, or ownership
        // moved after the producer's claim read.
        //
        // A terminate MUST still execute. The ordinary way to reach
        // this arm is two occupants leaving at once — the first
        // empties the room and tears the actor down, the second
        // arrives to find nothing. The departing participant may well
        // still hold an SFU registration and a live LiveKit session
        // here, and only running the handler unregisters them and
        // fires `RemoveParticipant`. Synthesizing a bare IQ result
        // instead would tell the client the call ended while leaving
        // their media session connected until the reconcile sweep
        // eventually noticed. This mirrors the MUC-leave path, which
        // deliberately tears the SFU participant down even when the
        // room actor is already gone (`handlers/presence/muc.rs`).
        // Terminate is never membership-gated, so executing it with no
        // derived capabilities is exactly what the local path does.
        GateOutcome::RoomNotLocal { room_jid } if is_terminate => {
            let _ = room_jid;
            None
        }
        // An initiate, though, genuinely cannot proceed: the requester
        // is not an occupant of a live local room. Terminal denial,
        // never a re-relay.
        GateOutcome::RoomNotLocal { room_jid } => {
            let error = jingle_muji_gate::deny_room_not_found(&room_jid, &sender.to_bare());
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from.as_deref(),
                Some(response_to.as_str()),
                *error,
            )]);
        }
    };

    let ctx = ProtocolStanzaContext {
        domain: state.deps.auth_state.xmpp_domain.as_str(),
        full_jid: &sender,
        media_capabilities,
    };
    let muji_terminate_room = jingle_muji_gate::muji_session_terminate_room(iq);
    let events = state.deps.protocol.dispatcher.dispatch_iq(iq, &ctx);
    let clear_after = muji_terminate_room.filter(|_| !events_contain_iq_error(&events));
    let session = synthetic_session(&sender);
    let deps = build_interpret_deps(state, Some(&session));
    let outcome = crate::server::routes::interpret::interpret(events, &deps).await;
    // Mirror the local adapter's post-terminate Muji-presence clear. It
    // has to run HERE, on the owner, because that is where the room
    // actor and the SFU registration both live — running it on the
    // client's replica would find neither.
    if let Some(room_jid) = clear_after {
        crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
            state, &room_jid, &sender,
        )
        .await;
    }
    Some(outcome.frames)
}

/// Minimal synthetic session for interpret-side owner checks, same
/// shape the other reserved MUC-proxy deliveries use.
fn synthetic_session(sender: &jid::FullJid) -> crate::auth::Session {
    let bare = sender.to_bare();
    let localpart = bare
        .node()
        .map(|node| node.to_string())
        .unwrap_or_else(|| bare.to_string());
    crate::auth::Session::new(
        bare.to_string().as_str(),
        localpart.as_str(),
        localpart.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_calls;
    use jid::{BareJid, FullJid};
    use waddle_xmpp::muc::room_actor::Join;
    use waddle_xmpp::muc::room_registry_actor::CreateInstantRoom;
    use waddle_xmpp::xep::xep0167::MediaKind;
    use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};
    use waddle_xmpp_core::{Affiliation, Role};
    use xmpp_parsers::iq::Iq;
    use xmpp_parsers::jingle::{
        Action, Content, ContentId, Creator as JingleCreator, Jingle, SessionId,
    };

    fn muji_initiate_iq(from: Option<&str>, room: &str) -> Iq {
        let mut content = Content::new(JingleCreator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            waddle_xmpp::xep::xep0167::opus_audio_description(),
        ));
        content.transport = Some(xmpp_parsers::jingle::Transport::Unknown(
            waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport::Request
                .to_element(),
        ));
        // No `initiator` attribute: XEP-0166 §7.1 makes it optional
        // and the handler resolves an omitted value to the
        // authenticated session (a present value must equal the FULL
        // sender JID, which varies per test case here).
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("relay-sid".into()));
        jingle.contents.push(content);
        let mut elem: xmpp_parsers::minidom::Element = jingle.into();
        elem.append_child(
            Muji {
                room: Some(room.parse().expect("valid room jid")),
                preparing: false,
                contents: vec![MujiContent::new(
                    "audio",
                    Creator::Initiator,
                    MediaKind::Audio,
                )],
            }
            .to_element(),
        );
        Iq::Set {
            from: from.map(|f| f.parse().expect("valid from jid")),
            to: Some("calls.example.com".parse().expect("valid mixer jid")),
            id: "relay-1".into(),
            payload: elem,
        }
    }

    async fn create_room_and_join(
        state: &crate::server::routes::websocket::WebSocketState,
        room: &BareJid,
        nick: &str,
        jid: &FullJid,
    ) {
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateInstantRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("create instant room")
            .actor_ref;
        actor
            .ask(Join {
                nick: nick.to_string(),
                real_jid: jid.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");
    }

    /// The relayed mint on the owner: gate passes against the local
    /// room actor and the reply frames carry the IQ ack plus the
    /// focus's `session-accept` with an issued token — exactly what
    /// rides back to the origin node as client replies.
    #[tokio::test]
    async fn occupant_relayed_initiate_mints_and_returns_ack_and_accept() {
        let state = create_test_websocket_state_with_calls().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        create_room_and_join(&state, &room, "alice", &alice).await;

        let iq = muji_initiate_iq(Some("alice@example.com/web"), "general@muc.example.com");
        let frames = handle_relayed_muji_initiate(&state, &iq)
            .await
            .expect("well-formed relayed IQ executes");

        assert!(
            frames
                .iter()
                .any(|f| f.contains("type='result'") || f.contains("type=\"result\"")),
            "the IQ ack must be among the replies: {frames:?}"
        );
        assert!(
            frames
                .iter()
                .any(|f| f.contains("session-accept") && f.contains("<token")),
            "the focus session-accept with an issued token must be among the replies: {frames:?}"
        );
    }

    fn muji_terminate_iq(from: &str, room: &str) -> Iq {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("relay-sid".into()));
        let mut elem: xmpp_parsers::minidom::Element = jingle.into();
        elem.append_child(
            Muji {
                room: Some(room.parse().expect("valid room jid")),
                preparing: false,
                contents: vec![],
            }
            .to_element(),
        );
        Iq::Set {
            from: Some(from.parse().expect("valid from jid")),
            to: Some("calls.example.com".parse().expect("valid mixer jid")),
            id: "relay-term-1".into(),
            payload: elem,
        }
    }

    /// #1445: an initiate registers the participant on the ROOM OWNER,
    /// so the matching terminate has to unregister on that same node.
    /// Executing it on the client's own replica would clear nothing
    /// here, leaving a phantom in-call participant that also keeps the
    /// call non-empty and so suppresses `DeleteRoom` for everyone
    /// else. Asserted against the owner's SFU registry directly.
    #[tokio::test]
    async fn relayed_terminate_unregisters_on_the_owner() {
        let state = create_test_websocket_state_with_calls().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        create_room_and_join(&state, &room, "alice", &alice).await;
        let sfu = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("the calls fixture wires an SFU");
        let call = waddle_sfu::CallId::new(room.to_string()).expect("room JID is a valid call id");
        let identity = waddle_sfu::Identity::from_jid(alice.clone());

        handle_relayed_muji_initiate(
            &state,
            &muji_initiate_iq(Some("alice@example.com/web"), "general@muc.example.com"),
        )
        .await
        .expect("relayed initiate executes");
        assert!(
            sfu.has_call_participant(&call, &identity),
            "the relayed initiate must register the participant on this node"
        );

        handle_relayed_muji_initiate(
            &state,
            &muji_terminate_iq("alice@example.com/web", "general@muc.example.com"),
        )
        .await
        .expect("relayed terminate executes");
        assert!(
            !sfu.has_call_participant(&call, &identity),
            "the relayed terminate must unregister on the node holding the registration"
        );
    }

    /// A hangup must never fail, and must actually tear the media
    /// session down. Two occupants leaving at once is the ordinary way
    /// to reach the owner with no room actor left: the first empties
    /// the room and tears the actor down, the second finds nothing.
    ///
    /// The regression this pins: synthesizing a bare IQ result for
    /// that case told the client the call had ended while never
    /// running the handler — so the participant kept their SFU
    /// registration and their live LiveKit session until the reconcile
    /// sweep eventually noticed. Registration state, not the reply
    /// shape, is the assertion that matters here.
    #[tokio::test]
    async fn relayed_terminate_tears_down_media_even_when_the_room_actor_is_gone() {
        let state = create_test_websocket_state_with_calls().await;
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        let sfu = state
            .deps
            .protocol
            .sfu
            .as_ref()
            .expect("the calls fixture wires an SFU");
        // A registration with NO room actor for its room — exactly the
        // state a departing occupant is left in when the last MUC leave
        // tore the actor down before their Jingle terminate arrived.
        let call = waddle_sfu::CallId::new("vanished@muc.example.com").expect("valid call id");
        let identity = waddle_sfu::Identity::from_jid(alice.clone());
        sfu.register_call_participant(&call, &identity);
        assert!(sfu.has_call_participant(&call, &identity));

        let frames = handle_relayed_muji_initiate(
            &state,
            &muji_terminate_iq("alice@example.com/web", "vanished@muc.example.com"),
        )
        .await
        .expect("terminate executes");

        assert!(
            !sfu.has_call_participant(&call, &identity),
            "the hangup must unregister the participant even with no room actor; \
             acking without executing would strand their LiveKit session"
        );
        assert!(
            frames
                .iter()
                .any(|f| f.contains("type='result'") || f.contains("type=\"result\"")),
            "and the client must still be told the hangup succeeded: {frames:?}"
        );
    }

    /// A non-occupant relayed to the owner is denied as a delivered
    /// IQ-error frame — never a relay NACK, so no re-relay loop.
    #[tokio::test]
    async fn non_occupant_relayed_initiate_returns_forbidden_frame() {
        let state = create_test_websocket_state_with_calls().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        create_room_and_join(&state, &room, "alice", &alice).await;

        let iq = muji_initiate_iq(
            Some("mallory@example.com/laptop"),
            "general@muc.example.com",
        );
        let frames = handle_relayed_muji_initiate(&state, &iq)
            .await
            .expect("a denial is a delivered reply, not a shape failure");

        assert_eq!(frames.len(), 1, "exactly the IQ error: {frames:?}");
        assert!(
            frames[0].contains("<forbidden") && frames[0].contains("relay-1"),
            "denial must be the forbidden IQ error for the original id: {}",
            frames[0]
        );
    }

    /// Ownership moved (or the room died) after the producer's claim
    /// read: no local actor here either. Terminal denial, no re-relay.
    #[tokio::test]
    async fn missing_room_on_owner_returns_terminal_room_not_found_frame() {
        let state = create_test_websocket_state_with_calls().await;

        let iq = muji_initiate_iq(Some("alice@example.com/web"), "ghost@muc.example.com");
        let frames = handle_relayed_muji_initiate(&state, &iq)
            .await
            .expect("terminal denial is a delivered reply");

        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("<forbidden"), "{}", frames[0]);
    }

    /// No full sender JID is a wire-shape failure: the caller NACKs
    /// the envelope instead of replying.
    #[tokio::test]
    async fn bare_or_missing_sender_is_a_shape_failure() {
        let state = create_test_websocket_state_with_calls().await;

        let no_from = muji_initiate_iq(None, "general@muc.example.com");
        assert!(handle_relayed_muji_initiate(&state, &no_from)
            .await
            .is_none());

        let bare_from = muji_initiate_iq(Some("alice@example.com"), "general@muc.example.com");
        assert!(handle_relayed_muji_initiate(&state, &bare_from)
            .await
            .is_none());
    }
}
