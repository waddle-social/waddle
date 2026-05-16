//! XEP-0317: Hats — dedicated conformance test suite.
//!
//! Hats are descriptive social metadata, not authority. XEP-0317 §1
//! frames the layer explicitly as a separate concept from standard
//! MUC roles, motivated by use cases like presenter, scribe, teacher,
//! teacher's assistant, comms officer, incident manager, or online-
//! game role — extended descriptive metadata "beyond" the affiliation/
//! role set MUC already provides.
//!
//! Authority lives in XEP-0045's `<x xmlns='http://jabber.org/protocol/muc#user'>
//! <item affiliation='…' role='…'/>`. Hats live in `<hats xmlns='urn:xmpp:hats:0'>`
//! and a hat URI like `urn:xmpp:hats:bot` MUST NOT grant any MUC
//! capability — clients render hats; servers enforce nothing on them.
//!
//! Waddle no longer auto-derives hats from MUC affiliation or role.
//! Owner/admin/moderator status is conveyed once, by the XEP-0045
//! payload that every join/update/leave presence already carries.
//! Hats are reserved for genuinely descriptive metadata the server
//! adds out-of-band (today: the extension-bot path).

use jid::{BareJid, FullJid};
use minidom::Element;
use waddle_xmpp::muc::presence::{
    build_occupant_presence, build_occupant_presence_update, NS_MUC_USER,
};
use waddle_xmpp::xep::xep0317::{
    build_hats_element, extract_hats_from_presence, has_hats, parse_hats_element, set_hats,
    well_known, Hat, HatSet, NS_HATS,
};
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OccupantIdentity};
use waddle_xmpp::{Affiliation, Role};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const ROOM: &str = "physicsforpoets@courses.example.edu";
const NICK: &str = "Steve";
const REAL_JID: &str = "steve@example.edu/tablet";

fn room_jid() -> FullJid {
    FullJid::new(&format!("{ROOM}/{NICK}")).expect("room jid")
}

fn real_jid() -> FullJid {
    FullJid::new(REAL_JID).expect("real jid")
}

fn bare(jid: &FullJid) -> BareJid {
    jid.to_bare()
}

fn secret() -> OccupantIdSecret {
    OccupantIdSecret::new([0u8; 32].to_vec()).expect("32-byte secret")
}

fn identity<'a>(
    bare_jid: &'a BareJid,
    real: Option<&'a FullJid>,
    secret: &'a OccupantIdSecret,
) -> OccupantIdentity<'a> {
    OccupantIdentity {
        bare_jid,
        real_jid: real,
        secret,
    }
}

fn item_element(presence: &Presence) -> Element {
    presence
        .payloads
        .iter()
        .find(|el| el.is("x", NS_MUC_USER))
        .expect("presence carries <x muc#user>")
        .children()
        .find(|el| el.name() == "item")
        .cloned()
        .expect("<x muc#user> carries <item/>")
}

fn join_presence(affiliation: Affiliation, role: Role) -> Presence {
    let from = room_jid();
    let to = real_jid();
    let bare_jid = bare(&from);
    let secret = secret();
    let identity = identity(&bare_jid, Some(&to), &secret);
    build_occupant_presence(&from, &to, affiliation, role, false, &identity)
}

fn update_presence(seed: Presence, affiliation: Affiliation, role: Role) -> Presence {
    let from = room_jid();
    let to = real_jid();
    let bare_jid = bare(&from);
    let secret = secret();
    let identity = identity(&bare_jid, Some(&to), &secret);
    build_occupant_presence_update(&seed, &from, &to, affiliation, role, false, &identity)
}

// ── XEP-0045 authority remains carried by `<x muc#user>` ─────────────

#[test]
fn xep_0317_owner_join_presence_carries_affiliation_in_xep_0045_payload() {
    let presence = join_presence(Affiliation::Owner, Role::Moderator);

    let item = item_element(&presence);
    assert_eq!(item.attr("affiliation"), Some("owner"));
    assert_eq!(item.attr("role"), Some("moderator"));
}

#[test]
fn xep_0317_admin_join_presence_carries_affiliation_in_xep_0045_payload() {
    let presence = join_presence(Affiliation::Admin, Role::Moderator);

    let item = item_element(&presence);
    assert_eq!(item.attr("affiliation"), Some("admin"));
    assert_eq!(item.attr("role"), Some("moderator"));
}

#[test]
fn xep_0317_member_join_presence_carries_affiliation_in_xep_0045_payload() {
    let presence = join_presence(Affiliation::Member, Role::Participant);

    let item = item_element(&presence);
    assert_eq!(item.attr("affiliation"), Some("member"));
    assert_eq!(item.attr("role"), Some("participant"));
}

// ── XEP-0317 separation: hats are NOT derived from MUC authority ────

