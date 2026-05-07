use super::*;
use jid::{FullJid, Jid};
use minidom::Element;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

fn test_secret() -> crate::xep::xep0421::OccupantIdSecret {
    crate::xep::xep0421::OccupantIdSecret::for_testing(b"presence-test-secret".to_vec())
}

fn make_sender_jid() -> FullJid {
    "user@example.com/resource".parse().unwrap()
}

fn make_join_presence(to: &str) -> Presence {
    let to_jid: Jid = to.parse().unwrap();
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(to_jid);

    let muc_element = Element::builder("x", NS_MUC).build();
    presence.payloads.push(muc_element);

    presence
}

fn make_leave_presence(to: &str) -> Presence {
    let to_jid: Jid = to.parse().unwrap();
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.to = Some(to_jid);
    presence
}

#[test]
fn test_parse_muc_join() {
    let presence = make_join_presence("room@muc.example.com/nickname");
    let sender = make_sender_jid();

    let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

    match result {
        MucPresenceAction::Join(req) => {
            assert_eq!(req.room_jid.to_string(), "room@muc.example.com");
            assert_eq!(req.nick, "nickname");
            assert_eq!(req.sender_jid, sender);
            assert!(req.password.is_none());
        }
        _ => panic!("Expected Join action"),
    }
}

#[test]
fn test_parse_muc_leave() {
    let presence = make_leave_presence("room@muc.example.com/nickname");
    let sender = make_sender_jid();

    let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

    match result {
        MucPresenceAction::Leave(req) => {
            assert_eq!(req.room_jid.to_string(), "room@muc.example.com");
            assert_eq!(req.nick, "nickname");
            assert_eq!(req.sender_jid, sender);
        }
        _ => panic!("Expected Leave action"),
    }
}

#[test]
fn test_parse_non_muc_presence() {
    let mut presence = Presence::new(PresenceType::None);
    let sender = make_sender_jid();

    let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();
    assert!(matches!(result, MucPresenceAction::NotMuc));

    let to_jid: Jid = "user@example.com/resource".parse().unwrap();
    presence.to = Some(to_jid);

    let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();
    assert!(matches!(result, MucPresenceAction::NotMuc));
}

#[test]
fn test_parse_muc_update_without_x_element() {
    let to_jid: Jid = "room@muc.example.com/nickname".parse().unwrap();
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(to_jid);

    let sender = make_sender_jid();
    let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

    match result {
        MucPresenceAction::Update(req) => {
            assert_eq!(req.room_jid.to_string(), "room@muc.example.com");
            assert_eq!(req.nick, "nickname");
            assert_eq!(req.sender_jid, sender);
        }
        _ => panic!("Expected Update action"),
    }
}

#[test]
fn test_build_occupant_presence() {
    let from: FullJid = "room@muc.example.com/joiner".parse().unwrap();
    let to: FullJid = "user@example.com/resource".parse().unwrap();
    let occupant_jid: FullJid = "joiner@example.com/desktop".parse().unwrap();

    let secret = test_secret();
    let occupant_bare = occupant_jid.to_bare();
    let presence = build_occupant_presence(
        &from,
        &to,
        Affiliation::Member,
        Role::Participant,
        true,
        &OccupantIdentity {
            bare_jid: &occupant_bare,
            real_jid: Some(&occupant_jid),
            secret: &secret,
        },
    );

    assert_eq!(presence.from, Some(Jid::from(from)));
    assert_eq!(presence.to, Some(Jid::from(to)));
    assert_eq!(presence.type_, PresenceType::None);
    assert!(!presence.payloads.is_empty());
    let muc_user = presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("MUC user payload");
    let item = muc_user
        .get_child("item", NS_MUC_USER)
        .expect("MUC item payload");
    assert_eq!(item.attr("jid"), Some("joiner@example.com/desktop"));
    assert!(
        muc_user
            .children()
            .any(|child| { child.is("status", NS_MUC_USER) && child.attr("code") == Some("100") }),
        "non-anonymous presence must include status 100"
    );
}

