use super::*;

fn make_groupchat_message(to: &str, body: &str) -> Message {
    let bare_jid: BareJid = to.parse().unwrap();
    let mut msg = Message::new(Some(Jid::from(bare_jid)));
    msg.type_ = MessageType::Groupchat;
    msg.id = Some(xmpp_parsers::message::Id("msg-1".to_string()));
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    msg
}

#[test]
fn test_muc_message_from_groupchat() {
    let msg = make_groupchat_message("room@muc.example.com", "Hello!");
    let sender: FullJid = "user@example.com/resource".parse().unwrap();

    let muc_msg = MucMessage::from_message(msg, sender.clone()).unwrap();

    assert_eq!(muc_msg.room_jid.to_string(), "room@muc.example.com");
    assert_eq!(muc_msg.sender_jid, sender);
    assert!(muc_msg.has_body());
    assert_eq!(muc_msg.body_text(), Some("Hello!"));
}

#[test]
fn test_muc_message_rejects_non_groupchat() {
    let mut msg = make_groupchat_message("room@muc.example.com", "Hello!");
    msg.type_ = MessageType::Chat; // Wrong type!

    let sender: FullJid = "user@example.com/resource".parse().unwrap();
    let result = MucMessage::from_message(msg, sender);

    assert!(result.is_err());
}

#[test]
fn test_muc_message_rejects_missing_to() {
    let mut msg = Message::new(None::<Jid>);
    msg.type_ = MessageType::Groupchat;

    let sender: FullJid = "user@example.com/resource".parse().unwrap();
    let result = MucMessage::from_message(msg, sender);

    assert!(result.is_err());
}

#[test]
fn test_is_muc_groupchat() {
    let groupchat = make_groupchat_message("room@muc.example.com", "Hello!");
    assert!(is_muc_groupchat(&groupchat));

    let bare_jid: BareJid = "user@example.com".parse().unwrap();
    let mut chat = Message::new(Some(Jid::from(bare_jid)));
    chat.type_ = MessageType::Chat;
    assert!(!is_muc_groupchat(&chat));
}

#[test]
fn test_looks_like_muc_jid() {
    let muc_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let conf_jid: BareJid = "room@conference.example.com".parse().unwrap();
    let user_jid: BareJid = "user@example.com".parse().unwrap();

    assert!(looks_like_muc_jid(&muc_jid));
    assert!(looks_like_muc_jid(&conf_jid));
    assert!(!looks_like_muc_jid(&user_jid));
}

#[test]
fn test_create_broadcast_message() {
    let original = make_groupchat_message("room@muc.example.com", "Hello!");
    let from: FullJid = "room@muc.example.com/sender_nick".parse().unwrap();
    let to: FullJid = "user@example.com/resource".parse().unwrap();

    let broadcast = create_broadcast_message(&original, from.clone(), to.clone());

    assert_eq!(broadcast.type_, MessageType::Groupchat);
    assert_eq!(broadcast.from, Some(Jid::from(from)));
    assert_eq!(broadcast.to, Some(Jid::from(to)));
    assert_eq!(
        broadcast.id,
        Some(xmpp_parsers::message::Id("msg-1".to_string()))
    );
}

#[test]
fn test_message_route_result() {
    let success = MessageRouteResult::success(vec![]);
    assert!(success.success);
    assert!(success.error.is_none());

    let failure = MessageRouteResult::failure("Room not found");
    assert!(!failure.success);
    assert_eq!(failure.error, Some("Room not found".to_string()));
}

// ── XEP-0045 §7.2.15 join-time subject emission ─────────────────────

use crate::xep::xep0203::{extract_delay_from_message, has_delay};
use crate::xep::xep0421::{extract_occupant_id_from_message, generate_occupant_id};
use chrono::TimeZone;

fn test_room() -> BareJid {
    "team@muc.example.com".parse().expect("valid bare jid")
}
fn test_recipient() -> FullJid {
    "joiner@example.com/web".parse().expect("valid full jid")
}
fn test_secret() -> OccupantIdSecret {
    OccupantIdSecret::for_testing(b"subject-builder-test-secret".to_vec())
}
fn sample_state(text: &str) -> SubjectState {
    let texts = crate::muc::RoomSubjectTexts::from_iter([(String::new(), text.to_string())]);
    SubjectState {
        texts,
        setter: "alice@example.com".parse().expect("valid bare jid"),
        setter_nick: "alice-nick".to_string(),
        set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
    }
}

#[test]
fn build_subject_message_set_state_produces_section_7_2_15_shape() {
    let room = test_room();
    let to = test_recipient();
    let secret = test_secret();
    let state = sample_state("Fire Burn and Cauldron Bubble!");

    let msg = build_subject_message(&room, &to, Some(&state), &secret);

    assert_eq!(msg.type_, MessageType::Groupchat);
    assert_eq!(
        msg.from.as_ref().map(|j| j.to_string()),
        Some("team@muc.example.com/alice-nick".to_string())
    );
    assert_eq!(msg.to.as_ref().map(|j| j.to_string()), Some(to.to_string()));
    assert_eq!(msg.subjects.len(), 1, "exactly one <subject/> element");
    assert_eq!(
        msg.subjects.iter().next().map(|s| s.1.as_str()),
        Some("Fire Burn and Cauldron Bubble!")
    );
    assert!(msg.bodies.is_empty(), "subject message has no <body/>");
    assert!(has_delay(&msg), "<delay/> SHOULD be present (§7.2.15)");
    assert!(
        extract_occupant_id_from_message(&msg).is_some(),
        "XEP-0421 occupant-id MUST be stamped"
    );
}

