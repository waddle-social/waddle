//! XMPP-native membership gate for `urn:waddle:muc-call:0` requests.
//!
//! The pure IQ dispatcher (`waddle_xmpp::protocol::handlers::muc_call::MucCallHandler`)
//! cannot itself enforce MUC membership — it is sans-I/O and has no
//! handle to the room registry. Without an upstream check, any
//! authenticated client could send `request-join` for an arbitrary
//! room JID (guessed, leaked, or scraped from another room) and have
//! the server mint a LiveKit JWT scoped to that room. The JWT lets the
//! attacker connect to the SFU room directly even if the MUC
//! affiliation/role rules would have rejected a regular XEP-0045 join.
//!
//! This gate runs immediately before sans-I/O IQ dispatch on the
//! `urn:waddle:muc-call:0` namespace. For `request-join` it asks the
//! room actor whether `full_jid` is a current occupant; if not, the
//! gate short-circuits with `<forbidden/>` and the SFU is never
//! touched. `request-leave` is intentionally not gated — `unregister`
//! on the SFU is idempotent and harmless, and an over-eager leave from
//! a non-occupant is benign cleanup.
//!
//! The gate is intentionally minimal: it consults the room actor (the
//! authoritative source of MUC occupancy) directly, with no caching.
//! A stale occupancy snapshot would let a recently-departed user mint
//! a fresh JWT for a few seconds; consulting the actor on every join
//! request closes that window.

use jid::{BareJid, FullJid};
use waddle_xmpp::muc::room_actor::GetOccupantByJid;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::StanzaError;

use super::super::super::cleanup::get_room_actor;
use super::super::super::WebSocketState;
use super::errors::{bad_request_iq_error, forbidden_iq_error, internal_server_error_iq_error};

const NS_WADDLE_MUC_CALL: &str = "urn:waddle:muc-call:0";
const REQUEST_JOIN: &str = "request-join";
const ATTR_ROOM: &str = "room";

/// Outcome of the muc-call membership gate.
///
/// `Allow` means the IQ should continue to the sans-I/O dispatcher;
/// `Deny` carries a typed `StanzaError` the caller must serialize into
/// the IQ-error reply frame.
pub(super) enum GateOutcome {
    Allow,
    Deny(Box<StanzaError>),
}