#[test]
fn test_build_leave_presence() {
    let from: FullJid = "room@muc.example.com/leaver".parse().unwrap();
    let to: FullJid = "user@example.com/resource".parse().unwrap();
    let occupant_jid: FullJid = "leaver@example.com/phone".parse().unwrap();

    let secret = test_secret();
    let occupant_bare = occupant_jid.to_bare();
    let presence = build_leave_presence(
        &from,
        &to,
        Affiliation::Member,
        true,
        &OccupantIdentity {
            bare_jid: &occupant_bare,
            real_jid: Some(&occupant_jid),
            secret: &secret,
        },
    );

    assert_eq!(presence.type_, PresenceType::Unavailable);
    assert!(!presence.payloads.is_empty());
    let muc_user = presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("MUC user payload");
    let item = muc_user
        .get_child("item", NS_MUC_USER)
        .expect("MUC item payload");
    assert_eq!(item.attr("jid"), Some("leaver@example.com/phone"));
}

#[test]
fn test_build_occupant_presence_update_replaces_spoofable_identity_payloads() {
    let from: FullJid = "room@muc.example.com/rawkode".parse().unwrap();
    let to: FullJid = "alice@example.com/resource".parse().unwrap();
    let occupant_jid: FullJid = "rawkode@example.com/desktop".parse().unwrap();
    let mut incoming = Presence::new(PresenceType::None);
    incoming
        .payloads
        .push(Element::builder("x", NS_MUC_USER).build());
    incoming.payloads.push(
        Element::builder("occupant-id", crate::xep::xep0421::NS_OCCUPANT_ID)
            .attr("id", "spoofed")
            .build(),
    );
    incoming
        .statuses
        .insert(String::new(), "coding".to_string());

    let secret = test_secret();
    let occupant_bare = occupant_jid.to_bare();
    let presence = build_occupant_presence_update(
        &incoming,
        &from,
        &to,
        Affiliation::Member,
        Role::Participant,
        false,
        &OccupantIdentity {
            bare_jid: &occupant_bare,
            real_jid: Some(&occupant_jid),
            secret: &secret,
        },
    );

    assert_eq!(presence.statuses.get(""), Some(&"coding".to_string()));
    assert_eq!(presence.from, Some(Jid::from(from)));
    assert_eq!(presence.to, Some(Jid::from(to)));
    assert_eq!(
        presence
            .payloads
            .iter()
            .filter(|payload| payload.is("x", NS_MUC_USER))
            .count(),
        1
    );
    let muc_user = presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("MUC user payload");
    let item = muc_user
        .get_child("item", NS_MUC_USER)
        .expect("MUC item payload");
    assert_eq!(item.attr("jid"), Some("rawkode@example.com/desktop"));
    assert!(
        presence.payloads.iter().any(|payload| {
            payload.is("occupant-id", crate::xep::xep0421::NS_OCCUPANT_ID)
                && payload.attr("id") != Some("spoofed")
        }),
        "server-generated occupant-id should replace spoofed client payload"
    );
}

#[test]
fn test_parse_muc_join_with_history() {
    let to_jid: Jid = "room@muc.example.com/nickname".parse().unwrap();
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(to_jid);

    let history = Element::builder("history", NS_MUC)
        .attr("maxstanzas", "50")
        .attr("seconds", "3600")
        .build();
    let muc_element = Element::builder("x", NS_MUC).append(history).build();
    presence.payloads.push(muc_element);

    let sender = make_sender_jid();
    let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

    match result {
        MucPresenceAction::Join(req) => {
            assert!(req.history.is_some());
            let history = req.history.unwrap();
            assert_eq!(history.maxstanzas, Some(50));
            assert_eq!(history.seconds, Some(3600));
            assert!(history.maxchars.is_none());
            assert!(history.since.is_none());
        }
        _ => panic!("Expected Join action"),
    }
}

#[test]
fn test_parse_muc_join_with_history_disabled() {
    let to_jid: Jid = "room@muc.example.com/nickname".parse().unwrap();
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(to_jid);

    let history = Element::builder("history", NS_MUC)
        .attr("maxchars", "0")
        .build();
    let muc_element = Element::builder("x", NS_MUC).append(history).build();
    presence.payloads.push(muc_element);

    let sender = make_sender_jid();
    let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

    match result {
        MucPresenceAction::Join(req) => {
            assert!(req.history.is_some());
            let history = req.history.unwrap();
            assert!(history.is_disabled());
        }
        _ => panic!("Expected Join action"),
    }
}

#[test]
fn test_history_request_default() {
    let default = HistoryRequest::default_request();
    assert_eq!(default.maxstanzas, Some(25));
    assert!(!default.is_disabled());
}