#[test]
fn build_subject_message_cleared_state_emits_empty_subject_with_delay() {
    let room = test_room();
    let to = test_recipient();
    let secret = test_secret();
    let state = sample_state("");

    let msg = build_subject_message(&room, &to, Some(&state), &secret);

    assert_eq!(
        msg.subjects.iter().next().map(|s| s.1.as_str()),
        Some(""),
        "explicitly cleared subject is empty <subject/>"
    );
    assert!(
        has_delay(&msg),
        "<delay/> SHOULD be included for actively-cleared subjects (§7.2.15)"
    );
    assert!(
        extract_occupant_id_from_message(&msg).is_some(),
        "occupant-id stamped because we know the user who cleared it"
    );
}

#[test]
fn build_subject_message_never_set_emits_empty_subject_without_delay() {
    let room = test_room();
    let to = test_recipient();
    let secret = test_secret();

    let msg = build_subject_message(&room, &to, None, &secret);

    assert_eq!(msg.type_, MessageType::Groupchat);
    assert_eq!(
        msg.from.as_ref().map(|j| j.to_string()),
        Some("team@muc.example.com".to_string()),
        "never-set rooms emit bare-from (§7.2.15 allows this; no setter exists)"
    );
    assert_eq!(
        msg.subjects.iter().next().map(|s| s.1.as_str()),
        Some(""),
        "MUST return an empty <subject/> (§7.2.15)"
    );
    assert!(
        !has_delay(&msg),
        "<delay/> MAY be omitted when the subject was never set (§7.2.15)"
    );
    assert!(
        extract_occupant_id_from_message(&msg).is_none(),
        "no setter means no input for the XEP-0421 HMAC; omitted, matching established servers"
    );
}

#[test]
fn build_subject_message_delay_from_attribute_is_room_jid_not_setter() {
    // §7.2.15 conditional MUST: "If the <delay/> element is included,
    // its 'from' attribute MUST be set to the JID of the room itself."
    let room = test_room();
    let to = test_recipient();
    let secret = test_secret();
    let state = sample_state("hello");

    let msg = build_subject_message(&room, &to, Some(&state), &secret);
    let delay = extract_delay_from_message(&msg).expect("<delay/> present");

    assert_eq!(
        delay.from.as_deref(),
        Some("team@muc.example.com"),
        "delay.from MUST be the room JID"
    );
    assert_ne!(
        delay.from.as_deref(),
        Some("team@muc.example.com/alice-nick"),
        "delay.from MUST NOT be the setter's room/nick"
    );
}

#[test]
fn build_subject_message_occupant_id_is_hmac_of_setter_bare_jid() {
    let room = test_room();
    let to = test_recipient();
    let secret = test_secret();
    let state = sample_state("hello");

    let msg = build_subject_message(&room, &to, Some(&state), &secret);

    let id = extract_occupant_id_from_message(&msg).expect("occupant-id stamped");
    let expected = generate_occupant_id(&state.setter, &room, &secret);
    assert_eq!(id, expected);
}

#[test]
fn build_subject_message_preserves_every_persisted_language_variant() {
    // §8.1 broadcasts (and §7.2.13 archive) carry every
    // <subject xml:lang='...'> the originating message had.
    // Join-time replay is built from the persisted state, so it
    // must reproduce all of them — not just the default-language
    // entry — otherwise late joiners see a different subject set
    // than every existing occupant did.
    let room = test_room();
    let to = test_recipient();
    let secret = test_secret();
    let texts = crate::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("en".to_string(), "English subject".to_string()),
        ("fr".to_string(), "Sujet français".to_string()),
    ]);
    let state = SubjectState {
        texts,
        setter: "alice@example.com".parse().expect("valid bare jid"),
        setter_nick: "alice-nick".to_string(),
        set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
    };

    let msg = build_subject_message(&room, &to, Some(&state), &secret);

    assert_eq!(
        msg.subjects.len(),
        3,
        "every persisted language variant must round-trip into the join-time replay"
    );
    assert_eq!(
        msg.subjects.get("").map(|s| s.as_str()),
        Some("Default subject")
    );
    assert_eq!(
        msg.subjects.get("en").map(|s| s.as_str()),
        Some("English subject")
    );
    assert_eq!(
        msg.subjects.get("fr").map(|s| s.as_str()),
        Some("Sujet français")
    );
}

#[test]
fn build_subject_message_delay_stamp_round_trips_as_xep_0082_datetime() {
    // XEP-0203 + XEP-0082: stamp MUST be a valid dateTime; the
    // round-trip through chrono confirms our `to_rfc3339()` output
    // is parseable by any conforming consumer.
    let room = test_room();
    let to = test_recipient();
    let secret = test_secret();
    let state = sample_state("hello");
    let original_stamp = state.set_at;

    let msg = build_subject_message(&room, &to, Some(&state), &secret);
    let delay = extract_delay_from_message(&msg).expect("<delay/> present");

    assert_eq!(delay.stamp, original_stamp);
}
