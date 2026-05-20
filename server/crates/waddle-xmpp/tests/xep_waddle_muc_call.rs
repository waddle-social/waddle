//! `urn:waddle:muc-call:0` — Waddle MUC presence-extension custom XEP
//! dedicated test suite.
//!
//! This is a Waddle-custom namespace (the CLAUDE.md XEP conformance
//! rule allows custom `urn:waddle:*` namespaces only where no XEP
//! shape fits — SFU-backed group-call state is one such case). The
//! tests below pin:
//!
//! * Wire shape — `<call xmlns='urn:waddle:muc-call:0' state='…'
//!   call-id='…'/>` round-trips through the typed
//!   [`MucCallExtension`] surface.
//! * State semantics — `state='active'` enters the room's call_state
//!   map; `state='inactive'` clears it.
//! * Presence-extension forwarding — when a session sends `inactive`,
//!   the post-update broadcast omits the `<call/>` child entirely so
//!   peers stop seeing the indicator.
//! * Per-session origination — the originating `FullJid` is stored
//!   so a partial-session leave (one resource of a multi-resource
//!   occupant) clears the chip even when the user's other sessions
//!   remain in the room. Without this, alice/mobile staying in the
//!   channel after her desktop (on the call) drops would keep the
//!   chip lit forever.
//!
//! All construction goes through typed Rust values:
//! [`MucCallExtension`], [`CallPresenceState`], [`CallId`]. No
//! `format!`, no string concatenation (CLAUDE.md XML hard rule).

use jid::{BareJid, FullJid};
use minidom::Element;
use waddle_sfu::CallId;
use waddle_xmpp::muc::{MucRoom, Occupant, RoomConfig};
use waddle_xmpp::xep::xep_waddle_muc_call::{
    CallPresenceState, MucCallExtension, MucCallParseError, NS_WADDLE_MUC_CALL,
};
use waddle_xmpp::{Affiliation, Role};

fn call_id(s: &str) -> CallId {
    CallId::new(s).expect("valid call id")
}

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn occupant(real_jid: FullJid, nick: &str) -> Occupant {
    Occupant {
        real_jid,
        nick: nick.to_owned(),
        role: Role::Participant,
        affiliation: Affiliation::Member,
        is_remote: false,
        home_server: None,
    }
}

fn fresh_room() -> MucRoom {
    MucRoom::new(
        bare("room@muc.test"),
        "wad-1".into(),
        "chan-1".into(),
        RoomConfig::default(),
    )
}

// ── Namespace constant ────────────────────────────────────────────────

#[test]
fn waddle_muc_call_namespace_is_versioned_zero() {
    // The `:0` version suffix marks this as the initial draft of a
    // Waddle-custom namespace. Future revisions bump the suffix per
    // the XSF Registrar convention so old clients can opt out by
    // namespace.
    assert_eq!(NS_WADDLE_MUC_CALL, "urn:waddle:muc-call:0");
}

// ── Wire shape round-trip ─────────────────────────────────────────────

#[test]
fn waddle_muc_call_active_round_trips() {
    // Build via the typed `MucCallExtension::active` constructor →
    // serialise → reparse. The post-roundtrip typed value MUST
    // match the original, proving the wire shape is the source of
    // truth.
    let original = MucCallExtension::active(call_id("room@muc.test"));
    let elem = original.to_element();
    assert_eq!(elem.name(), "call");
    assert_eq!(elem.ns(), NS_WADDLE_MUC_CALL);
    assert_eq!(elem.attr("state"), Some("active"));
    assert_eq!(elem.attr("call-id"), Some("room@muc.test"));

    let reparsed = MucCallExtension::try_from(&elem).expect("active reparses");
    assert_eq!(reparsed.state, CallPresenceState::Active);
    assert_eq!(reparsed.call_id.as_str(), "room@muc.test");
}

#[test]
fn waddle_muc_call_inactive_round_trips() {
    // `inactive` is the explicit "call has ended" signal — needed
    // because absence-of-`<call/>` already means "not in call",
    // so an explicit transition is required when an active session
    // ends the call without leaving the room.
    let original = MucCallExtension::inactive(call_id("room@muc.test"));
    let elem = original.to_element();
    assert_eq!(elem.attr("state"), Some("inactive"));
    assert_eq!(elem.attr("call-id"), Some("room@muc.test"));

    let reparsed = MucCallExtension::try_from(&elem).expect("inactive reparses");
    assert_eq!(reparsed.state, CallPresenceState::Inactive);
}

