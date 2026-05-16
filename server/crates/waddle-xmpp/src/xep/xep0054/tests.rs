use super::*;

#[test]
fn test_is_vcard_get_classifies_only_get_with_correct_payload() {
    let vcard_elem = Element::builder("vCard", NS_VCARD).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "vcard-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(vcard_elem),
    };

    assert!(is_vcard_get(&iq));
    assert!(!is_vcard_set(&iq));
}

#[test]
fn test_is_vcard_set_classifies_only_set_with_correct_payload() {
    let vcard_elem = Element::builder("vCard", NS_VCARD)
        .append(Element::builder("FN", NS_VCARD).append("John Doe").build())
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "vcard-2".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(vcard_elem),
    };

    assert!(!is_vcard_get(&iq));
    assert!(is_vcard_set(&iq));
}

#[test]
fn test_is_not_vcard_query_wrong_ns() {
    let elem = Element::builder("vCard", "wrong:namespace").build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(elem),
    };

    assert!(!is_vcard_get(&iq));
}

#[test]
fn test_is_not_vcard_query_wrong_name() {
    let elem = Element::builder("query", NS_VCARD).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test-2".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(elem),
    };

    assert!(!is_vcard_get(&iq));
}

#[test]
fn test_parse_vcard_full() {
    let vcard_elem = Element::builder("vCard", NS_VCARD)
        .append(Element::builder("FN", NS_VCARD).append("John Doe").build())
        .append(
            Element::builder("NICKNAME", NS_VCARD)
                .append("johnd")
                .build(),
        )
        .append(
            Element::builder("EMAIL", NS_VCARD)
                .append(Element::builder("INTERNET", NS_VCARD).build())
                .append(
                    Element::builder("USERID", NS_VCARD)
                        .append("john@example.com")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("NOTE", NS_VCARD)
                .append("Hello, world!")
                .build(),
        )
        .append(
            Element::builder("URL", NS_VCARD)
                .append("https://example.com")
                .build(),
        )
        .build();

    let vcard = parse_vcard_element(&vcard_elem).unwrap();

    assert_eq!(vcard.full_name, Some("John Doe".to_string()));
    assert_eq!(vcard.nickname, Some("johnd".to_string()));
    assert_eq!(vcard.email, Some("john@example.com".to_string()));
    assert_eq!(vcard.note, Some("Hello, world!".to_string()));
    assert_eq!(vcard.url, Some("https://example.com".to_string()));
}

#[test]
fn test_parse_vcard_with_photo() {
    let vcard_elem = Element::builder("vCard", NS_VCARD)
        .append(
            Element::builder("PHOTO", NS_VCARD)
                .append(
                    Element::builder("TYPE", NS_VCARD)
                        .append("image/png")
                        .build(),
                )
                .append(
                    Element::builder("BINVAL", NS_VCARD)
                        .append("iVBORw0KGgo=")
                        .build(),
                )
                .build(),
        )
        .build();

    let vcard = parse_vcard_element(&vcard_elem).unwrap();

    assert!(vcard.photo.is_some());
    match vcard.photo.unwrap() {
        VCardPhoto::Binary { mime_type, data } => {
            assert_eq!(mime_type, "image/png");
            assert_eq!(data, "iVBORw0KGgo=");
        }
        _ => panic!("Expected Binary photo"),
    }
}

#[test]
fn test_parse_vcard_empty() {
    let vcard_elem = Element::builder("vCard", NS_VCARD).build();

    let vcard = parse_vcard_element(&vcard_elem).unwrap();

    assert!(vcard.full_name.is_none());
    assert!(vcard.nickname.is_none());
    assert!(vcard.email.is_none());
    assert!(vcard.photo.is_none());
}

#[test]
fn test_build_vcard_element() {
    let vcard = VCard {
        full_name: Some("Jane Doe".to_string()),
        nickname: Some("janed".to_string()),
        email: Some("jane@example.com".to_string()),
        note: Some("Test note".to_string()),
        url: Some("https://jane.example.com".to_string()),
        photo: None,
        birthday: Some("1990-01-15".to_string()),
        org: Some("Example Corp".to_string()),
        title: Some("Engineer".to_string()),
        desc: None,
    };

    let elem = build_vcard_element(&vcard);

    assert_eq!(elem.name(), "vCard");
    assert_eq!(elem.ns(), NS_VCARD);

    // Check FN
    let fn_elem = elem.get_child("FN", NS_VCARD).unwrap();
    assert_eq!(fn_elem.text(), "Jane Doe");

    // Check NICKNAME
    let nick_elem = elem.get_child("NICKNAME", NS_VCARD).unwrap();
    assert_eq!(nick_elem.text(), "janed");

    // Check EMAIL structure
    let email_elem = elem.get_child("EMAIL", NS_VCARD).unwrap();
    assert!(email_elem.get_child("INTERNET", NS_VCARD).is_some());
    assert!(email_elem.get_child("PREF", NS_VCARD).is_some());
    let userid_elem = email_elem.get_child("USERID", NS_VCARD).unwrap();
    assert_eq!(userid_elem.text(), "jane@example.com");
}

#[test]
fn test_build_vcard_response() {
    let vcard_elem = Element::builder("vCard", NS_VCARD).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("server.example.com".parse().unwrap()),
        id: "vcard-get-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(vcard_elem),
    };

    let vcard = VCard {
        full_name: Some("Test User".to_string()),
        ..Default::default()
    };

    let response = build_vcard_response(&original_iq, &vcard);

    assert_eq!(response.id, "vcard-get-1");
    assert!(matches!(
        response.payload,
        xmpp_parsers::iq::IqType::Result(Some(_))
    ));
}

