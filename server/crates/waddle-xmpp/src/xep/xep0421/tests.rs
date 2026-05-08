use super::*;

use xmpp_parsers::message::MessageType;

fn test_secret() -> OccupantIdSecret {
    OccupantIdSecret::new(b"waddle-test-secret-key-for-occupant-ids".to_vec())
        .expect("test secret meets length floor")
}

fn alice() -> jid::BareJid {
    "alice@example.com".parse().expect("bare")
}
fn bob() -> jid::BareJid {
    "bob@example.com".parse().expect("bare")
}
fn room() -> jid::BareJid {
    "room@muc.example.com".parse().expect("bare")
}
fn room1() -> jid::BareJid {
    "room1@muc.example.com".parse().expect("bare")
}
fn room2() -> jid::BareJid {
    "room2@muc.example.com".parse().expect("bare")
}

#[test]
fn test_generate_occupant_id_deterministic() {
    let secret = test_secret();
    let id1 = generate_occupant_id(&alice(), &room(), &secret);
    let id2 = generate_occupant_id(&alice(), &room(), &secret);
    assert_eq!(id1, id2);
}

#[test]
fn test_generate_different_users() {
    let secret = test_secret();
    let alice_id = generate_occupant_id(&alice(), &room(), &secret);
    let bob_id = generate_occupant_id(&bob(), &room(), &secret);
    assert_ne!(alice_id, bob_id);
}

#[test]
fn test_generate_different_rooms() {
    let secret = test_secret();
    let r1 = generate_occupant_id(&alice(), &room1(), &secret);
    let r2 = generate_occupant_id(&alice(), &room2(), &secret);
    assert_ne!(r1, r2);
}

#[test]
fn test_generate_different_secrets_produce_different_ids() {
    // XEP-0421 §3: the deployment-keyed derivation MUST make occupant-ids
    // unlinkable across deployments using different secrets. Same (user,
    // room) inputs with different keys must yield different ids.
    let secret_a =
        OccupantIdSecret::new(b"deployment-a-secret-32-bytes-long".to_vec()).expect("≥32 bytes");
    let secret_b =
        OccupantIdSecret::new(b"deployment-b-secret-32-bytes-long".to_vec()).expect("≥32 bytes");
    let id_a = generate_occupant_id(&alice(), &room(), &secret_a);
    let id_b = generate_occupant_id(&alice(), &room(), &secret_b);
    assert_ne!(id_a, id_b);
}

#[test]
fn test_generate_id_length() {
    let secret = test_secret();
    let id = generate_occupant_id(&alice(), &room(), &secret);
    // 16 bytes = 32 hex chars
    assert_eq!(id.0.len(), 32);
}

#[test]
fn test_secret_rejects_short_input() {
    let result = OccupantIdSecret::new(b"too-short".to_vec());
    assert_eq!(
        result.unwrap_err(),
        OccupantIdSecretError::TooShort {
            got: 9,
            min: OCCUPANT_ID_SECRET_MIN_BYTES,
        }
    );
}

#[test]
fn test_secret_accepts_minimum_length() {
    let bytes = vec![0x42u8; OCCUPANT_ID_SECRET_MIN_BYTES];
    OccupantIdSecret::new(bytes).expect("32 bytes meets floor");
}

#[test]
fn test_secret_debug_redacts_bytes() {
    // Build the secret from a known marker substring. The `Debug`
    // impl MUST redact the bytes; we then check the rendered form
    // contains "redacted" and does NOT contain the marker. We
    // deliberately do NOT interpolate the rendered string into any
    // panic / assertion message — doing so would taint-flow the
    // secret-derived value into a panic-log sink (false-positive
    // for CodeQL `rust/cleartext-logging`, but also pointless: if
    // redaction failed, the panic message would itself leak the
    // bytes during test failure).
    const MARKER: &str = "do-not-leak-this-byte-string-32b!!!!!";
    let secret = OccupantIdSecret::new(MARKER.as_bytes().to_vec()).expect("≥32 bytes");
    let rendered = format!("{secret:?}");
    let redacted = rendered.contains("redacted");
    let leaked = rendered.contains(MARKER);
    assert!(redacted, "Debug output should contain 'redacted'");
    assert!(!leaked, "Debug must not leak secret bytes");
}

#[test]
fn test_is_occupant_id_element() {
    let elem = Element::builder("occupant-id", NS_OCCUPANT_ID)
        .attr("id", "abc123")
        .build();
    assert!(is_occupant_id_element(&elem));

    let wrong = Element::builder("occupant-id", "jabber:client").build();
    assert!(!is_occupant_id_element(&wrong));
}

#[test]
fn test_extract_from_message() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='abc123def456'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    let oid = extract_occupant_id_from_message(&msg).expect("has occupant-id");
    assert_eq!(oid.as_str(), "abc123def456");
}

