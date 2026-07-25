//! XMPP-native membership gate for XEP-0272 Muji-bearing Jingle IQ
//! requests.
//!
//! The pure IQ dispatcher
//! (`waddle_xmpp::protocol::handlers::jingle::JingleHandler`) cannot
//! itself enforce MUC membership — it is sans-I/O and has no handle
//! to the room registry. Without an upstream check, any
//! authenticated client could send a Muji `session-initiate` for an
//! arbitrary room JID (guessed, leaked, or scraped from another
//! room) and have the server mint a LiveKit JWT scoped to that
//! room. The JWT lets the attacker connect to the SFU room directly
//! even if the MUC affiliation/role rules would have rejected a
//! regular XEP-0045 join.
//!
//! This gate runs immediately before sans-I/O IQ dispatch on the
//! `urn:xmpp:jingle:1` namespace WHEN the embedded `<jingle/>`
//! carries a `<muji xmlns='urn:xmpp:jingle:muji:0'/>` child. For
//! `session-initiate` it asks the room actor whether `full_jid` is
//! a current occupant; if not, the gate short-circuits with
//! `<forbidden/>` and the SFU is never touched.
//! `session-terminate` is intentionally not gated — `unregister`
//! on the SFU is idempotent and harmless, and an over-eager leave
//! from a non-occupant is benign cleanup.
//!
//! Replaces the legacy `muc_call_gate.rs` (which gated the
//! `urn:waddle:muc-call:0` `<request-join>` IQ surface).

use jid::{BareJid, FullJid};
use waddle_xmpp::muc::room_actor::GetOccupantByJid;
use waddle_xmpp::telemetry::attributes::{CallSignalEvent, SfuDenialReason};
use waddle_xmpp::xep::xep0166::NS_JINGLE;
use waddle_xmpp::xep::xep0272::{find_muji, Muji};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jingle::{Action, Jingle};
use xmpp_parsers::stanza_error::StanzaError;

use super::super::super::call_signaling_telemetry::{
    record_call_signal, record_sfu_token_denial, CallSignalTarget,
};
use super::super::super::cleanup::get_room_actor_result;
use super::super::super::WebSocketState;
use super::errors::{bad_request_iq_error, forbidden_iq_error, internal_server_error_iq_error};

/// Outcome of the Muji Jingle membership gate.
///
/// `Allow` means the IQ should continue to the sans-I/O dispatcher;
/// `Deny` carries a typed `StanzaError` the caller must serialize
/// into the IQ-error reply frame.
pub(super) enum GateOutcome {
    Allow,
    Deny(Box<StanzaError>),
}

/// Run the membership pre-check for a Muji-bearing Jingle IQ.
///
/// Returns:
/// - `Allow` if the IQ is not a Muji-bearing `<jingle/>` (other
///   Jingle traffic — 1:1 calls — passes through unchanged), or if
///   the action is something other than `session-initiate`
///   (terminate/info are idempotent on the SFU), or if the caller
///   is a current occupant of the requested room.
/// - `Deny(forbidden)` if the IQ is a Muji `session-initiate` but
///   the caller is not a current occupant of the room.
/// - `Deny(bad_request)` if the `<muji room='…'/>` attribute is
///   missing or not a valid bare JID.
pub(super) async fn verify_muji_jingle_request(
    state: &WebSocketState,
    full_jid: &FullJid,
    iq: &Iq,
) -> GateOutcome {
    let Iq::Set { payload, .. } = iq else {
        return GateOutcome::Allow;
    };
    // Only Jingle IQs are interesting; let everything else through.
    if payload.ns() != NS_JINGLE || payload.name() != "jingle" {
        return GateOutcome::Allow;
    }
    // Without a `<muji/>` child this is a 1:1 Jingle — not our
    // gate.
    let Some(muji_elem) = find_muji(payload) else {
        return GateOutcome::Allow;
    };
    // Parse the Muji shape so we can read the room JID. Failure
    // here is a wire-shape error returned as `<bad-request/>`.
    let muji = match Muji::try_from(muji_elem) {
        Ok(m) => m,
        Err(_) => {
            return GateOutcome::Deny(Box::new(bad_request_iq_error(
                "Muji <muji/> child inside <jingle/> failed to parse",
            )));
        }
    };
    let Some(room_jid) = muji.room else {
        return GateOutcome::Deny(Box::new(bad_request_iq_error(
            "Muji <muji/> inside <jingle/> requires the 'room' attribute",
        )));
    };

    // Parse the surrounding Jingle to extract the action — we only
    // gate session-initiate; terminate is idempotent.
    let jingle = match Jingle::try_from(payload.clone()) {
        Ok(j) => j,
        Err(_) => {
            return GateOutcome::Deny(Box::new(bad_request_iq_error("malformed <jingle/> stanza")));
        }
    };
    let user = full_jid.to_bare();
    match jingle.action {
        Action::SessionInitiate => record_call_signal(
            CallSignalEvent::MujiJoin,
            &user,
            Some(CallSignalTarget::Room(&room_jid)),
        ),
        Action::SessionTerminate => record_call_signal(
            CallSignalEvent::MujiLeave,
            &user,
            Some(CallSignalTarget::Room(&room_jid)),
        ),
        _ => {}
    }
    if !matches!(jingle.action, Action::SessionInitiate) {
        return GateOutcome::Allow;
    }

    verify_room_membership(state, full_jid, &room_jid).await
}