/// Run the membership pre-check for a `urn:waddle:muc-call:0` IQ.
///
/// Returns:
/// - `Allow` if the IQ is not a `request-join` (other operations are
///   either passed through or idempotent), or if the caller is a
///   current occupant of the requested room.
/// - `Deny(forbidden)` if the IQ is a `request-join` but the caller
///   is not a current occupant of the room.
/// - `Deny(bad_request)` if the `room` attribute is missing or not a
///   valid bare JID.
pub(super) async fn verify_muc_call_request(
    state: &WebSocketState,
    full_jid: &FullJid,
    iq: &Iq,
) -> GateOutcome {
    // Only `request-join` mints tokens; other muc-call operations
    // (request-leave, future variants) either don't need the gate or
    // are handled inside the dispatcher's payload validation.
    let Iq::Set { payload, .. } = iq else {
        return GateOutcome::Allow;
    };
    if payload.ns() != NS_WADDLE_MUC_CALL || payload.name() != REQUEST_JOIN {
        return GateOutcome::Allow;
    }

    let Some(room_attr) = payload.attr(ATTR_ROOM) else {
        return GateOutcome::Deny(Box::new(bad_request_iq_error(
            "request-join missing 'room' attribute",
        )));
    };
    let Ok(room_jid) = room_attr.parse::<BareJid>() else {
        return GateOutcome::Deny(Box::new(bad_request_iq_error(
            "request-join 'room' attribute is not a bare JID",
        )));
    };

    // No room actor → the user definitively hasn't joined the room
    // through the XEP-0045 path. (The room actor is created on first
    // MUC join; absence means "no one has joined this room in this
    // process's lifetime.")
    let Some(actor) = get_room_actor(state, &room_jid).await else {
        return GateOutcome::Deny(Box::new(forbidden_iq_error(
            "not an occupant of the requested room — join the MUC first",
        )));
    };

    match actor
        .ask(GetOccupantByJid {
            jid: full_jid.clone(),
        })
        .await
    {
        Ok(Some(_occupant)) => GateOutcome::Allow,
        Ok(None) => GateOutcome::Deny(Box::new(forbidden_iq_error(
            "not an occupant of the requested room — join the MUC first",
        ))),
        Err(_) => {
            // Actor ask flaked — this is a transient server-side
            // failure, not an authorization decision. RFC 6120
            // §8.3.3 maps "the server could not process the request
            // because of a misconfiguration or other transient
            // problem" to `<internal-server-error/>` with
            // `type='wait'`, which is also the conventional retry
            // hint to the client. Returning `forbidden` here would
            // mis-classify a transient failure as an entitlement
            // decision and make retries cosmetically wrong.
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
    use waddle_xmpp::muc::room_actor::Join;
    use waddle_xmpp::muc::room_registry_actor::CreateInstantRoom;
    use waddle_xmpp_core::{Affiliation, Role};
    use xmpp_parsers::minidom::Element;
    use xmpp_parsers::stanza_error::DefinedCondition;

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn room_attr_name() -> minidom::rxml::NcName {
        minidom::rxml::xml_ncname!("room").to_owned()
    }

    fn request_join_iq(room: &str) -> Iq {
        let payload = Element::builder("request-join", NS_WADDLE_MUC_CALL)
            .attr(room_attr_name(), room)
            .build();
        Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some(room.parse().expect("valid bare jid")),
            id: "j1".into(),
            payload,
        }
    }

    fn request_leave_iq(room: &str) -> Iq {
        let payload = Element::builder("request-leave", NS_WADDLE_MUC_CALL)
            .attr(room_attr_name(), room)
            .build();
        Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some(room.parse().expect("valid bare jid")),
            id: "l1".into(),
            payload,
        }
    }

    /// Helper: create + join a MUC room so the gate's occupancy
    /// lookup returns `Some`. Mirrors the join shape `handle_muc_join`
    /// uses in production.
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
            .expect("create instant room");
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

        let outcome =
            verify_muc_call_request(&state, &alice, &request_join_iq("general@muc.example.com"))
                .await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }

    #[tokio::test]
    async fn deny_after_caller_has_left_the_room() {
        // Regression guard for the leave→gate coupling: a session
        // that was a valid occupant at one point in time but has
        // since left must NOT be able to mint a fresh JWT for the
        // room. Without the gate consulting the live actor on
        // every request-join, a stale snapshot would let a recently
        // departed user keep minting tokens.
        use waddle_xmpp::muc::room_actor::LeaveByRealJid;
        let state = create_test_websocket_state().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice = full("alice@example.com/web");
        create_room_and_join(&state, &room, "alice", &alice).await;

        // Sanity: alice is in the room, gate allows.
        let outcome =
            verify_muc_call_request(&state, &alice, &request_join_iq("general@muc.example.com"))
                .await;
        assert!(matches!(outcome, GateOutcome::Allow));

        // alice leaves.
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("room ask")
            .expect("room actor present");
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
            })
            .await
            .expect("leave");

        // After leave, the gate must deny.
        let outcome =
            verify_muc_call_request(&state, &alice, &request_join_iq("general@muc.example.com"))
                .await;
        let GateOutcome::Deny(err) = outcome else {
            panic!("expected Deny after leaving the room");
        };
        assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
    }

    #[tokio::test]
    async fn deny_when_caller_is_not_an_occupant() {
        // Room exists, alice joined, but the call request comes from
        // mallory who never joined — must be rejected even though the
        // room itself is "real".
        let state = create_test_websocket_state().await;
        let room: BareJid = "general@muc.example.com".parse().unwrap();
        let alice = full("alice@example.com/web");
        let mallory = full("mallory@example.com/laptop");
        create_room_and_join(&state, &room, "alice", &alice).await;

        let outcome = verify_muc_call_request(
            &state,
            &mallory,
            &request_join_iq("general@muc.example.com"),
        )
        .await;
        let GateOutcome::Deny(err) = outcome else {
            panic!("expected Deny");
        };
        assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
    }

    #[tokio::test]
    async fn deny_when_room_actor_does_not_exist() {
        // Room JID looks plausible but no actor was ever created —
        // means no one has joined this room in this process. The
        // gate denies rather than ask-failing.
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let outcome = verify_muc_call_request(
            &state,
            &alice,
            &request_join_iq("ghost-room@muc.example.com"),
        )
        .await;
        let GateOutcome::Deny(err) = outcome else {
            panic!("expected Deny");
        };
        assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
    }

    #[tokio::test]
    async fn allow_for_request_leave_without_membership_check() {
        // request-leave is idempotent on the SFU; the gate must let
        // it through even from a non-occupant so a stuck client can
        // explicitly tell the server it has hung up.
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let outcome =
            verify_muc_call_request(&state, &alice, &request_leave_iq("general@muc.example.com"))
                .await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }

    #[tokio::test]
    async fn allow_for_non_set_iq() {
        // The dispatcher itself rejects non-`set` muc-call IQs with
        // BadRequest; the gate has nothing to add for those.
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let payload = Element::builder("request-join", NS_WADDLE_MUC_CALL)
            .attr(room_attr_name(), "general@muc.example.com")
            .build();
        let iq = Iq::Get {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("general@muc.example.com".parse().expect("valid bare jid")),
            id: "g1".into(),
            payload,
        };
        let outcome = verify_muc_call_request(&state, &alice, &iq).await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }

    #[tokio::test]
    async fn bad_request_when_room_attribute_missing() {
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let payload = Element::builder("request-join", NS_WADDLE_MUC_CALL).build();
        let iq = Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("muc.example.com".parse().expect("valid domain jid")),
            id: "j2".into(),
            payload,
        };
        let outcome = verify_muc_call_request(&state, &alice, &iq).await;
        let GateOutcome::Deny(err) = outcome else {
            panic!("expected Deny");
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    // The dedicated "room is not a bare JID" branch returns BadRequest
    // when `parse::<BareJid>()` fails outright. The `jid` crate is
    // quite lenient with the bare-JID grammar (it will treat unusual
    // strings as domain-only JIDs), so for any input that does parse
    // but doesn't correspond to a real room the gate falls through to
    // the room-actor lookup — which then returns Forbidden via
    // `deny_when_room_actor_does_not_exist` above. Both paths share a
    // user-facing meaning ("you may not call here"), and routing
    // unparseable-but-not-rejected values to Forbidden is acceptable
    // defense in depth, so no separate happy-path BadRequest test is
    // needed beyond `bad_request_when_room_attribute_missing`.

    #[tokio::test]
    async fn allow_when_payload_namespace_is_not_muc_call() {
        // The caller in `handle_sans_io_iq` only invokes the gate
        // when `payload_ns == "urn:waddle:muc-call:0"`, but the gate
        // itself defends in depth: a non-muc-call namespace falls
        // through to Allow.
        let state = create_test_websocket_state().await;
        let alice = full("alice@example.com/web");

        let payload = Element::builder("request-join", "urn:xmpp:ping").build();
        let iq = Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("muc.example.com".parse().expect("valid domain jid")),
            id: "j4".into(),
            payload,
        };
        let outcome = verify_muc_call_request(&state, &alice, &iq).await;
        assert!(matches!(outcome, GateOutcome::Allow));
    }
}
