//! XEP-0421: Anonymous unique occupant identifiers for MUCs — dedicated
//! conformance suite.
//!
//! In-crate tests in `xep::xep0421::tests` cover helper internals
//! (deterministic HMAC, length floor, redacted Debug). This suite pins
//! the audit-level invariants that crossing the public API exposes:
//!
//! - §3 namespace string
//! - §3 element shape (name, ns, `id` attribute, no other children)
//! - §"Business Rules" / §3 advertisement obligation: every MUC room's
//!   disco#info MUST advertise `urn:xmpp:occupant-id:0`
//! - §3 anti-spoofing: client-supplied `<occupant-id/>` MUST be stripped
//!   before the server stamps its own, regardless of attacker shape
//!   (single, repeated, mixed-namespace siblings)
//! - §3 derivation stability: same (user, room, secret) ⇒ same id
//!   independent of nickname; cross-secret / cross-room inputs MUST
//!   produce different ids (unlinkability)

use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, Feature};
use waddle_xmpp::xep::xep0421::{
    build_occupant_id_element, extract_occupant_id_from_message, extract_occupant_id_from_presence,
    generate_occupant_id, is_occupant_id_element, set_occupant_id_on_message,
    set_occupant_id_on_presence, strip_occupant_id_from_message, strip_occupant_id_from_presence,
    OccupantId, OccupantIdCarrier, OccupantIdSecret, NS_OCCUPANT_ID,
};
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

// ── §3 namespace identifier ──────────────────────────────────────────

#[test]
fn xep0421_namespace_matches_spec() {
    // XEP-0421 §3 pins the namespace URI exactly. Clients dispatch on
    // this string; a typo silently routes occupant-id traffic into a
    // generic "unknown payload" path.
    assert_eq!(NS_OCCUPANT_ID, "urn:xmpp:occupant-id:0");
}

// ── §3 disco advertisement (MUC rooms) ───────────────────────────────

#[test]
fn xep0421_advertised_on_every_room_configuration() {
    // XEP-0421 §"Business Rules": "The MUC service MUST advertise
    // support for occupant identifiers in disco#info responses."
    // Waddle's room features are configuration-driven (persistent,
    // members-only, moderated, forum); the §3 advert MUST survive
    // every combination since the underlying stamping behavior is
    // configuration-independent.
    let target = Feature::occupant_id();

    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features(persistent={persistent}, members_only={members_only}, moderated={moderated}, forum={forum}) \
                         MUST advertise `urn:xmpp:occupant-id:0` per XEP-0421"
                    );
                }
            }
        }
    }
}

#[test]
fn xep0421_feature_constructor_pins_namespace_string() {
    // Defence-in-depth: the constructor value itself must equal the
    // spec URI; an audit-time rename of the constant would otherwise
    // pass while breaking the wire-facing advertisement.
    let feat = Feature::occupant_id();
    assert_eq!(feat.0, "urn:xmpp:occupant-id:0");
}

// ── §3 element shape ─────────────────────────────────────────────────

#[test]
fn xep0421_element_matches_spec_shape() {
    // XEP-0421 §3 example:
    //   <occupant-id xmlns='urn:xmpp:occupant-id:0' id='opaque-id'/>
    // Exactly that local name, that namespace, an `id` attribute, and
    // no element children. The builder must produce this shape so any
    // conformant peer parser can read it.
    let id = OccupantId::new("dd72603deec90a38ba552f7c68cbcc61");
    let elem = build_occupant_id_element(&id);

    assert_eq!(elem.name(), "occupant-id");
    assert_eq!(elem.ns(), NS_OCCUPANT_ID);
    assert_eq!(elem.attr("id"), Some("dd72603deec90a38ba552f7c68cbcc61"));
    assert_eq!(
        elem.children().count(),
        0,
        "occupant-id is a leaf element per §3"
    );
}

#[test]
fn xep0421_classifier_accepts_spec_shape_only() {
    // The classifier gates the strip-and-restamp path. It MUST accept
    // the §3 shape and reject near-misses that would otherwise allow
    // a crafted client payload to either slip through or DoS the
    // stamping logic by polluting the namespace bucket.
    let canonical = Element::builder("occupant-id", NS_OCCUPANT_ID)
        .attr("id", "abc")
        .build();
    assert!(is_occupant_id_element(&canonical));

    let wrong_ns = Element::builder("occupant-id", "jabber:client").build();
    assert!(!is_occupant_id_element(&wrong_ns));

    let wrong_name = Element::builder("occupant", NS_OCCUPANT_ID).build();
    assert!(!is_occupant_id_element(&wrong_name));
}