/// When `iq` is a Muji-bearing Jingle `session-terminate`, return the
/// target MUC room JID so the websocket adapter can mirror the
/// LiveKit webhook's Muji-clear broadcast after the sans-I/O handler
/// unregisters the SFU participant.
pub(super) fn muji_session_terminate_room(iq: &Iq) -> Option<BareJid> {
    let Iq::Set { payload, .. } = iq else {
        return None;
    };
    if payload.ns() != NS_JINGLE || payload.name() != "jingle" {
        return None;
    }
    let muji_elem = find_muji(payload)?;
    let muji = Muji::try_from(muji_elem).ok()?;
    let room_jid = muji.room?;
    let jingle = Jingle::try_from(payload.clone()).ok()?;
    if !matches!(jingle.action, Action::SessionTerminate) {
        return None;
    }
    Some(room_jid)
}

async fn verify_room_membership(
    state: &WebSocketState,
    full_jid: &FullJid,
    room_jid: &BareJid,
) -> GateOutcome {
    // No room actor → the user definitively hasn't joined the room
    // through the XEP-0045 path. (The room actor is created on
    // first MUC join; absence means "no one has joined this room
    // in this process's lifetime.")
    let actor = match get_room_actor_result(state, room_jid).await {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            record_sfu_token_denial(room_jid, &full_jid.to_bare(), SfuDenialReason::RoomNotFound);
            return GateOutcome::Deny(Box::new(forbidden_iq_error(
                "not an occupant of the requested room — join the MUC first",
            )));
        }
        Err(error) => {
            tracing::debug!(room = %room_jid, error = %error, "Muji gate room lookup failed");
            record_sfu_token_denial(
                room_jid,
                &full_jid.to_bare(),
                SfuDenialReason::InternalError,
            );
            // Telemetry records the true cause, but the wire error
            // stays `forbidden` — the exact shape this path produced
            // before the telemetry work; changing client-visible
            // retry semantics is out of scope here.
            return GateOutcome::Deny(Box::new(forbidden_iq_error(
                "not an occupant of the requested room — join the MUC first",
            )));
        }
    };

    match actor
        .ask(GetOccupantByJid {
            jid: full_jid.clone(),
        })
        .await
    {
        Ok(Some(_occupant)) => GateOutcome::Allow,
        Ok(None) => {
            record_sfu_token_denial(
                room_jid,
                &full_jid.to_bare(),
                SfuDenialReason::MembershipDenied,
            );
            GateOutcome::Deny(Box::new(forbidden_iq_error(
                "not an occupant of the requested room — join the MUC first",
            )))
        }
        Err(_) => {
            record_sfu_token_denial(
                room_jid,
                &full_jid.to_bare(),
                SfuDenialReason::InternalError,
            );
            // Actor ask flaked — this is a transient server-side
            // failure, not an authorization decision. RFC 6120
            // §8.3.3 maps "the server could not process the
            // request because of a misconfiguration or other
            // transient problem" to `<internal-server-error/>`
            // with `type='wait'`, which is also the conventional
            // retry hint to the client.
            GateOutcome::Deny(Box::new(internal_server_error_iq_error(
                "could not verify MUC membership; please retry",
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use std::io;
    use std::sync::{Arc, Mutex};
    use waddle_xmpp::muc::room_actor::Join;
    use waddle_xmpp::muc::room_registry_actor::CreateInstantRoom;
    use waddle_xmpp::xep::xep0167::MediaKind;
    use waddle_xmpp::xep::xep0272::{Creator, MujiContent};
    use waddle_xmpp_core::{Affiliation, Role};
    use xmpp_parsers::jingle::{Content, ContentId, Creator as JingleCreator, SessionId};
    use xmpp_parsers::stanza_error::DefinedCondition;

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn muji_audio() -> Muji {
        Muji {
            room: Some("general@muc.example.com".parse().expect("valid bare jid")),
            preparing: false,
            contents: vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )],
        }
    }

    fn build_jingle_muji_iq(action: Action, sid: &str) -> Iq {
        let mut content = Content::new(JingleCreator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            waddle_xmpp::xep::xep0167::opus_audio_description(),
        ));
        content.transport = Some(xmpp_parsers::jingle::Transport::Unknown(
            waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport::Request
                .to_element(),
        ));
        let mut jingle = Jingle::new(action, SessionId(sid.into()));
        jingle.initiator = Some(
            "alice@example.com"
                .parse()
                .expect("valid bare jid for initiator"),
        );
        jingle.contents.push(content);
        let mut elem: xmpp_parsers::minidom::Element = jingle.into();
        elem.append_child(muji_audio().to_element());
        Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("calls.example.com".parse().expect("valid mixer jid")),
            id: "j1".into(),
            payload: elem,
        }
    }

    async fn create_room_and_join(
        state: &WebSocketState,
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

    #[tokio::test]
    async fn allow_when_caller_is_current_occupant() {
        let state = create_test_websocket_state().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice = full("alice@example.com/web");
        create_room_and_join(&state, &room, "alice", &alice).await;

        let outcome = verify_muji_jingle_request(
            &state,
            &alice,
            &build_jingle_muji_iq(Action::SessionInitiate, "general@muc.example.com"),
        )
        .await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }

    #[tokio::test]
    async fn deny_when_caller_is_not_an_occupant() {
        let state = create_test_websocket_state().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice = full("alice@example.com/web");
        let mallory = full("mallory@example.com/laptop");
        create_room_and_join(&state, &room, "alice", &alice).await;

        let outcome = verify_muji_jingle_request(
            &state,
            &mallory,
            &build_jingle_muji_iq(Action::SessionInitiate, "general@muc.example.com"),
        )
        .await;
        let GateOutcome::Deny(err) = outcome else {
            panic!("expected Deny");
        };
        assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn membership_denial_emits_warn_with_bare_jid_and_counter() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::WARN)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let state = create_test_websocket_state().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice = full("alice@example.com/web");
        let mallory = full("mallory@example.com/laptop");
        create_room_and_join(&state, &room, "alice", &alice).await;

        let outcome = verify_muji_jingle_request(
            &state,
            &mallory,
            &build_jingle_muji_iq(Action::SessionInitiate, "general@muc.example.com"),
        )
        .await;

        assert!(matches!(outcome, GateOutcome::Deny(_)));
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.sfu_token.denied",
                &[("reason", "membership_denied")]
            ),
            Some(1)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.sfu_token.denied"),
            Some("1".to_string())
        );
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(logs.contains("general@muc.example.com"), "{logs}");
        assert!(logs.contains("mallory@example.com"), "{logs}");
        assert!(logs.contains("membership_denied"), "{logs}");
        assert!(logs.contains("\"level\":\"WARN\""), "{logs}");
        assert!(logs.contains("SFU token request denied"), "{logs}");
        assert!(!logs.contains("mallory@example.com/laptop"), "{logs}");
    }

    /// #1452: a gate denial is a *complete* call-setup attempt — the
    /// sans-I/O Jingle handler never runs, so the gate must count the
    /// attempted/failed pair itself or `room_not_found` would show up
    /// as failures with no denominator.
    #[tokio::test(flavor = "current_thread")]
    async fn room_not_found_denial_records_a_call_setup_failure() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");
        let iq = ghost_room_session_initiate_iq();

        let outcome = verify_muji_jingle_request(&state, &alice, &iq).await;

        assert!(matches!(outcome, GateOutcome::Deny(_)));
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.failed", &[("reason", "room_not_found")]),
            Some(1)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.setup.failed"),
            Some("1".to_string())
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn membership_denial_records_a_call_setup_failure() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice = full("alice@example.com/web");
        let mallory = full("mallory@example.com/laptop");
        create_room_and_join(&state, &room, "alice", &alice).await;

        let outcome = verify_muji_jingle_request(
            &state,
            &mallory,
            &build_jingle_muji_iq(Action::SessionInitiate, "general@muc.example.com"),
        )
        .await;

        assert!(matches!(outcome, GateOutcome::Deny(_)));
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "membership_denied")]
            ),
            Some(1)
        );
    }

    /// The denial log line must carry the bounded, non-PII correlation
    /// id derived from the room name — the join key the client Faro
    /// events and the LiveKit webhook logs also carry (#1452).
    #[tokio::test(flavor = "current_thread")]
    async fn denial_log_carries_the_call_correlation_id_and_not_the_room_hash_preimage() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::WARN)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        verify_muji_jingle_request(&state, &alice, &ghost_room_session_initiate_iq()).await;

        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        let expected = waddle_sfu::CallCorrelationId::for_room_name(GHOST_ROOM).to_string();
        assert_eq!(expected.len(), waddle_sfu::CORRELATION_ID_HEX_LEN);
        assert!(logs.contains(&expected), "{logs}");
    }

    /// A Muji `session-initiate` whose single `<muji room='…'/>` names
    /// a room no actor has ever created, so the gate takes the
    /// `RoomNotFound` branch. Built from scratch rather than restamped
    /// on top of [`build_jingle_muji_iq`], which would leave two
    /// `<muji/>` children and have `find_muji` read the first one.
    fn ghost_room_session_initiate_iq() -> Iq {
        let mut content = Content::new(JingleCreator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            waddle_xmpp::xep::xep0167::opus_audio_description(),
        ));
        content.transport = Some(xmpp_parsers::jingle::Transport::Unknown(
            waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport::Request
                .to_element(),
        ));
        let mut jingle = Jingle::new(
            Action::SessionInitiate,
            SessionId("ghost-room-initiate".into()),
        );
        jingle.initiator = Some("alice@example.com".parse().expect("valid bare jid"));
        jingle.contents.push(content);
        let mut elem: xmpp_parsers::minidom::Element = jingle.into();
        elem.append_child(
            Muji {
                room: Some(GHOST_ROOM.parse().expect("valid bare jid")),
                preparing: false,
                contents: muji_audio().contents,
            }
            .to_element(),
        );
        Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("calls.example.com".parse().expect("valid mixer jid")),
            id: "ghost-1".into(),
            payload: elem,
        }
    }

    /// A MUC room JID that no test ever creates a room actor for.
    const GHOST_ROOM: &str = "ghost-room@muc.example.com";

    #[tokio::test]
    async fn deny_when_room_actor_does_not_exist() {
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let mut iq = build_jingle_muji_iq(Action::SessionInitiate, "ghost-room@muc.example.com");
        // Re-stamp the embedded `<muji>` so its `room` attr points
        // at the same ghost room.
        if let Iq::Set { payload, .. } = &mut iq {
            payload.children().for_each(drop);
            payload.append_child(
                Muji {
                    room: Some("ghost-room@muc.example.com".parse().unwrap()),
                    preparing: false,
                    contents: muji_audio().contents,
                }
                .to_element(),
            );
        }
        let outcome = verify_muji_jingle_request(&state, &alice, &iq).await;
        let GateOutcome::Deny(err) = outcome else {
            panic!("expected Deny");
        };
        assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
    }

    #[tokio::test]
    async fn allow_for_session_terminate_without_membership_check() {
        // session-terminate is idempotent on the SFU; the gate must
        // let it through even from a non-occupant so a stuck client
        // can explicitly tell the server it has hung up.
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let outcome = verify_muji_jingle_request(
            &state,
            &alice,
            &build_jingle_muji_iq(Action::SessionTerminate, "general@muc.example.com"),
        )
        .await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }

    #[tokio::test]
    async fn allow_for_non_jingle_namespace() {
        // The dispatcher's namespace fan-out invokes this gate on
        // every IQ; non-Jingle payloads must pass through. Defends
        // in depth in case the caller forgets to gate the fan-out.
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let payload = xmpp_parsers::minidom::Element::builder(
            "query",
            "http://jabber.org/protocol/disco#info",
        )
        .build();
        let iq = Iq::Get {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("example.com".parse().expect("valid domain jid")),
            id: "d1".into(),
            payload,
        };
        let outcome = verify_muji_jingle_request(&state, &alice, &iq).await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }

    #[tokio::test]
    async fn allow_for_jingle_without_muji_child() {
        // 1:1 Jingle (no Muji) must NOT be gated — those go
        // straight to the JingleHandler's forwarding path.
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let mut content = Content::new(JingleCreator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            waddle_xmpp::xep::xep0167::opus_audio_description(),
        ));
        content.transport = Some(xmpp_parsers::jingle::Transport::Unknown(
            waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport::Request
                .to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle.initiator = Some("alice@example.com".parse().expect("valid bare jid"));
        jingle.contents.push(content);
        let payload: xmpp_parsers::minidom::Element = jingle.into();
        let iq = Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("bob@example.com/desktop".parse().expect("valid full jid")),
            id: "p2p".into(),
            payload,
        };
        let outcome = verify_muji_jingle_request(&state, &alice, &iq).await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }

    #[test]
    fn muji_session_terminate_room_extracts_target_room() {
        let room: BareJid = "general@muc.example.com".parse().expect("valid room");
        let iq = build_jingle_muji_iq(Action::SessionTerminate, "test-sid-terminate");
        assert_eq!(super::muji_session_terminate_room(&iq), Some(room));
    }

    #[test]
    fn muji_session_terminate_room_ignores_session_initiate() {
        let iq = build_jingle_muji_iq(Action::SessionInitiate, "test-sid-initiate");
        assert_eq!(super::muji_session_terminate_room(&iq), None);
    }
}