#[test]
fn xep_0317_owner_join_presence_does_not_synthesise_an_owner_hat() {
    let presence = join_presence(Affiliation::Owner, Role::Moderator);
    assert!(
        !has_hats(&presence),
        "owner authority belongs in <x muc#user>; <hats> must be absent unless an \
         out-of-band descriptive hat has been assigned. Presence: {presence:?}"
    );
}

#[test]
fn xep_0317_admin_join_presence_does_not_synthesise_an_admin_hat() {
    let presence = join_presence(Affiliation::Admin, Role::Moderator);
    assert!(
        !has_hats(&presence),
        "admin authority belongs in <x muc#user>; <hats> must be absent unless an \
         out-of-band descriptive hat has been assigned"
    );
}

#[test]
fn xep_0317_moderator_role_alone_does_not_synthesise_a_moderator_hat() {
    // role=Moderator with affiliation=Member is the "promoted moderator"
    // case. The old implementation slapped a Moderator hat on this
    // presence — confusing the runtime role layer with the descriptive
    // hat layer. The new implementation must not.
    let presence = join_presence(Affiliation::Member, Role::Moderator);
    assert!(
        !has_hats(&presence),
        "moderator role belongs in <x muc#user>; <hats> must be absent unless an \
         out-of-band descriptive hat has been assigned"
    );
}

#[test]
fn xep_0317_member_join_presence_carries_no_hats() {
    let presence = join_presence(Affiliation::Member, Role::Participant);
    assert!(!has_hats(&presence));
}

#[test]
fn xep_0317_presence_update_does_not_synthesise_hats_from_authority() {
    // Even when an incoming presence is rebuilt server-side (e.g. a
    // mid-session status update), authority MUST NOT bleed into <hats>.
    let seed = Presence::new(PresenceType::None);
    let presence = update_presence(seed, Affiliation::Owner, Role::Moderator);

    let item = item_element(&presence);
    assert_eq!(item.attr("affiliation"), Some("owner"));
    assert_eq!(item.attr("role"), Some("moderator"));
    assert!(!has_hats(&presence));
}

// ── Descriptive-hat assignment still works via the public API ───────

#[test]
fn xep_0317_descriptive_hat_assigned_via_set_hats_round_trips_through_the_wire() {
    // A descriptive hat — e.g. "Bot" — is assigned by an out-of-band
    // mechanism (today: the extension-bot path). It travels in
    // <hats xmlns='urn:xmpp:hats:0'> as XEP-0317 specifies, alongside
    // any XEP-0045 authority payload.
    let mut presence = join_presence(Affiliation::None, Role::Participant);
    let descriptive = HatSet::new().with_hat(Hat::bot());
    set_hats(&mut presence, &descriptive);

    let extracted = extract_hats_from_presence(&presence).expect("descriptive hat is carried");
    assert_eq!(extracted.len(), 1);
    assert!(extracted.is_bot());
}

#[test]
fn xep_0317_emitted_hats_element_uses_canonical_namespace_and_shape() {
    // §3 wire shape: <hats xmlns='urn:xmpp:hats:0'> carrying one or
    // more <hat/> children, each in the same namespace.
    let set = HatSet::new()
        .with_hat(Hat::bot())
        .with_hat(Hat::new("Speaker", "urn:example:speaker"));
    let elem = build_hats_element(&set);

    assert_eq!(elem.name(), "hats");
    assert_eq!(elem.ns(), NS_HATS, "container namespace pins to {NS_HATS}");

    let children: Vec<&Element> = elem.children().collect();
    assert_eq!(children.len(), 2);
    for child in &children {
        assert_eq!(child.name(), "hat");
        assert_eq!(
            child.ns(),
            NS_HATS,
            "<hat/> children share the container's namespace per the §3 examples"
        );
    }

    // Round-trip back through the parser to confirm both directions
    // honour the canonical shape.
    let parsed = parse_hats_element(&elem);
    assert_eq!(parsed.len(), 2);
    assert!(parsed.has_uri(well_known::BOT));
    assert!(parsed.has_uri("urn:example:speaker"));
}

#[test]
fn xep_0317_co_existing_authority_and_descriptive_hats_are_carried_independently() {
    // A bot might also be an admin in the room. The wire payload must
    // carry BOTH layers: affiliation=admin in <x muc#user>, and Bot in
    // <hats>. The hat does not duplicate or replace the affiliation.
    let mut presence = join_presence(Affiliation::Admin, Role::Moderator);
    set_hats(&mut presence, &HatSet::new().with_hat(Hat::bot()));

    let item = item_element(&presence);
    assert_eq!(item.attr("affiliation"), Some("admin"));
    assert_eq!(item.attr("role"), Some("moderator"));

    let hats = extract_hats_from_presence(&presence).expect("descriptive hat carried");
    assert_eq!(hats.len(), 1);
    assert!(hats.is_bot());
}