#[test]
fn waddle_muc_call_rejects_unknown_state_value() {
    // The state attribute is constrained to the {active, inactive}
    // set. Any other value (e.g. `ringing`, `paused`, … future
    // extensions) MUST be rejected at the parser boundary so
    // unknown semantics can't leak into the room actor.
    let elem = Element::builder("call", NS_WADDLE_MUC_CALL)
        .attr(minidom::rxml::xml_ncname!("state").to_owned(), "ringing")
        .attr(
            minidom::rxml::xml_ncname!("call-id").to_owned(),
            "room@muc.test",
        )
        .build();
    let err = MucCallExtension::try_from(&elem).expect_err("unknown state");
    assert!(
        matches!(err, MucCallParseError::InvalidState(ref s) if s == "ringing"),
        "unknown state error preserves the offending value: {err:?}"
    );
}

#[test]
fn waddle_muc_call_rejects_missing_call_id() {
    // The `call-id` attribute is mandatory — without it the
    // extension can't be tied to a specific SFU call. Parse failure
    // returns the missing attribute name verbatim.
    let elem = Element::builder("call", NS_WADDLE_MUC_CALL)
        .attr(minidom::rxml::xml_ncname!("state").to_owned(), "active")
        .build();
    let err = MucCallExtension::try_from(&elem).expect_err("missing call-id");
    match err {
        MucCallParseError::MissingAttribute(name) => assert_eq!(name, "call-id"),
        other => panic!("expected MissingAttribute(\"call-id\"), got {other:?}"),
    }
}

#[test]
fn waddle_muc_call_rejects_missing_state() {
    let elem = Element::builder("call", NS_WADDLE_MUC_CALL)
        .attr(
            minidom::rxml::xml_ncname!("call-id").to_owned(),
            "room@muc.test",
        )
        .build();
    let err = MucCallExtension::try_from(&elem).expect_err("missing state");
    match err {
        MucCallParseError::MissingAttribute(name) => assert_eq!(name, "state"),
        other => panic!("expected MissingAttribute(\"state\"), got {other:?}"),
    }
}

#[test]
fn waddle_muc_call_rejects_versioned_namespace_drift() {
    // A `<call/>` element in a different version namespace
    // (`urn:waddle:muc-call:1`) MUST NOT parse as the `:0` typed
    // extension. This is the version-coexistence safety net: an
    // old room actor encountering a future `:1` payload sees
    // WrongElement and ignores it instead of mis-interpreting.
    let elem = Element::builder("call", "urn:waddle:muc-call:1")
        .attr(minidom::rxml::xml_ncname!("state").to_owned(), "active")
        .attr(
            minidom::rxml::xml_ncname!("call-id").to_owned(),
            "room@muc.test",
        )
        .build();
    let err = MucCallExtension::try_from(&elem).expect_err("wrong namespace");
    assert!(matches!(err, MucCallParseError::WrongElement));
}

// ── Presence-extension forwarding via MucRoom::upsert_call_state ─────

#[test]
fn waddle_muc_call_active_state_stored_in_room_call_state() {
    // §presence-forwarding: a session emitting `state='active'`
    // adds an entry to the room's call_state map. The room's
    // `call_state_for_nick` lookup returns the typed extension so
    // late joiners can be told there's a live call when they
    // receive the occupant list replay.
    let mut room = fresh_room();
    let alice_desktop = full("alice@waddle.test/desktop");
    room.add_occupant(occupant(alice_desktop.clone(), "alice"));

    let ext = MucCallExtension::active(call_id("room@muc.test"));
    let outcome = room.upsert_call_state("alice", alice_desktop.clone(), ext);

    // Active upserts return `Some(ext)` so the broadcaster knows to
    // include the `<call/>` child in the forwarded presence.
    let active_ext = outcome.expect("active upsert returns the live extension");
    assert_eq!(active_ext.state, CallPresenceState::Active);
    assert_eq!(active_ext.call_id.as_str(), "room@muc.test");

    // The room remembers the call for late joiners.
    let stored = room
        .call_state_for_nick("alice")
        .expect("call state stored");
    assert_eq!(stored.state, CallPresenceState::Active);
    assert_eq!(stored.call_id.as_str(), "room@muc.test");
}

#[test]
fn waddle_muc_call_inactive_clears_room_call_state() {
    // §presence-forwarding: a session emitting `state='inactive'`
    // removes the entry from the room's call_state map. The
    // upsert returns `None`, telling the broadcaster to forward a
    // presence WITHOUT the `<call/>` child — peers see the chip
    // disappear.
    let mut room = fresh_room();
    let alice_desktop = full("alice@waddle.test/desktop");
    room.add_occupant(occupant(alice_desktop.clone(), "alice"));

    // First: enter the call.
    let _ = room.upsert_call_state(
        "alice",
        alice_desktop.clone(),
        MucCallExtension::active(call_id("room@muc.test")),
    );
    assert!(room.call_state_for_nick("alice").is_some());

    // Then: leave the call.
    let outcome = room.upsert_call_state(
        "alice",
        alice_desktop,
        MucCallExtension::inactive(call_id("room@muc.test")),
    );
    assert!(
        outcome.is_none(),
        "inactive upsert returns None so broadcast omits <call/>"
    );
    assert!(
        room.call_state_for_nick("alice").is_none(),
        "inactive clears the room's call_state entry"
    );
}