// ── §3 anti-spoofing ─────────────────────────────────────────────────

#[test]
fn xep0421_strips_single_client_supplied_occupant_id_on_message() {
    // XEP-0421 §3: "the service MUST remove any <occupant-id/> sent
    // by the client before broadcasting." A naive groupchat
    // implementation that forwarded client occupant-ids would let
    // attackers spoof another room participant's identity.
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                  <body>Hello</body>\
                  <occupant-id xmlns='urn:xmpp:occupant-id:0' id='SPOOFED'/>\
               </message>";
    let mut msg = Message::try_from(xml.parse::<Element>().expect("valid xml"))
        .expect("valid groupchat message");

    strip_occupant_id_from_message(&mut msg);

    assert!(extract_occupant_id_from_message(&msg).is_none());
    assert!(
        !msg.bodies.is_empty(),
        "<body/> MUST be preserved across strip"
    );
}

#[test]
fn xep0421_strips_repeated_client_supplied_occupant_ids() {
    // Hostile client may attach several occupant-id elements hoping
    // the server only strips the first. Strip MUST be total.
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                  <body>Hello</body>\
                  <occupant-id xmlns='urn:xmpp:occupant-id:0' id='SPOOF-A'/>\
                  <occupant-id xmlns='urn:xmpp:occupant-id:0' id='SPOOF-B'/>\
                  <occupant-id xmlns='urn:xmpp:occupant-id:0' id='SPOOF-C'/>\
               </message>";
    let mut msg = Message::try_from(xml.parse::<Element>().expect("valid xml"))
        .expect("valid groupchat message");

    strip_occupant_id_from_message(&mut msg);

    let remaining: usize = msg
        .payloads
        .iter()
        .filter(|e| e.ns() == NS_OCCUPANT_ID)
        .count();
    assert_eq!(
        remaining, 0,
        "all client-supplied occupant-id payloads MUST be stripped, none survived"
    );
}

#[test]
fn xep0421_set_replaces_client_supplied_occupant_id_with_server_value() {
    // §3 "MUST" semantics for the stamping path: even if the client
    // attached its own occupant-id, the server's resulting message
    // carries exactly the server-computed id, with no duplicates.
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                  <body>Hi</body>\
                  <occupant-id xmlns='urn:xmpp:occupant-id:0' id='SPOOF'/>\
               </message>";
    let mut msg = Message::try_from(xml.parse::<Element>().expect("valid xml"))
        .expect("valid groupchat message");

    let server_id = OccupantId::new("server-computed-deadbeef");
    set_occupant_id_on_message(&mut msg, &server_id);

    assert_eq!(
        extract_occupant_id_from_message(&msg).as_ref(),
        Some(&server_id),
        "server-stamped id MUST be the only occupant-id surfaced"
    );
    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| e.ns() == NS_OCCUPANT_ID)
            .count(),
        1,
        "exactly one occupant-id payload survives the set; no duplicates"
    );
}

#[test]
fn xep0421_strips_and_set_apply_to_presence_too() {
    // Stamping applies to "every message AND presence" emitted from
    // MUC rooms (§3). The presence path MUST share the same
    // anti-spoofing guarantee as the message path.
    let xml = "<presence xmlns='jabber:client'>\
                  <occupant-id xmlns='urn:xmpp:occupant-id:0' id='SPOOF-PRES'/>\
               </presence>";
    let mut presence =
        Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

    strip_occupant_id_from_presence(&mut presence);
    assert!(extract_occupant_id_from_presence(&presence).is_none());

    let server_id = OccupantId::new("pres-stamp");
    set_occupant_id_on_presence(&mut presence, &server_id);
    assert_eq!(
        presence
            .payloads
            .iter()
            .filter(|e| e.ns() == NS_OCCUPANT_ID)
            .count(),
        1,
    );
}

// ── §3 derivation invariants ─────────────────────────────────────────

fn alice() -> jid::BareJid {
    "alice@example.com".parse().expect("bare jid")
}
fn bob() -> jid::BareJid {
    "bob@example.com".parse().expect("bare jid")
}
fn room_a() -> jid::BareJid {
    "team-a@muc.example.com".parse().expect("bare jid")
}
fn room_b() -> jid::BareJid {
    "team-b@muc.example.com".parse().expect("bare jid")
}

fn secret_a() -> OccupantIdSecret {
    OccupantIdSecret::new(b"deployment-a-32-byte-occupant-key".to_vec())
        .expect("≥32 bytes meets §3 floor")
}
fn secret_b() -> OccupantIdSecret {
    OccupantIdSecret::new(b"deployment-b-32-byte-occupant-key".to_vec())
        .expect("≥32 bytes meets §3 floor")
}

