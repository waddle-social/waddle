use super::*;
use crate::{StanzaErrorCondition, StanzaErrorType};

fn make_admin_get_iq(room_jid: &str, affiliation: &str) -> Iq {
    let query = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    affiliation,
                )
                .build(),
        )
        .build();

    Iq::Get {
        from: Some("user@example.com/res".parse().unwrap()),
        to: Some(room_jid.parse().unwrap()),
        id: "test-1".to_string(),
        payload: query,
    }
}

fn make_admin_set_iq(room_jid: &str, target_jid: &str, affiliation: &str) -> Iq {
    let query = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), target_jid)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    affiliation,
                )
                .build(),
        )
        .build();

    Iq::Set {
        from: Some("owner@example.com/res".parse().unwrap()),
        to: Some(room_jid.parse().unwrap()),
        id: "test-2".to_string(),
        payload: query,
    }
}

#[test]
fn test_is_muc_admin_get() {
    let iq = make_admin_get_iq("room@muc.example.com", "member");
    assert!(is_muc_admin_get(&iq));
    assert!(!is_muc_admin_set(&iq));
}

#[test]
fn test_is_muc_admin_set() {
    let iq = make_admin_set_iq("room@muc.example.com", "user@example.com", "admin");
    assert!(is_muc_admin_set(&iq));
    assert!(!is_muc_admin_get(&iq));
}

#[test]
fn test_is_muc_admin_iq() {
    let iq = make_admin_get_iq("room@muc.example.com", "member");
    assert!(is_muc_admin_iq(&iq, "muc.example.com"));
    assert!(!is_muc_admin_iq(&iq, "other.domain.com"));
}

#[test]
fn test_parse_admin_query_get() {
    let iq = make_admin_get_iq("room@muc.example.com", "member");
    let query = parse_admin_query(&iq, "muc.example.com").unwrap();

    assert_eq!(query.room_jid.to_string(), "room@muc.example.com");
    assert!(query.is_get);
    assert_eq!(query.items.len(), 1);
    assert_eq!(query.items[0].affiliation, Some(Affiliation::Member));
}

#[test]
fn test_parse_admin_query_set() {
    let iq = make_admin_set_iq("room@muc.example.com", "user@example.com", "admin");
    let query = parse_admin_query(&iq, "muc.example.com").unwrap();

    assert_eq!(query.room_jid.to_string(), "room@muc.example.com");
    assert!(!query.is_get);
    assert_eq!(query.items.len(), 1);
    assert_eq!(
        query.items[0].jid.as_ref().unwrap().to_string(),
        "user@example.com"
    );
    assert_eq!(query.items[0].affiliation, Some(Affiliation::Admin));
}

#[test]
fn test_parse_admin_query_set_normalizes_full_jid_to_bare() {
    let iq = make_admin_set_iq("room@muc.example.com", "user@example.com/laptop", "member");
    let query = parse_admin_query(&iq, "muc.example.com").unwrap();

    assert_eq!(query.items.len(), 1);
    assert_eq!(
        query.items[0].jid.as_ref().unwrap().to_string(),
        "user@example.com"
    );
    assert_eq!(query.items[0].affiliation, Some(Affiliation::Member));
}

#[test]
fn test_parse_admin_query_set_rejects_malformed_jid() {
    let iq = make_admin_set_iq("room@muc.example.com", "not a jid", "member");

    let err = parse_admin_query(&iq, "muc.example.com").expect_err("malformed jid rejected");
    match err {
        XmppError::Stanza {
            condition,
            error_type,
            text,
        } => {
            assert_eq!(condition, StanzaErrorCondition::BadRequest);
            assert_eq!(error_type, StanzaErrorType::Modify);
            assert_eq!(text.as_deref(), Some("Malformed jid: not a jid"));
        }
        other => panic!("expected bad-request stanza error, got {other:?}"),
    }
}

#[test]
fn test_parse_affiliation() {
    assert_eq!(parse_muc_affiliation("owner").unwrap(), Affiliation::Owner);
    assert_eq!(parse_muc_affiliation("admin").unwrap(), Affiliation::Admin);
    assert_eq!(
        parse_muc_affiliation("member").unwrap(),
        Affiliation::Member
    );
    assert_eq!(parse_muc_affiliation("none").unwrap(), Affiliation::None);
    assert_eq!(
        parse_muc_affiliation("outcast").unwrap(),
        Affiliation::Outcast
    );
    assert!(parse_muc_affiliation("invalid").is_err());
}