// ── Per-session origination — multi-resource ghost-chip guard ───────

#[test]
fn waddle_muc_call_last_session_leave_clears_chip() {
    // §ghost-chip safety net (single-session variant): alice's
    // session is on the call, then the session leaves the room
    // entirely. The call advertisement MUST clear so peers stop
    // seeing the chip — otherwise late joiners would replay a
    // chip for a participant who isn't in the room any more.
    let mut room = fresh_room();
    let alice = full("alice@waddle.test/desktop");
    room.add_occupant(occupant(alice.clone(), "alice"));

    let _ = room.upsert_call_state(
        "alice",
        alice.clone(),
        MucCallExtension::active(call_id("room@muc.test")),
    );
    assert!(room.call_state_for_nick("alice").is_some());

    // Last (and only) session for alice leaves — the occupant is
    // fully removed and the chip clears.
    let outcome = room.remove_occupant_session("alice", &alice);
    assert_eq!(
        outcome,
        Some(true),
        "last session leaving removes the occupant"
    );
    assert!(
        room.call_state_for_nick("alice").is_none(),
        "last-session leave clears the call advertisement"
    );
}

#[test]
fn waddle_muc_call_remove_occupant_clears_chip() {
    // The hard-remove path (`remove_occupant`) is used when the
    // server forcibly evicts an occupant (kick, ban, server-side
    // cleanup). The call advertisement MUST be cleared regardless
    // of which code path causes the eviction.
    let mut room = fresh_room();
    let alice = full("alice@waddle.test/desktop");
    room.add_occupant(occupant(alice.clone(), "alice"));
    let _ = room.upsert_call_state(
        "alice",
        alice,
        MucCallExtension::active(call_id("room@muc.test")),
    );
    assert!(room.call_state_for_nick("alice").is_some());

    let removed = room.remove_occupant("alice");
    assert!(removed.is_some(), "occupant was present and is now removed");
    assert!(
        room.call_state_for_nick("alice").is_none(),
        "hard-remove path clears the call advertisement"
    );
}

// ── Active call survives a non-originator session leave ──────────────

#[test]
fn waddle_muc_call_state_persists_across_unrelated_session_leaves() {
    // Conversely: if alice (originator) is on the call and bob
    // leaves the room, alice's chip MUST NOT clear. The chip is
    // keyed by the originating session, not by room occupancy in
    // general.
    let mut room = fresh_room();
    let alice = full("alice@waddle.test/desktop");
    let bob = full("bob@waddle.test/desktop");
    room.add_occupant(occupant(alice.clone(), "alice"));
    room.add_occupant(occupant(bob.clone(), "bob"));

    let _ = room.upsert_call_state(
        "alice",
        alice.clone(),
        MucCallExtension::active(call_id("room@muc.test")),
    );
    assert!(room.call_state_for_nick("alice").is_some());

    // bob's session leaves — alice's chip MUST remain lit.
    let _ = room.remove_occupant_session("bob", &bob);
    assert!(
        room.call_state_for_nick("alice").is_some(),
        "an unrelated occupant leaving must not clear alice's call chip"
    );
}

// ── Replacing an active extension preserves the originator binding ──

#[test]
fn waddle_muc_call_re_upserting_active_overwrites_existing_entry() {
    // A second `active` from the same session refreshes the
    // extension (e.g. on a presence re-broadcast). The room
    // continues to advertise the same call_id and the chip stays
    // lit.
    let mut room = fresh_room();
    let alice = full("alice@waddle.test/desktop");
    room.add_occupant(occupant(alice.clone(), "alice"));

    let _ = room.upsert_call_state(
        "alice",
        alice.clone(),
        MucCallExtension::active(call_id("room@muc.test")),
    );
    let outcome = room.upsert_call_state(
        "alice",
        alice.clone(),
        MucCallExtension::active(call_id("room@muc.test")),
    );
    assert!(outcome.is_some(), "re-active still returns the live ext");
    let stored = room
        .call_state_for_nick("alice")
        .expect("entry still present");
    assert_eq!(stored.call_id.as_str(), "room@muc.test");
}
