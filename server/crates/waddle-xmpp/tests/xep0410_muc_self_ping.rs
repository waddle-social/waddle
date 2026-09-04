//! XEP-0410: MUC Self-Ping (Schrödinger's Chat) — dedicated test suite.
//!
//! Spec: https://xmpp.org/extensions/xep-0410.html (v1.1.0, Draft).
//!
//! This file is the CLAUDE.md "every implemented XEP MUST have a
//! dedicated Rust custom test suite" coverage for XEP-0410. The server-
//! side optimization (the MUC IQ handler that short-circuits a joined
//! self-ping with `result` and a non-joined sender with `not-acceptable`)
//! is exercised end-to-end through the websocket integration suite at
//! `server/crates/waddle-server/tests/xep0054_0049_0191_ws.rs`. This
//! file pins the **client-facing helper library** under
//! `waddle_xmpp::xep::xep0410`: the IQ builder, the receive-side
//! classifier, and the response-interpretation switch the spec
//! mandates in §2.2.

use minidom::Element;
use waddle_xmpp::xep::xep0410::{
    build_self_ping, interpret_self_ping_response, is_self_ping, SelfPingResult,
    FEATURE_MUC_SELFPING, PING_TIMEOUT_SECS, RECOMMENDED_INTERVAL_SECS,
};
use xmpp_parsers::{
    iq::Iq,
    stanza_error::{DefinedCondition, ErrorType, StanzaError},
};

const NS_PING: &str = "urn:xmpp:ping";

fn occupant_jid() -> jid::Jid {
    "room@muc.example.com/mynick"
        .parse()
        .expect("valid occupant jid")
}

fn error_iq(condition: DefinedCondition) -> Iq {
    Iq::Error {
        from: Some("room@muc.example.com/mynick".parse().expect("valid")),
        to: Some("juliet@example.com/client".parse().expect("valid")),
        id: "sp-err".to_owned(),
        error: StanzaError::new(ErrorType::Cancel, condition, "en", ""),
        payload: None,
    }
}

// ── Feature advertisement & spec constants ─────────────────────────────

#[test]
fn xep_0410_feature_string_matches_spec_exactly() {
    // XEP-0410 §3 + Registrar §7: the disco#info feature on individual
    // MUC room JIDs that signals server-side optimization support.
    assert_eq!(
        FEATURE_MUC_SELFPING,
        "http://jabber.org/protocol/muc#self-ping-optimization"
    );
}

#[test]
fn xep_0410_recommended_constants_match_spec_intent() {
    // §2.2 "After an adequate amount of silence from a given MUC (e.g.
    // 15 minutes), or from all MUCs from a given service domain, a
    // client should initiate a self-ping." The 60 s default in the
    // helper library is below that 15-minute threshold; clients that
    // want longer can override.
    assert_eq!(RECOMMENDED_INTERVAL_SECS, 60);
    // The spec doesn't fix a timeout, but every joined-occupant ping
    // either reflects to a peer or short-circuits via the optimization
    // — both bounded in single-digit seconds in practice.
    assert_eq!(PING_TIMEOUT_SECS, 10);
}

// ── §2.2 client-side builder ──────────────────────────────────────────

#[test]
fn xep_0410_build_self_ping_emits_canonical_iq_get_shape() {
    let iq = build_self_ping(None::<jid::Jid>, occupant_jid(), "sp-1");
    assert_eq!(iq.id(), "sp-1");
    assert_eq!(iq.to().cloned(), Some(occupant_jid()));
    // Payload is `<ping xmlns='urn:xmpp:ping'/>` inside an `iq type='get'`,
    // matching the §2.2 canonical example.
    let Iq::Get { payload: elem, .. } = &iq else {
        panic!("self-ping must be type='get'");
    };
    assert_eq!(elem.name(), "ping");
    assert_eq!(elem.ns(), NS_PING);
    assert!(
        elem.children().next().is_none(),
        "the <ping/> element has no children per XEP-0199 §3.1"
    );
}

#[test]
fn xep_0410_build_self_ping_round_trips_explicit_from_jid() {
    let from: jid::Jid = "juliet@example.com/laptop".parse().unwrap();
    let iq = build_self_ping(Some(from.clone()), occupant_jid(), "sp-2");
    assert_eq!(iq.from().cloned(), Some(from));
}

// ── §2.2 receive-side classifier (`is_self_ping`) ─────────────────────

#[test]
fn xep_0410_is_self_ping_accepts_ping_to_full_occupant_jid() {
    let iq = build_self_ping(None::<jid::Jid>, occupant_jid(), "sp-1");
    assert!(
        is_self_ping(&iq),
        "an IQ-get with <ping/> destined for a full MUC occupant JID is a self-ping"
    );
}

#[test]
fn xep_0410_is_self_ping_rejects_bare_jid_destination() {
    // A ping to the room's bare JID is a regular server / domain ping,
    // not a self-ping. Spec scope is occupant-JID destinations only.
    let bare: jid::Jid = "room@muc.example.com".parse().expect("valid bare jid");
    let ping_elem = Element::builder("ping", NS_PING).build();
    let iq = Iq::Get {
        from: None,
        to: Some(bare),
        id: "sp-bare".to_owned(),
        payload: ping_elem,
    };
    assert!(!is_self_ping(&iq));
}