#[test]
fn test_parse_role() {
    assert_eq!(parse_muc_role("moderator").unwrap(), Role::Moderator);
    assert_eq!(parse_muc_role("participant").unwrap(), Role::Participant);
    assert_eq!(parse_muc_role("visitor").unwrap(), Role::Visitor);
    assert_eq!(parse_muc_role("none").unwrap(), Role::None);
    assert!(parse_muc_role("invalid").is_err());
}

#[test]
fn test_build_admin_result() {
    let items = vec![
        ("user1@example.com".parse().unwrap(), Affiliation::Member),
        ("user2@example.com".parse().unwrap(), Affiliation::Admin),
    ];

    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let to_jid: Jid = "requester@example.com".parse().unwrap();

    let result = build_admin_result("test-1", &room_jid, &to_jid, &items);

    assert_eq!(result.id(), "test-1");
    assert!(matches!(
        result,
        Iq::Result {
            payload: Some(_),
            ..
        }
    ));
}

#[test]
fn test_build_role_result_includes_required_affiliation_and_full_jid() {
    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let to_jid: Jid = "requester@example.com/desktop".parse().unwrap();
    let items = vec![(
        "thirdwitch".to_string(),
        Role::Moderator,
        Affiliation::Member,
        "hag66@example.com/pda".parse().unwrap(),
    )];

    let result = build_role_result("mod3", &room_jid, &to_jid, &items);
    let Iq::Result {
        payload: Some(query),
        ..
    } = result
    else {
        panic!("expected IQ result with query payload");
    };
    let item = query
        .get_child("item", NS_MUC_ADMIN)
        .expect("role result item");
    assert_eq!(item.attr("nick"), Some("thirdwitch"));
    assert_eq!(item.attr("role"), Some("moderator"));
    assert_eq!(item.attr("affiliation"), Some("member"));
    assert_eq!(item.attr("jid"), Some("hag66@example.com/pda"));
}

#[test]
fn test_build_admin_set_result() {
    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let to_jid: Jid = "owner@example.com".parse().unwrap();

    let result = build_admin_set_result("test-2", &room_jid, &to_jid);

    assert_eq!(result.id(), "test-2");
    assert!(matches!(result, Iq::Result { payload: None, .. }));
}

#[test]
fn test_parse_admin_query_with_reason() {
    let query = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    "banned@example.com",
                )
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    "outcast",
                )
                .append(
                    Element::builder("reason", NS_MUC_ADMIN)
                        .append("Spamming")
                        .build(),
                )
                .build(),
        )
        .build();

    let iq = Iq::Set {
        from: Some("owner@example.com/res".parse().unwrap()),
        to: Some("room@muc.example.com".parse().unwrap()),
        id: "ban-1".to_string(),
        payload: query,
    };

    let parsed = parse_admin_query(&iq, "muc.example.com").unwrap();

    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.items[0].affiliation, Some(Affiliation::Outcast));
    assert_eq!(parsed.items[0].reason.as_deref(), Some("Spamming"));
}

#[test]
fn test_parse_kick_query_with_nick() {
    let query = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("nick").to_owned(),
                    "troublemaker",
                )
                .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
                .append(
                    Element::builder("reason", NS_MUC_ADMIN)
                        .append("Kicked for bad behavior")
                        .build(),
                )
                .build(),
        )
        .build();

    let iq = Iq::Set {
        from: Some("moderator@example.com/res".parse().unwrap()),
        to: Some("room@muc.example.com".parse().unwrap()),
        id: "kick-1".to_string(),
        payload: query,
    };

    let parsed = parse_admin_query(&iq, "muc.example.com").unwrap();

    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.items[0].nick.as_deref(), Some("troublemaker"));
    assert_eq!(parsed.items[0].role, Some(Role::None));
    assert_eq!(
        parsed.items[0].reason.as_deref(),
        Some("Kicked for bad behavior")
    );
}