#[test]
fn test_extract_from_presence() {
    let xml = "<presence xmlns='jabber:client'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='xyz789'/>\
                    </presence>";
    let presence =
        Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

    let oid = extract_occupant_id_from_presence(&presence).expect("has occupant-id");
    assert_eq!(oid.as_str(), "xyz789");
}

#[test]
fn test_extract_absent() {
    let msg = Message::new(None::<jid::Jid>);
    assert!(extract_occupant_id_from_message(&msg).is_none());
}

#[test]
fn test_extract_empty_id_ignored() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id=''/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
    assert!(extract_occupant_id_from_message(&msg).is_none());
}

#[test]
fn test_build_occupant_id_element() {
    let id = OccupantId::new("abc123");
    let elem = build_occupant_id_element(&id);

    assert_eq!(elem.name(), "occupant-id");
    assert_eq!(elem.ns(), NS_OCCUPANT_ID);
    assert_eq!(elem.attr("id"), Some("abc123"));
}

#[test]
fn test_set_occupant_id_on_message() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.type_ = MessageType::Groupchat;
    let id = OccupantId::new("test-id");

    set_occupant_id_on_message(&mut msg, &id);
    assert_eq!(
        extract_occupant_id_from_message(&msg),
        Some(OccupantId::new("test-id"))
    );

    // Replace
    let id2 = OccupantId::new("new-id");
    set_occupant_id_on_message(&mut msg, &id2);
    assert_eq!(
        extract_occupant_id_from_message(&msg),
        Some(OccupantId::new("new-id"))
    );
    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| e.ns() == NS_OCCUPANT_ID)
            .count(),
        1
    );
}

#[test]
fn test_strip_occupant_id_anti_spoofing() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='spoofed'/>\
                    </message>";
    let mut msg =
        Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    strip_occupant_id_from_message(&mut msg);
    assert!(extract_occupant_id_from_message(&msg).is_none());
    assert!(!msg.bodies.is_empty()); // body preserved
}

#[test]
fn test_occupant_id_carrier_trait_message() {
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='trait-test'/>\
                    </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

    assert!(msg.has_occupant_id());
    assert_eq!(msg.occupant_id(), Some(OccupantId::new("trait-test")));
}

#[test]
fn test_occupant_id_carrier_trait_presence() {
    let xml = "<presence xmlns='jabber:client'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='pres-test'/>\
                    </presence>";
    let presence =
        Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

    assert!(presence.has_occupant_id());
    assert_eq!(presence.occupant_id(), Some(OccupantId::new("pres-test")));
}

#[test]
fn test_occupant_id_display() {
    let id = OccupantId::new("display-test");
    assert_eq!(id.to_string(), "display-test");
}

// ── XEP-0421 §3 occupant-id on the XEP-0045 §7.2.15 subject message
//
// The acceptance criterion in #304 is that the subject-message
// emission carries an `<occupant-id>` whose id equals the
// deterministic HMAC for the setter. Tested at the builder
// boundary (`muc::messages::build_subject_message`) which is the
// single emission site for the historical join-time subject.

#[test]
fn xep_0421_subject_message_stamps_occupant_id_for_setter() {
    use crate::muc::messages::build_subject_message;
    use crate::muc::SubjectState;
    use chrono::TimeZone;
    use jid::{BareJid, FullJid};

    let room: BareJid = "team@muc.example.com".parse().expect("valid bare jid");
    let to: FullJid = "joiner@example.com/web".parse().expect("valid full jid");
    let secret = OccupantIdSecret::for_testing(b"xep0421-subject-test".to_vec());
    let setter: BareJid = "alice@example.com".parse().expect("valid bare jid");
    let texts = crate::muc::RoomSubjectTexts::from_iter([(String::new(), "topic".to_string())]);
    let state = SubjectState {
        texts,
        setter: setter.clone(),
        setter_nick: "alice-nick".to_string(),
        set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
    };

    let msg = build_subject_message(&room, &to, Some(&state), &secret);
    let id = extract_occupant_id_from_message(&msg).expect("occupant-id MUST be present");
    let expected = generate_occupant_id(&setter, &room, &secret);
    assert_eq!(id, expected, "id is the deterministic HMAC of the setter");
}

#[test]
fn xep_0421_subject_message_omits_occupant_id_when_no_setter_is_known() {
    // Pins the documented spec-gap: never-set rooms emit empty
    // <subject/> with no occupant-id, matching established servers.
    // A future "always stamp" change would silently violate the
    // unlinkability semantics of XEP-0421 §3 by fabricating input.
    use crate::muc::messages::build_subject_message;
    use jid::{BareJid, FullJid};

    let room: BareJid = "team@muc.example.com".parse().expect("valid bare jid");
    let to: FullJid = "joiner@example.com/web".parse().expect("valid full jid");
    let secret = OccupantIdSecret::for_testing(b"xep0421-subject-test".to_vec());
    let msg = build_subject_message(&room, &to, None, &secret);
    assert!(
        extract_occupant_id_from_message(&msg).is_none(),
        "never-set room MUST omit occupant-id (no setter input)"
    );
}