#[test]
fn xep_0410_is_self_ping_rejects_non_ping_payloads() {
    // A roster-query IQ to a full JID is NOT a self-ping.
    let other_elem = Element::builder("query", "jabber:iq:roster").build();
    let iq = Iq::Get {
        from: None,
        to: Some(occupant_jid()),
        id: "rq-1".to_owned(),
        payload: other_elem,
    };
    assert!(!is_self_ping(&iq));
}

#[test]
fn xep_0410_is_self_ping_rejects_ping_in_wrong_namespace() {
    let elem = Element::builder("ping", "urn:example:not-ping").build();
    let iq = Iq::Get {
        from: None,
        to: Some(occupant_jid()),
        id: "wrongns".to_owned(),
        payload: elem,
    };
    assert!(!is_self_ping(&iq));
}

// ── §2.2 response interpretation (the canonical 5-way switch) ─────────
//
// The spec enumerates the responses a client must distinguish:
//   - Successful IQ response → still joined
//   - service-unavailable / feature-not-implemented → joined,
//     but the pinged client doesn't implement XEP-0199 — treat
//     as Connected (the routing was reflected, so we ARE joined)
//   - item-not-found → joined, but the occupant nick changed
//     mid-session — treat as Connected
//   - remote-server-not-found / remote-server-timeout → no
//     decision available; treat as Timeout
//   - Any other error → probably not joined; rejoin
//   - Timeout (no response) → caller-driven (handled outside this
//     classifier).

#[test]
fn xep_0410_iq_result_interpreted_as_connected() {
    let iq = Iq::Result {
        from: Some(occupant_jid()),
        to: None,
        id: "sp-1".to_owned(),
        payload: None,
    };
    assert_eq!(interpret_self_ping_response(&iq), SelfPingResult::Connected);
}

#[test]
fn xep_0410_not_acceptable_error_interpreted_as_disconnected() {
    // §2.2 explicitly: "<not-acceptable> — the client is not joined
    // any more. It should perform a re-join."
    assert_eq!(
        interpret_self_ping_response(&error_iq(DefinedCondition::NotAcceptable)),
        SelfPingResult::Disconnected
    );
}

#[test]
fn xep_0410_service_unavailable_means_joined_but_peer_lacks_ping() {
    // §2.2: "<service-unavailable> or <feature-not-implemented> — the
    // client is joined, but the pinged client does not implement
    // XEP-0199." Treat as Connected because the routing reached a
    // co-occupant.
    assert_eq!(
        interpret_self_ping_response(&error_iq(DefinedCondition::ServiceUnavailable)),
        SelfPingResult::Connected
    );
    assert_eq!(
        interpret_self_ping_response(&error_iq(DefinedCondition::FeatureNotImplemented)),
        SelfPingResult::Connected
    );
}

#[test]
fn xep_0410_item_not_found_means_joined_mid_nick_change() {
    // §2.2: "<item-not-found> — the client is joined, but the
    // occupant just changed their name (e.g. initiated by a different
    // client)."
    assert_eq!(
        interpret_self_ping_response(&error_iq(DefinedCondition::ItemNotFound)),
        SelfPingResult::Connected
    );
}

#[test]
fn xep_0410_remote_server_errors_are_indeterminate_timeouts() {
    // §2.2: "<remote-server-not-found> or <remote-server-timeout> —
    // the remote server is unreachable for unspecified reasons; this
    // can be a temporary network failure or a server outage. No
    // decision can be made based on this; Treat like a timeout."
    assert_eq!(
        interpret_self_ping_response(&error_iq(DefinedCondition::RemoteServerNotFound)),
        SelfPingResult::Timeout
    );
    assert_eq!(
        interpret_self_ping_response(&error_iq(DefinedCondition::RemoteServerTimeout)),
        SelfPingResult::Timeout
    );
}

#[test]
fn xep_0410_unrecognised_errors_fall_to_disconnected_per_spec_guidance() {
    // §2.2: "Any other error — the client is probably not joined any
    // more. It should perform a re-join."
    //
    // Spec notes that real implementations send `not-acceptable`,
    // `not-allowed`, or `bad-request` for this case. Confirm all three
    // are treated as Disconnected.
    for condition in [
        DefinedCondition::NotAllowed,
        DefinedCondition::BadRequest,
        DefinedCondition::Forbidden,
    ] {
        assert_eq!(
            interpret_self_ping_response(&error_iq(condition.clone())),
            SelfPingResult::Disconnected,
            "{condition:?} should trigger a rejoin per spec catch-all",
        );
    }
}

#[test]
fn xep_0410_self_ping_result_should_rejoin_table() {
    // The Disconnected and Timeout cases both warrant a rejoin
    // (Timeout because the connection state is indeterminate and the
    // safe default is to assume disconnected).
    assert!(!SelfPingResult::Connected.should_rejoin());
    assert!(SelfPingResult::Disconnected.should_rejoin());
    assert!(SelfPingResult::Timeout.should_rejoin());
}