#[test]
fn test_build_empty_vcard_response() {
    let vcard_elem = Element::builder("vCard", NS_VCARD).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: None,
        id: "vcard-get-2".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(vcard_elem),
    };

    let response = build_empty_vcard_response(&original_iq);

    assert_eq!(response.id, "vcard-get-2");
    if let xmpp_parsers::iq::IqType::Result(Some(elem)) = &response.payload {
        assert_eq!(elem.name(), "vCard");
        assert_eq!(elem.ns(), NS_VCARD);
        // Empty vCard should have no children
        assert!(elem.children().next().is_none());
    } else {
        panic!("Expected Result with vCard element");
    }
}

#[test]
fn test_build_vcard_success() {
    let vcard_elem = Element::builder("vCard", NS_VCARD).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: None,
        id: "vcard-set-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(vcard_elem),
    };

    let response = build_vcard_success(&original_iq);

    assert_eq!(response.id, "vcard-set-1");
    assert!(matches!(
        response.payload,
        xmpp_parsers::iq::IqType::Result(None)
    ));
}

#[test]
fn test_vcard_error_display() {
    assert_eq!(VCardError::NotFound.to_string(), "vCard not found");
    assert_eq!(
        VCardError::BadRequest("test".to_string()).to_string(),
        "Bad request: test"
    );
    assert_eq!(
        VCardError::InternalError("err".to_string()).to_string(),
        "Internal error: err"
    );
    assert_eq!(VCardError::NotAuthorized.to_string(), "Not authorized");
}

#[test]
fn test_roundtrip_vcard_binval() {
    let original = VCard {
        full_name: Some("Round Trip".to_string()),
        nickname: Some("rt".to_string()),
        email: Some("rt@example.com".to_string()),
        note: Some("Testing roundtrip".to_string()),
        url: Some("https://roundtrip.example.com".to_string()),
        birthday: Some("2000-12-31".to_string()),
        org: Some("Test Org".to_string()),
        title: Some("Tester".to_string()),
        desc: Some("A description".to_string()),
        photo: Some(VCardPhoto::Binary {
            mime_type: "image/jpeg".to_string(),
            data: "dGVzdA==".to_string(),
        }),
    };

    let elem = build_vcard_element(&original);
    let parsed = parse_vcard_element(&elem).unwrap();

    assert_eq!(original.full_name, parsed.full_name);
    assert_eq!(original.nickname, parsed.nickname);
    assert_eq!(original.email, parsed.email);
    assert_eq!(original.note, parsed.note);
    assert_eq!(original.url, parsed.url);
    assert_eq!(original.birthday, parsed.birthday);
    assert_eq!(original.org, parsed.org);
    assert_eq!(original.title, parsed.title);
    assert_eq!(original.desc, parsed.desc);
    assert!(parsed.photo.is_some());
    match parsed.photo.unwrap() {
        VCardPhoto::Binary { mime_type, data } => {
            assert_eq!(mime_type, "image/jpeg");
            assert_eq!(data, "dGVzdA==");
        }
        _ => panic!("Expected Binary photo"),
    }
}

#[test]
fn test_roundtrip_vcard_extval() {
    let original = VCard {
        full_name: Some("External Photo".to_string()),
        desc: Some("Has an external avatar".to_string()),
        photo: Some(VCardPhoto::External {
            url: "https://cdn.bsky.app/img/avatar/plain/did:plc:abc123/cid@jpeg".to_string(),
        }),
        ..Default::default()
    };

    let elem = build_vcard_element(&original);
    let parsed = parse_vcard_element(&elem).unwrap();

    assert_eq!(parsed.full_name, Some("External Photo".to_string()));
    assert_eq!(parsed.desc, Some("Has an external avatar".to_string()));
    match parsed.photo.unwrap() {
        VCardPhoto::External { url } => {
            assert_eq!(
                url,
                "https://cdn.bsky.app/img/avatar/plain/did:plc:abc123/cid@jpeg"
            );
        }
        _ => panic!("Expected External photo"),
    }
}

#[test]
fn test_parse_vcard_with_desc() {
    let vcard_elem = Element::builder("vCard", NS_VCARD)
        .append(Element::builder("FN", NS_VCARD).append("Test User").build())
        .append(
            Element::builder("DESC", NS_VCARD)
                .append("A bio from Bluesky")
                .build(),
        )
        .build();

    let vcard = parse_vcard_element(&vcard_elem).unwrap();
    assert_eq!(vcard.full_name, Some("Test User".to_string()));
    assert_eq!(vcard.desc, Some("A bio from Bluesky".to_string()));
}

#[test]
fn test_parse_vcard_with_extval_photo() {
    let vcard_elem = Element::builder("vCard", NS_VCARD)
        .append(
            Element::builder("PHOTO", NS_VCARD)
                .append(
                    Element::builder("EXTVAL", NS_VCARD)
                        .append("https://example.com/avatar.jpg")
                        .build(),
                )
                .build(),
        )
        .build();

    let vcard = parse_vcard_element(&vcard_elem).unwrap();
    assert!(vcard.photo.is_some());
    match vcard.photo.unwrap() {
        VCardPhoto::External { url } => {
            assert_eq!(url, "https://example.com/avatar.jpg");
        }
        _ => panic!("Expected External photo"),
    }
}
