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
        MucPresenceStatus::new(true, true),
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
fn test_build_occupant_presence_created_room_self_includes_201() {
    let from: FullJid = "room@muc.example.com/creator".parse().unwrap();
    let creator: FullJid = "creator@example.com/desktop".parse().unwrap();
    let peer: FullJid = "peer@example.com/phone".parse().unwrap();

    let secret = test_secret();
    let creator_bare = creator.to_bare();
    let identity = OccupantIdentity {
        bare_jid: &creator_bare,
        real_jid: Some(&creator),
        secret: &secret,
    };

    let self_presence = build_occupant_presence(
        &from,
        &creator,
        Affiliation::Owner,
        Role::Moderator,
        MucPresenceStatus::created_self(true),
        &identity,
    );
    let self_x = self_presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("self muc#user payload");
    let self_codes: Vec<&str> = self_x
        .children()
        .filter(|child| child.is("status", NS_MUC_USER))
        .filter_map(|status| status.attr("code"))
        .collect();
    assert!(
        self_codes.contains(&"110"),
        "creator self-presence must include status 110; got {self_codes:?}"
    );
    assert!(
        self_codes.contains(&"201"),
        "created-room self-presence must include status 201; got {self_codes:?}"
    );

    let peer_presence = build_occupant_presence(
        &from,
        &peer,
        Affiliation::Owner,
        Role::Moderator,
        MucPresenceStatus {
            is_self: false,
            room_created: true,
            include_nonanonymous_status: true,
        },
        &identity,
    );
    let peer_x = peer_presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("peer muc#user payload");
    assert!(
        !peer_x
            .children()
            .any(|child| { child.is("status", NS_MUC_USER) && child.attr("code") == Some("201") }),
        "status 201 is only valid on the creator's self-presence"
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
        MucPresenceStatus::new(true, true),
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
    assert_eq!(item.attr("affiliation"), Some("member"));
    // XEP-0045 §7.14: leave presence MUST carry `role='none'` on the
    // wire so receivers can distinguish "I have left" from a no-op
    // presence update.
    assert_eq!(item.attr("role"), Some("none"));
}

/// XEP-0045 §9.1.1 ("Kicking an Occupant"), normative:
///
/// > The service MUST then remove the kicked occupant by sending a
/// > presence stanza of type "unavailable" to each kicked occupant,
/// > including status code 307 in the extended presence information,
/// > optionally along with the reason (if provided) and the JID of
/// > the actor who initiated the kick.
///
/// The wire form is `<presence type='unavailable' from='room/nick'>
/// <x xmlns='muc#user'><item affiliation='…' role='none'><actor jid='…'/>
/// <reason>…</reason></item><status code='307'/></x></presence>`.
#[test]
fn test_build_kick_presence_self_includes_307_and_110_and_actor_reason() {
    use jid::BareJid;
    let from: FullJid = "room@muc.example.com/bob".parse().unwrap();
    let to: FullJid = "bob@example.com/desk".parse().unwrap();
    let target_jid: FullJid = "bob@example.com/desk".parse().unwrap();
    let actor: BareJid = "alice@example.com".parse().unwrap();

    let secret = test_secret();
    let target_bare = target_jid.to_bare();
    let presence = build_kick_presence(
        &from,
        &to,
        Affiliation::Member,
        MucPresenceStatus::new(true, true),
        Some("spam"),
        Some(&actor),
        &OccupantIdentity {
            bare_jid: &target_bare,
            real_jid: Some(&target_jid),
            secret: &secret,
        },
    );

    assert_eq!(presence.type_, PresenceType::Unavailable);
    let muc_user = presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("muc#user payload");
    let item = muc_user
        .get_child("item", NS_MUC_USER)
        .expect("muc#user item");

    assert_eq!(item.attr("affiliation"), Some("member"));
    assert_eq!(item.attr("role"), Some("none"));
    assert_eq!(item.attr("jid"), Some("bob@example.com/desk"));

    let actor_elem = item.get_child("actor", NS_MUC_USER).expect("actor element");
    assert_eq!(actor_elem.attr("jid"), Some("alice@example.com"));

    let reason = item
        .get_child("reason", NS_MUC_USER)
        .expect("reason element");
    assert_eq!(reason.text(), "spam");

    let codes: Vec<&str> = muc_user
        .children()
        .filter(|child| child.is("status", NS_MUC_USER))
        .filter_map(|status| status.attr("code"))
        .collect();
    assert!(
        codes.contains(&"307"),
        "kick presence MUST carry status code 307; got {codes:?}"
    );
    assert!(
        codes.contains(&"110"),
        "self-presence to the kicked occupant MUST carry status code 110; got {codes:?}"
    );
}

/// XEP-0045 §9.1.1: a kick broadcast to a *remaining* occupant MUST
/// have status code 307 but MUST NOT carry status code 110, since
/// 110 means "this presence is about you".
#[test]
fn test_build_kick_presence_remaining_excludes_110() {
    use jid::BareJid;
    let from: FullJid = "room@muc.example.com/bob".parse().unwrap();
    let to: FullJid = "charlie@example.com/desk".parse().unwrap();
    let target_jid: FullJid = "bob@example.com/desk".parse().unwrap();
    let actor: BareJid = "alice@example.com".parse().unwrap();

    let secret = test_secret();
    let target_bare = target_jid.to_bare();
    let presence = build_kick_presence(
        &from,
        &to,
        Affiliation::Member,
        MucPresenceStatus::new(false, true),
        None,
        Some(&actor),
        &OccupantIdentity {
            bare_jid: &target_bare,
            real_jid: Some(&target_jid),
            secret: &secret,
        },
    );

    let muc_user = presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("muc#user payload");
    let codes: Vec<&str> = muc_user
        .children()
        .filter(|child| child.is("status", NS_MUC_USER))
        .filter_map(|status| status.attr("code"))
        .collect();
    assert!(
        codes.contains(&"307"),
        "remaining-occupant kick broadcast MUST carry status code 307; got {codes:?}"
    );
    assert!(
        !codes.contains(&"110"),
        "remaining-occupant kick broadcast MUST NOT carry status code 110; got {codes:?}"
    );

    let item = muc_user
        .get_child("item", NS_MUC_USER)
        .expect("muc#user item");
    assert_eq!(item.attr("role"), Some("none"));
    // No reason provided, so no <reason/> child.
    assert!(item.get_child("reason", NS_MUC_USER).is_none());
}

/// XEP-0045 §10.2 ("Banning a User"): unavailable presence with
/// `<item affiliation='outcast' role='none'>` and status code 301.
#[test]
fn test_build_ban_presence_self_includes_301_outcast_role_none() {
    use jid::BareJid;
    let from: FullJid = "room@muc.example.com/bob".parse().unwrap();
    let to: FullJid = "bob@example.com/desk".parse().unwrap();
    let target_jid: FullJid = "bob@example.com/desk".parse().unwrap();
    let actor: BareJid = "alice@example.com".parse().unwrap();

    let secret = test_secret();
    let target_bare = target_jid.to_bare();
    let presence = build_ban_presence(
        &from,
        &to,
        MucPresenceStatus::new(true, true),
        Some("trolling"),
        Some(&actor),
        &OccupantIdentity {
            bare_jid: &target_bare,
            real_jid: Some(&target_jid),
            secret: &secret,
        },
    );

    let muc_user = presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
        .expect("muc#user payload");
    let item = muc_user
        .get_child("item", NS_MUC_USER)
        .expect("muc#user item");
    assert_eq!(item.attr("affiliation"), Some("outcast"));
    assert_eq!(item.attr("role"), Some("none"));

    let codes: Vec<&str> = muc_user
        .children()
        .filter(|child| child.is("status", NS_MUC_USER))
        .filter_map(|status| status.attr("code"))
        .collect();
    assert!(
        codes.contains(&"301"),
        "ban presence MUST carry status code 301; got {codes:?}"
    );
    assert!(
        codes.contains(&"110"),
        "self-presence to the banned occupant MUST carry status code 110; got {codes:?}"
    );

    let reason = item
        .get_child("reason", NS_MUC_USER)
        .expect("reason element");
    assert_eq!(reason.text(), "trolling");
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
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "spoofed")
            .build(),
    );
    incoming.statuses.insert(
        xmpp_parsers::message::Lang(String::new()),
        "coding".to_string(),
    );

    let secret = test_secret();
    let occupant_bare = occupant_jid.to_bare();
    let presence = build_occupant_presence_update(
        &incoming,
        &from,
        &to,
        Affiliation::Member,
        Role::Participant,
        MucPresenceStatus::new(false, true),
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
        .attr(minidom::rxml::xml_ncname!("maxstanzas").to_owned(), "50")
        .attr(minidom::rxml::xml_ncname!("seconds").to_owned(), "3600")
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
        .attr(minidom::rxml::xml_ncname!("maxchars").to_owned(), "0")
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

/// XEP-0045 §10.9: when a room is destroyed, each occupant receives
/// `<presence type='unavailable'>` carrying `<x xmlns='muc#user'>`
/// with `<item affiliation='none' role='none'/>` and a `<destroy/>`
/// child that conveys the optional alternate venue and reason.
/// Self-presence additionally carries status code 110.
#[test]
fn test_build_destroy_notification() {
    use jid::BareJid;
    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let occupant_jid: FullJid = "user@example.com/res".parse().unwrap();

    let request = DestroyRequest {
        reason: Some("Room closed".to_string()),
        alternate_venue: Some("newroom@muc.example.com".parse().unwrap()),
        password: None,
    };

    let secret = test_secret();
    let occupant_bare = occupant_jid.to_bare();
    let presence = build_destroy_notification(
        &room_jid,
        "user",
        &occupant_jid,
        &request,
        true,
        &OccupantIdentity {
            bare_jid: &occupant_bare,
            real_jid: Some(&occupant_jid),
            secret: &secret,
        },
    );

    assert!(matches!(presence.type_, PresenceType::Unavailable));
    assert!(presence.from.is_some());
    assert!(presence.to.is_some());

    let x_elem = presence
        .payloads
        .iter()
        .find(|p| p.name() == "x" && p.ns() == NS_MUC_USER)
        .expect("muc#user x element");

    let item = x_elem.get_child("item", NS_MUC_USER).expect("item element");
    assert_eq!(item.attr("affiliation"), Some("none"));
    assert_eq!(item.attr("role"), Some("none"));

    let destroy = x_elem
        .get_child("destroy", NS_MUC_USER)
        .expect("destroy element");
    assert_eq!(destroy.attr("jid"), Some("newroom@muc.example.com"));

    let reason = destroy
        .get_child("reason", NS_MUC_USER)
        .expect("reason element");
    assert_eq!(reason.text(), "Room closed");

    let status_110 = x_elem
        .children()
        .filter(|c| c.is("status", NS_MUC_USER))
        .find(|s| s.attr("code") == Some("110"));
    assert!(
        status_110.is_some(),
        "self-presence must carry status code 110"
    );

    // XEP-0421 Business Rules: occupant-id MUST be on every presence
    // sent by a MUC — the destroy notification included (#1268).
    let occupant_id = crate::xep::xep0421::extract_occupant_id_from_presence(&presence)
        .expect("destroy presence carries occupant-id");
    let expected =
        crate::xep::xep0421::generate_occupant_id(&occupant_jid.to_bare(), &room_jid, &secret);
    assert_eq!(occupant_id, expected);
}

/// XEP-0045 §10.9: a destroy notification addressed to a different
/// occupant (not self) must NOT carry status code 110, and the
/// `<destroy/>` child stays present even with no reason / alternate.
#[test]
fn test_build_destroy_notification_not_self_minimal() {
    use jid::BareJid;
    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let occupant_jid: FullJid = "bob@example.com/desk".parse().unwrap();
    let secret = test_secret();
    let occupant_bare = occupant_jid.to_bare();
    let presence = build_destroy_notification(
        &room_jid,
        "alice",
        &occupant_jid,
        &DestroyRequest::default(),
        false,
        &OccupantIdentity {
            bare_jid: &occupant_bare,
            real_jid: Some(&occupant_jid),
            secret: &secret,
        },
    );
    let x_elem = presence
        .payloads
        .iter()
        .find(|p| p.name() == "x" && p.ns() == NS_MUC_USER)
        .expect("muc#user x element");
    assert!(x_elem.get_child("destroy", NS_MUC_USER).is_some());
    assert!(
        x_elem
            .children()
            .filter(|c| c.is("status", NS_MUC_USER))
            .all(|s| s.attr("code") != Some("110")),
        "non-self destroy must not carry status code 110"
    );
}