#[test]
fn xep_0410_self_ping_result_display_strings() {
    // Display form is used in logs and telemetry; pin the values so
    // log-parsing dashboards don't drift.
    assert_eq!(SelfPingResult::Connected.to_string(), "connected");
    assert_eq!(SelfPingResult::Disconnected.to_string(), "disconnected");
    assert_eq!(SelfPingResult::Timeout.to_string(), "timeout");
}

// ── §3 server optimization — RoomActor `PingSelfCheck` (#1253) ────────
//
// The server-side optimized answer is authoritative: a joined session
// MUST get an IQ result; only a not-joined session gets
// `<not-acceptable/>`. `PingSelfCheck` is the RoomActor message the
// websocket IQ handler asks; `Ok(())` maps to the IQ result and
// `Err(HandlerError)` maps to `<not-acceptable/>`.

use kameo::actor::{ActorRef, Spawn};
use waddle_xmpp::muc::room_actor::{
    GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation, PingSelfCheck, RoomActor,
};
use waddle_xmpp::muc::{MucRoom, RoomConfig};
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};
use waddle_xmpp::Affiliation;

fn spawn_room() -> ActorRef<RoomActor> {
    let room_jid: jid::BareJid = "pingroom@muc.example.com".parse().expect("room jid");
    let room = MucRoom::new(
        room_jid,
        "waddle-1".to_string(),
        "channel-1".to_string(),
        RoomConfig::default(),
    );
    let secret =
        OccupantIdSecret::new(vec![9u8; OCCUPANT_ID_SECRET_MIN_BYTES]).expect("valid secret");
    RoomActor::spawn(RoomActor::new(room, secret))
}

fn session(resource: &str) -> jid::FullJid {
    format!("alice@example.com/{resource}")
        .parse()
        .expect("full jid")
}

async fn join_as_alice(actor: &ActorRef<RoomActor>, jid: jid::FullJid) {
    let admission_revision = actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .admission_revision;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: jid,
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision,
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        })
        .await
        .expect("join");
}

/// #1253: a nick joined from multiple sessions must answer the
/// self-ping with success for EVERY joined session — not just the
/// first one recorded in `occupant.real_jid`. Pre-fix, the second
/// device's periodic self-ping returned `<not-acceptable/>` and the
/// client entered a perpetual rejoin loop.
#[tokio::test]
async fn xep_0410_self_ping_succeeds_for_every_session_of_a_multi_session_nick() {
    let actor = spawn_room();
    join_as_alice(&actor, session("web")).await;
    join_as_alice(&actor, session("mobile")).await;

    for resource in ["web", "mobile"] {
        actor
            .ask(PingSelfCheck {
                nick: "alice".to_string(),
                sender_jid: session(resource),
            })
            .await
            .unwrap_or_else(|error| {
                panic!("self-ping from joined session `{resource}` must succeed: {error:?}")
            });
    }
}

/// XEP-0410 server optimization: a session that is NOT joined under
/// the pinged nick gets the not-joined answer — including a session
/// of the same bare JID that never joined.
#[tokio::test]
async fn xep_0410_self_ping_rejects_session_not_joined_under_nick() {
    let actor = spawn_room();
    join_as_alice(&actor, session("web")).await;

    let result = actor
        .ask(PingSelfCheck {
            nick: "alice".to_string(),
            sender_jid: session("tablet-never-joined"),
        })
        .await;
    assert!(
        result.is_err(),
        "a session that never joined must be told it is not joined"
    );

    let stranger: jid::FullJid = "mallory@example.com/pc".parse().expect("jid");
    let result = actor
        .ask(PingSelfCheck {
            nick: "alice".to_string(),
            sender_jid: stranger,
        })
        .await;
    assert!(result.is_err(), "another user pinging alice's nick fails");
}

/// XEP-0410 server optimization: pinging a nick with no occupant at
/// all is a not-joined answer.
#[tokio::test]
async fn xep_0410_self_ping_rejects_unknown_nick() {
    let actor = spawn_room();
    let result = actor
        .ask(PingSelfCheck {
            nick: "ghost".to_string(),
            sender_jid: session("web"),
        })
        .await;
    assert!(result.is_err());
}

/// #1253 regression shape: after the FIRST session leaves, the
/// remaining session must still self-ping successfully (the occupant's
/// `real_jid` belonged to the departed session).
#[tokio::test]
async fn xep_0410_self_ping_survives_first_session_departure() {
    let actor = spawn_room();
    join_as_alice(&actor, session("web")).await;
    join_as_alice(&actor, session("mobile")).await;

    let outcome = actor
        .ask(waddle_xmpp::muc::room_actor::LeaveByRealJid {
            sender_jid: session("web"),
            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
            session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
            attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
            origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
        })
        .await
        .expect("leave first session");
    let waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome) = outcome else {
        panic!("web session was joined");
    };
    assert!(
        !outcome.removed_last_session,
        "mobile is still joined after web leaves"
    );

    actor
        .ask(PingSelfCheck {
            nick: "alice".to_string(),
            sender_jid: session("mobile"),
        })
        .await
        .expect("remaining session is still joined");
}