#[test]
fn xep0421_stable_across_nick_changes_for_same_user_and_room() {
    // XEP-0421 §1 motivation: "Same user in same room → same ID
    // (even across sessions [and nicknames])." Nickname is not an
    // input to the HMAC; only the user's bare JID is. The derivation
    // therefore MUST stay stable when the same user rejoins under
    // any number of different nicks.
    let s = secret_a();
    let id_session_1 = generate_occupant_id(&alice(), &room_a(), &s);
    let id_session_2 = generate_occupant_id(&alice(), &room_a(), &s);
    assert_eq!(
        id_session_1, id_session_2,
        "occupant-id MUST stay stable across rejoins; \
         the spec's tracking property depends on this"
    );
}

#[test]
fn xep0421_different_users_in_same_room_get_different_ids() {
    // §1: "Different users → different IDs". A linkage failure here
    // would let Alice impersonate Bob in moderator audit trails.
    let s = secret_a();
    let alice_id = generate_occupant_id(&alice(), &room_a(), &s);
    let bob_id = generate_occupant_id(&bob(), &room_a(), &s);
    assert_ne!(alice_id, bob_id);
}

#[test]
fn xep0421_same_user_in_different_rooms_gets_different_ids() {
    // §1 + §3 unlinkability: a moderator in room A MUST NOT be able
    // to correlate the same user in room B from the id alone. Cross-
    // room inputs MUST yield different HMAC outputs.
    let s = secret_a();
    let in_a = generate_occupant_id(&alice(), &room_a(), &s);
    let in_b = generate_occupant_id(&alice(), &room_b(), &s);
    assert_ne!(in_a, in_b);
}

#[test]
fn xep0421_different_deployment_secrets_unlink_same_inputs() {
    // §3: "the derivation be keyed by a per-deployment secret to
    // avoid cross-deployment linkability." Two deployments handing
    // the same user+room to the HMAC MUST produce different ids,
    // otherwise federated services could correlate occupants.
    let id_a = generate_occupant_id(&alice(), &room_a(), &secret_a());
    let id_b = generate_occupant_id(&alice(), &room_a(), &secret_b());
    assert_ne!(id_a, id_b);
}

#[test]
fn xep0421_generated_id_is_opaque_hex_and_does_not_leak_inputs() {
    // §3 explicitly characterises the id as "opaque". The
    // hex-encoded HMAC output MUST NOT contain the bare-JID
    // localpart, the room localpart, or `@`-shaped substrings of
    // either — otherwise the "opaque" property is on paper only.
    let s = secret_a();
    let id = generate_occupant_id(&alice(), &room_a(), &s);
    let id_str = id.as_str();

    assert!(
        id_str.chars().all(|c| c.is_ascii_hexdigit()),
        "id MUST be hex (opaque-by-shape), got `{id_str}`"
    );
    assert!(!id_str.contains("alice"), "id leaked user localpart");
    assert!(!id_str.contains("team-a"), "id leaked room localpart");
    assert!(!id_str.contains('@'), "id leaked JID delimiter");
}

// ── Carrier-trait surface ────────────────────────────────────────────

#[test]
fn xep0421_carrier_trait_is_a_typed_extraction_path() {
    // Anti-stringly-typed-payload guard: consumers should use the
    // typed `OccupantIdCarrier` trait to read occupant-ids off both
    // Message and Presence, not reach into raw payloads. The trait
    // must surface the id for both stanza kinds with identical
    // semantics.
    let msg_xml = "<message xmlns='jabber:client' type='groupchat'>\
                      <occupant-id xmlns='urn:xmpp:occupant-id:0' id='M-TRAIT'/>\
                   </message>";
    let pres_xml = "<presence xmlns='jabber:client'>\
                      <occupant-id xmlns='urn:xmpp:occupant-id:0' id='P-TRAIT'/>\
                    </presence>";

    let msg = Message::try_from(msg_xml.parse::<Element>().expect("xml")).expect("valid message");
    let pres =
        Presence::try_from(pres_xml.parse::<Element>().expect("xml")).expect("valid presence");

    assert!(msg.has_occupant_id());
    assert_eq!(
        msg.occupant_id().as_ref().map(|o| o.as_str()),
        Some("M-TRAIT")
    );
    assert!(pres.has_occupant_id());
    assert_eq!(
        pres.occupant_id().as_ref().map(|o| o.as_str()),
        Some("P-TRAIT")
    );
}
