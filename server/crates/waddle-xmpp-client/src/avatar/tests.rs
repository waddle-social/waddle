use super::*;
use crate::pubsub_event::{PubsubEvent, PubsubEventItem, PubsubEventPayload};

const HELLO_ID: &str = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";
const WORLD_ID: &str = "7c211433f02071597741e6ff5a8ea34789abbf43";

fn item_id(value: &str) -> AvatarItemId {
    AvatarItemId::new(value).expect("valid SHA-1 item id")
}

fn make_metadata_iq(id: &str, mime: &str) -> Element {
    let info = Element::builder("info", NS_AVATAR_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), mime)
        .attr(minidom::rxml::xml_ncname!("bytes").to_owned(), "42")
        .attr(minidom::rxml::xml_ncname!("width").to_owned(), "64")
        .attr(minidom::rxml::xml_ncname!("height").to_owned(), "64")
        .build();
    let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(info)
        .build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(metadata)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_METADATA,
        )
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .append(pubsub)
        .build()
}

fn make_metadata_url_iq(id: &str, mime: &str, url: &str) -> Element {
    let info = Element::builder("info", NS_AVATAR_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), mime)
        .attr(minidom::rxml::xml_ncname!("bytes").to_owned(), "42")
        .attr(minidom::rxml::xml_ncname!("url").to_owned(), url)
        .build();
    let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(info)
        .build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(metadata)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_METADATA,
        )
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .append(pubsub)
        .build()
}

fn make_data_iq(id: &str, base64_data: &str) -> Element {
    let data = Element::builder("data", NS_AVATAR_DATA)
        .append(minidom::Node::Text(base64_data.to_string()))
        .build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(data)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_DATA,
        )
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .append(pubsub)
        .build()
}

fn make_vcard_binval_iq(mime: &str, base64_data: &str) -> Element {
    let photo = Element::builder("PHOTO", NS_VCARD_TEMP)
        .append(Element::builder("TYPE", NS_VCARD_TEMP).append(mime).build())
        .append(
            Element::builder("BINVAL", NS_VCARD_TEMP)
                .append(base64_data)
                .build(),
        )
        .build();
    let vcard = Element::builder("vCard", NS_VCARD_TEMP)
        .append(photo)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .append(vcard)
        .build()
}

fn make_vcard_extval_iq(url: &str) -> Element {
    let photo = Element::builder("PHOTO", NS_VCARD_TEMP)
        .append(
            Element::builder("EXTVAL", NS_VCARD_TEMP)
                .append(url)
                .build(),
        )
        .build();
    let vcard = Element::builder("vCard", NS_VCARD_TEMP)
        .append(photo)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .append(vcard)
        .build()
}

#[test]
fn parse_metadata_extracts_info() {
    let iq = make_metadata_iq(HELLO_ID, "image/png");
    let info = parse_metadata_response(&iq).expect("info");
    assert_eq!(info.id, item_id(HELLO_ID));
    assert_eq!(info.mime_type, "image/png");
    assert_eq!(info.width, Some(64));
    assert_eq!(info.height, Some(64));
    assert_eq!(info.bytes, Some(42));
}

#[test]
fn parse_metadata_ignores_url_only_info() {
    let iq = make_metadata_url_iq(HELLO_ID, "image/png", "https://example.test/a.png");
    assert!(parse_metadata_response(&iq).is_none());
}

#[test]
fn parse_metadata_skips_malformed_and_url_info_for_in_band() {
    let malformed_info = Element::builder("info", NS_AVATAR_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "not-a-hash")
        .build();
    let url_info = Element::builder("info", NS_AVATAR_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), WORLD_ID)
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "https://example.test/avatar.png",
        )
        .build();
    let in_band_info = Element::builder("info", NS_AVATAR_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), HELLO_ID)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "image/png")
        .build();
    let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(malformed_info)
        .append(url_info)
        .append(in_band_info)
        .build();
    let item = Element::builder("item", NS_PUBSUB).append(metadata).build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_METADATA,
        )
        .append(item)
        .build();
    let iq = Element::builder("iq", NS_CLIENT)
        .append(Element::builder("pubsub", NS_PUBSUB).append(items).build())
        .build();

    assert_eq!(
        parse_metadata_response(&iq).map(|info| info.id),
        Some(item_id(HELLO_ID))
    );
}

#[test]
fn avatar_item_id_validates_sha1_hex_without_changing_wire_case() {
    assert!(AvatarItemId::new("not-a-hash").is_none());
    assert!(AvatarItemId::new("z".repeat(40)).is_none());
    let upper = HELLO_ID.to_ascii_uppercase();
    assert_eq!(item_id(&upper).as_str(), upper);
}

#[test]
fn parse_metadata_event_reports_set_disable_and_retract() {
    let owner: jid::Jid = "alice@example.com".parse().expect("valid bare JID");
    let set = PubsubEvent {
        from: Some(owner.clone()),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some(HELLO_ID.to_string()),
            retracted: false,
            payload: PubsubEventPayload::Opaque {
                element: Element::builder("metadata", NS_AVATAR_METADATA)
                    .append(
                        Element::builder("info", NS_AVATAR_METADATA)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), HELLO_ID)
                            .build(),
                    )
                    .build(),
            },
        }],
    };
    assert_eq!(
        parse_metadata_event(&set),
        Some(AvatarChanged {
            jid: "alice@example.com".parse().expect("valid bare JID"),
            avatar_id: Some(item_id(HELLO_ID)),
        })
    );

    let url_only = PubsubEvent {
        from: Some("alice@example.com".parse().expect("valid bare JID")),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some(WORLD_ID.to_string()),
            retracted: false,
            payload: PubsubEventPayload::Opaque {
                element: Element::builder("metadata", NS_AVATAR_METADATA)
                    .append(
                        Element::builder("info", NS_AVATAR_METADATA)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), WORLD_ID)
                            .attr(
                                minidom::rxml::xml_ncname!("url").to_owned(),
                                "https://example.test/avatar.png",
                            )
                            .build(),
                    )
                    .build(),
            },
        }],
    };
    assert_eq!(
        parse_metadata_event(&url_only).and_then(|event| event.avatar_id),
        Some(item_id(WORLD_ID))
    );

    let empty_metadata = PubsubEvent {
        from: Some(owner.clone()),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some(AVATAR_REMOVE_ITEM_ID.to_string()),
            retracted: false,
            payload: PubsubEventPayload::Opaque {
                element: Element::builder("metadata", NS_AVATAR_METADATA).build(),
            },
        }],
    };
    assert_eq!(
        parse_metadata_event(&empty_metadata).and_then(|event| event.avatar_id),
        None
    );

    let retract = PubsubEvent {
        from: Some(owner),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some(HELLO_ID.to_string()),
            retracted: true,
            payload: PubsubEventPayload::Empty,
        }],
    };
    assert_eq!(
        parse_metadata_event(&retract),
        None,
        "a retract is not a disable; it must not clear the avatar"
    );

    // A node that publishes a new avatar and retracts the old item in one
    // notification reports the new id, not a disable.
    let publish_and_retract = PubsubEvent {
        from: Some("alice@example.com".parse().expect("valid bare JID")),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![
            PubsubEventItem {
                id: Some("avatar-old".to_string()),
                retracted: true,
                payload: PubsubEventPayload::Empty,
            },
            PubsubEventItem {
                id: Some(WORLD_ID.to_string()),
                retracted: false,
                payload: PubsubEventPayload::Opaque {
                    element: Element::builder("metadata", NS_AVATAR_METADATA)
                        .append(
                            Element::builder("info", NS_AVATAR_METADATA)
                                .attr(minidom::rxml::xml_ncname!("id").to_owned(), WORLD_ID)
                                .build(),
                        )
                        .build(),
                },
            },
        ],
    };
    assert_eq!(
        parse_metadata_event(&publish_and_retract).and_then(|event| event.avatar_id),
        Some(item_id(WORLD_ID))
    );
}

#[test]
fn parse_metadata_event_rejects_full_jid_senders() {
    // A peer's client or a MUC occupant (room@muc/nick) is not a PEP
    // service; only the owner's bare JID may announce avatar changes.
    for from in ["alice@example.com/desktop", "room@muc.example.com/alice"] {
        let event = PubsubEvent {
            from: Some(from.parse().expect("valid full JID")),
            node: NS_AVATAR_METADATA.to_string(),
            items: vec![PubsubEventItem {
                id: Some("avatar-1".to_string()),
                retracted: true,
                payload: PubsubEventPayload::Empty,
            }],
        };
        assert!(parse_metadata_event(&event).is_none(), "{from}");
    }
}

#[test]
fn malformed_metadata_id_does_not_announce_avatar_disable() {
    let event = PubsubEvent {
        from: Some("alice@example.com".parse().expect("valid bare JID")),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some("broken".to_string()),
            retracted: false,
            payload: PubsubEventPayload::Opaque {
                element: Element::builder("metadata", NS_AVATAR_METADATA)
                    .append(
                        Element::builder("info", NS_AVATAR_METADATA)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "broken")
                            .build(),
                    )
                    .build(),
            },
        }],
    };
    assert!(parse_metadata_event(&event).is_none());

    let pointer_only = PubsubEvent {
        items: vec![PubsubEventItem {
            id: Some("pointer".to_string()),
            retracted: false,
            payload: PubsubEventPayload::Opaque {
                element: Element::builder("metadata", NS_AVATAR_METADATA)
                    .append(Element::builder("pointer", NS_AVATAR_METADATA).build())
                    .build(),
            },
        }],
        ..event
    };
    assert!(parse_metadata_event(&pointer_only).is_none());
}

#[test]
fn transient_stanza_errors_are_failures_not_absent_avatars() {
    use crate::error::{StanzaError, StanzaErrorType};
    let error = |error_type, condition: &str| StanzaError {
        error_type,
        condition: condition.to_string(),
        text: None,
        application_condition: None,
    };
    for transient in [
        error(StanzaErrorType::Wait, "resource-constraint"),
        error(StanzaErrorType::Cancel, "remote-server-not-found"),
        error(StanzaErrorType::Wait, "remote-server-timeout"),
        error(StanzaErrorType::Cancel, "internal-server-error"),
    ] {
        let condition = transient.condition.clone();
        assert_eq!(
            AvatarRequestFailure::from_stanza_error(transient, |_| ()),
            AvatarRequestFailure::Other(()),
            "{condition}"
        );
    }
    for definitive in [
        error(StanzaErrorType::Cancel, "item-not-found"),
        error(StanzaErrorType::Auth, "forbidden"),
        error(StanzaErrorType::Cancel, "feature-not-implemented"),
        error(StanzaErrorType::Cancel, "service-unavailable"),
    ] {
        let condition = definitive.condition.clone();
        assert_eq!(
            AvatarRequestFailure::from_stanza_error(definitive, |_| ()),
            AvatarRequestFailure::<()>::StanzaError,
            "{condition}"
        );
    }
}

#[test]
fn parse_metadata_event_ignores_non_metadata_nodes() {
    let event = PubsubEvent {
        from: Some("alice@example.com".parse().expect("valid bare JID")),
        node: "urn:example:not-avatar".to_string(),
        items: Vec::new(),
    };
    assert!(parse_metadata_event(&event).is_none());
}

#[test]
fn parse_metadata_returns_none_without_info() {
    let empty_items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_METADATA,
        )
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(empty_items)
        .build();
    let iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "x")
        .append(pubsub)
        .build();
    assert!(parse_metadata_response(&iq).is_none());
}

#[test]
fn parse_metadata_rejects_wrong_node() {
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "some:other:node",
        )
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    let iq = Element::builder("iq", NS_CLIENT).append(pubsub).build();
    assert!(parse_metadata_response(&iq).is_none());
}

#[test]
fn parse_data_extracts_base64() {
    let iq = make_data_iq("deadbeef", "aGVsbG8=");
    let text = parse_data_response(&iq).expect("text");
    assert_eq!(text, "aGVsbG8=");
}

#[test]
fn parse_data_returns_none_for_empty() {
    let iq = make_data_iq("deadbeef", "");
    assert!(parse_data_response(&iq).is_none());
}

#[test]
fn parse_vcard_photo_extracts_binval_bytes() {
    let iq = make_vcard_binval_iq("image/jpeg", "aG Vs\n bG8=");
    let photo = parse_vcard_photo_response(&iq).expect("photo");
    assert_eq!(photo.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(photo.data.as_deref(), Some(b"hello".as_slice()));
}

#[test]
fn parse_vcard_photo_rejects_extval_url() {
    let iq = make_vcard_extval_iq("https://example.test/avatar.png");
    assert!(parse_vcard_photo_response(&iq).is_none());
}

#[test]
fn build_metadata_request_has_correct_shape() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let iq = build_metadata_request_iq(&jid);
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some("alice@example.com"));
    let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
    let items = pubsub.get_child("items", NS_PUBSUB).expect("items");
    assert_eq!(items.attr("node"), Some(NS_AVATAR_METADATA));
    assert_eq!(items.attr("max_items"), Some("1"));
}

#[test]
fn build_data_request_includes_item_id() {
    let jid: BareJid = "bob@example.com".parse().unwrap();
    let iq = build_data_request_iq(&jid, &item_id(HELLO_ID));
    let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
    let items = pubsub.get_child("items", NS_PUBSUB).expect("items");
    assert_eq!(items.attr("node"), Some(NS_AVATAR_DATA));
    let item = items.get_child("item", NS_PUBSUB).expect("item");
    assert_eq!(item.attr("id"), Some(HELLO_ID));
}

#[test]
fn build_vcard_request_has_correct_shape() {
    let jid: BareJid = "bob@example.com".parse().unwrap();
    let iq = build_vcard_request_iq(&jid);
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some("bob@example.com"));
    assert!(iq.get_child("vCard", NS_VCARD_TEMP).is_some());
}

#[test]
fn request_avatar_prefers_xep_0084_data() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_iq(HELLO_ID, "image/png"),
        make_data_iq(HELLO_ID, "aGVsbG8="),
        make_vcard_binval_iq("image/jpeg", "d29ybGQ="),
    ]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
        let response = responses.borrow_mut().remove(0);
        async move {
            assert_eq!(stanza.name(), "iq");
            Ok::<_, AvatarRequestFailure<()>>(response)
        }
    }))
    .unwrap()
    .expect("avatar");

    assert_eq!(avatar.id, AvatarId::Item(item_id(HELLO_ID)));
    assert_eq!(avatar.mime_type, "image/png");
    assert_eq!(avatar.data, b"hello");
    assert_eq!(responses.borrow().len(), 1);
}

#[test]
fn request_avatar_rejects_data_that_does_not_match_advertised_hash() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_iq(HELLO_ID, "image/png"),
        make_data_iq(HELLO_ID, "d29ybGQ="), // "world" under "hello"'s hash
    ]);
    let requests = std::cell::RefCell::new(Vec::new());

    let result =
        futures::executor::block_on(request_avatar_with_iq_skipping(&jid, &[], |stanza| {
            let is_vcard = stanza.get_child("vCard", NS_VCARD_TEMP).is_some();
            requests.borrow_mut().push(is_vcard);
            let response = (!is_vcard).then(|| responses.borrow_mut().remove(0));
            async move { response.ok_or(AvatarRequestFailure::<()>::StanzaError) }
        }))
        .expect("lookup succeeds");

    assert!(result.is_none(), "mismatched data must not enter the cache");
    assert_eq!(*requests.borrow(), vec![false, false, true]);
}

#[test]
fn request_avatar_accepts_uppercase_hash_without_changing_item_lookup() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let upper = HELLO_ID.to_ascii_uppercase();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_iq(&upper, "image/png"),
        make_data_iq(&upper, "aGVsbG8="),
    ]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
        if stanza
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_DATA))
        {
            let item = stanza
                .get_child("pubsub", NS_PUBSUB)
                .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
                .and_then(|items| items.get_child("item", NS_PUBSUB))
                .expect("data item");
            assert_eq!(item.attr("id"), Some(upper.as_str()));
        }
        let response = responses.borrow_mut().remove(0);
        async move { Ok::<_, AvatarRequestFailure<()>>(response) }
    }))
    .expect("lookup succeeds")
    .expect("avatar");

    assert_eq!(avatar.id, AvatarId::Item(item_id(&upper)));
    assert_eq!(avatar.data, b"hello");
}

#[test]
fn vcard_fallback_id_changes_when_photo_bytes_change() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let first = vcard_photo_to_avatar(
        &jid,
        VcardPhoto {
            mime_type: Some("image/png".to_string()),
            data: Some(b"hello".to_vec()),
        },
    )
    .expect("first photo");
    let second = vcard_photo_to_avatar(
        &jid,
        VcardPhoto {
            mime_type: Some("image/png".to_string()),
            data: Some(b"world".to_vec()),
        },
    )
    .expect("second photo");

    assert_eq!(first.id.to_string(), format!("vcard-photo:{HELLO_ID}"));
    assert_eq!(second.id.to_string(), format!("vcard-photo:{WORLD_ID}"));
    assert_ne!(first.id, second.id);
}

#[test]
fn request_avatar_url_only_metadata_falls_back_to_vcard_binval() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_url_iq(HELLO_ID, "image/png", "https://example.test/a.png"),
        make_vcard_binval_iq("image/jpeg", "d29ybGQ="),
    ]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |_stanza| {
        let response = responses.borrow_mut().remove(0);
        async move { Ok::<_, AvatarRequestFailure<()>>(response) }
    }))
    .unwrap()
    .expect("avatar");

    assert_eq!(avatar.id.to_string(), format!("vcard-photo:{WORLD_ID}"));
    assert_eq!(avatar.data, b"world");
    assert!(responses.borrow().is_empty());
}

#[test]
fn request_avatar_url_only_metadata_and_vcard_returns_none() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_url_iq(HELLO_ID, "image/png", "https://example.test/a.png"),
        make_vcard_extval_iq("https://example.test/vcard.png"),
    ]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
        let is_metadata = stanza
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_METADATA));
        let response = if is_metadata || !responses.borrow().is_empty() {
            Some(responses.borrow_mut().remove(0))
        } else {
            None
        };
        async move {
            match response {
                Some(response) => Ok::<_, AvatarRequestFailure<()>>(response),
                None => Err(AvatarRequestFailure::StanzaError),
            }
        }
    }))
    .unwrap();

    assert!(avatar.is_none());
    assert!(responses.borrow().is_empty());
}

#[test]
fn request_avatar_falls_back_to_vcard_binval() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![make_vcard_binval_iq("image/jpeg", "d29ybGQ=")]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
        let is_metadata = stanza
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_METADATA));
        let response = (!is_metadata).then(|| responses.borrow_mut().remove(0));
        async move {
            match response {
                Some(response) => Ok::<_, AvatarRequestFailure<()>>(response),
                None => Err(AvatarRequestFailure::StanzaError),
            }
        }
    }))
    .unwrap()
    .expect("avatar");

    assert_eq!(avatar.id.to_string(), format!("vcard-photo:{WORLD_ID}"));
    assert_eq!(avatar.mime_type, "image/jpeg");
    assert_eq!(avatar.data, b"world");
}

#[test]
fn request_avatar_does_not_return_vcard_extval() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses =
        std::cell::RefCell::new(vec![make_vcard_extval_iq("https://example.test/vcard.png")]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
        let is_metadata = stanza
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_METADATA));
        let response = (!is_metadata).then(|| responses.borrow_mut().remove(0));
        async move {
            match response {
                Some(response) => Ok::<_, AvatarRequestFailure<()>>(response),
                None => Err(AvatarRequestFailure::StanzaError),
            }
        }
    }))
    .unwrap();

    assert!(avatar.is_none());
}

// ── §4.2 known-id skip (request_avatar_with_iq_skipping) ─────────────────────

#[test]
fn skipping_fetch_answers_id_only_for_a_known_metadata_id() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    // Only the metadata response is provisioned: issuing the data IQ
    // (or the vCard fallback) would panic on the empty queue, so the
    // assertion below also proves no further IQ left the seam.
    let responses = std::cell::RefCell::new(vec![make_metadata_iq(HELLO_ID, "image/png")]);
    let known = vec![item_id(HELLO_ID)];

    let fetch =
        futures::executor::block_on(request_avatar_with_iq_skipping(&jid, &known, |_stanza| {
            let response = responses.borrow_mut().remove(0);
            async move { Ok::<_, AvatarRequestFailure<()>>(response) }
        }))
        .unwrap()
        .expect("fetch outcome");

    assert_eq!(fetch.id, AvatarId::Item(item_id(HELLO_ID)));
    assert_eq!(fetch.avatar, None);
    assert!(responses.borrow().is_empty());
}

#[test]
fn skipping_fetch_retrieves_data_for_an_unknown_metadata_id() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_iq(HELLO_ID, "image/png"),
        make_data_iq(HELLO_ID, "aGVsbG8="),
    ]);
    let known = vec![item_id(WORLD_ID)];

    let fetch =
        futures::executor::block_on(request_avatar_with_iq_skipping(&jid, &known, |_stanza| {
            let response = responses.borrow_mut().remove(0);
            async move { Ok::<_, AvatarRequestFailure<()>>(response) }
        }))
        .unwrap()
        .expect("fetch outcome");

    assert_eq!(fetch.id, AvatarId::Item(item_id(HELLO_ID)));
    let avatar = fetch.avatar.expect("avatar bytes fetched");
    assert_eq!(avatar.data, b"hello");
    assert!(responses.borrow().is_empty());
}

// ── Publish builders (XEP-0084 §3) ───────────────────────────────────────────

/// Walk a publish IQ down to its `<item>` and assert the envelope targets
/// `node`. Returns the item element for payload-level assertions.
fn publish_item<'a>(iq: &'a Element, node: &str) -> &'a Element {
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), None);
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish");
    assert_eq!(publish.attr("node"), Some(node));
    publish.get_child("item", NS_PUBSUB).expect("item")
}

#[test]
fn compute_avatar_item_id_matches_sha1_vectors() {
    // FIPS 180-1 "abc" vector + the empty-input digest.
    assert_eq!(
        compute_avatar_item_id(b"abc").to_string(),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        compute_avatar_item_id(b"").to_string(),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
}

#[test]
fn build_publish_avatar_data_iq_carries_base64_at_sha1_item_id() {
    let data = b"hello".as_slice();
    let item_id = compute_avatar_item_id(data);
    let iq = build_publish_avatar_data_iq(&item_id, data);
    let item = publish_item(&iq, NS_AVATAR_DATA);
    assert_eq!(item.attr("id"), Some(item_id.as_str()));
    let payload = item.get_child("data", NS_AVATAR_DATA).expect("data");
    assert_eq!(payload.text(), "aGVsbG8=");
}

#[test]
fn build_publish_avatar_metadata_iq_sets_required_info_attrs() {
    let info = AvatarPublishInfo {
        bytes: 5,
        id: item_id("a9993e364706816aba3e25717850c26c9cd0d89d"),
        mime_type: "image/png".to_string(),
        width: Some(64),
        height: Some(48),
    };
    let iq = build_publish_avatar_metadata_iq(&info);
    let item = publish_item(&iq, NS_AVATAR_METADATA);
    assert_eq!(item.attr("id"), Some(info.id.as_str()));
    let metadata = item
        .get_child("metadata", NS_AVATAR_METADATA)
        .expect("metadata");
    let info_elem = metadata
        .get_child("info", NS_AVATAR_METADATA)
        .expect("info");
    assert_eq!(info_elem.attr("bytes"), Some("5"));
    assert_eq!(info_elem.attr("id"), Some(info.id.as_str()));
    assert_eq!(info_elem.attr("type"), Some("image/png"));
    assert_eq!(info_elem.attr("width"), Some("64"));
    assert_eq!(info_elem.attr("height"), Some("48"));
}

#[test]
fn build_publish_avatar_metadata_iq_omits_unknown_dimensions() {
    let info = AvatarPublishInfo {
        bytes: 5,
        id: item_id(HELLO_ID),
        mime_type: "image/jpeg".to_string(),
        width: None,
        height: None,
    };
    let iq = build_publish_avatar_metadata_iq(&info);
    let item = publish_item(&iq, NS_AVATAR_METADATA);
    let info_elem = item
        .get_child("metadata", NS_AVATAR_METADATA)
        .and_then(|metadata| metadata.get_child("info", NS_AVATAR_METADATA))
        .expect("info");
    assert_eq!(info_elem.attr("width"), None);
    assert_eq!(info_elem.attr("height"), None);
}

#[test]
fn build_disable_avatar_iq_publishes_empty_metadata_at_current() {
    let iq = build_disable_avatar_iq();
    let item = publish_item(&iq, NS_AVATAR_METADATA);
    assert_eq!(item.attr("id"), Some(AVATAR_REMOVE_ITEM_ID));
    let metadata = item
        .get_child("metadata", NS_AVATAR_METADATA)
        .expect("metadata");
    assert_eq!(metadata.children().count(), 0);
}

#[test]
fn metadata_publish_round_trips_through_fetch_parser() {
    let data = b"round-trip".as_slice();
    let info = AvatarPublishInfo {
        bytes: u32::try_from(data.len()).expect("fits"),
        id: compute_avatar_item_id(data),
        mime_type: "image/png".to_string(),
        width: Some(32),
        height: Some(32),
    };
    // Rewrap the published payload in an items response and confirm the
    // fetch-side parser reads back exactly what was published.
    let iq = build_publish_avatar_metadata_iq(&info);
    let metadata = publish_item(&iq, NS_AVATAR_METADATA)
        .get_child("metadata", NS_AVATAR_METADATA)
        .expect("metadata")
        .clone();
    let item = Element::builder("item", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            info.id.as_str(),
        )
        .append(metadata)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_METADATA,
        )
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    let response = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
        .append(pubsub)
        .build();
    let parsed = parse_metadata_response(&response).expect("info");
    assert_eq!(parsed.id, info.id);
    assert_eq!(parsed.mime_type, info.mime_type);
    assert_eq!(parsed.bytes, Some(u64::from(info.bytes)));
    assert_eq!(parsed.width, Some(32));
    assert_eq!(parsed.height, Some(32));
}

#[test]
fn request_avatar_rejects_plaintext_vcard_extval() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses =
        std::cell::RefCell::new(vec![make_vcard_extval_iq("http://example.test/vcard.png")]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
        let is_metadata = stanza
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_METADATA));
        let response = (!is_metadata).then(|| responses.borrow_mut().remove(0));
        async move {
            match response {
                Some(response) => Ok::<_, AvatarRequestFailure<()>>(response),
                None => Err(AvatarRequestFailure::StanzaError),
            }
        }
    }))
    .unwrap();

    assert!(avatar.is_none());
}

#[test]
fn request_avatar_disabled_metadata_skips_vcard_fallback() {
    // XEP-0084 §4.3: an empty <metadata/> item disables the avatar; an old
    // vCard PHOTO must not bring it back.
    let jid: BareJid = "alice@example.com".parse().expect("valid bare JID");
    let mut requested = Vec::new();
    let fetch = futures::executor::block_on(request_avatar_with_iq_skipping(
        &jid,
        &[],
        |stanza: Element| {
            let is_vcard = stanza.get_child("vCard", NS_VCARD_TEMP).is_some();
            requested.push(is_vcard);
            async move {
                if is_vcard {
                    Ok::<Element, AvatarRequestFailure<()>>(
                        "<iq xmlns='jabber:client' type='result'><vCard xmlns='vcard-temp'><PHOTO><TYPE>image/png</TYPE><BINVAL>AQID</BINVAL></PHOTO></vCard></iq>"
                            .parse()
                            .expect("valid vcard"),
                    )
                } else {
                    Ok("<iq xmlns='jabber:client' type='result'><pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='urn:xmpp:avatar:metadata'><item id='current'><metadata xmlns='urn:xmpp:avatar:metadata'/></item></items></pubsub></iq>"
                        .parse()
                        .expect("valid metadata"))
                }
            }
        },
    ))
    .expect("fetch");
    assert!(fetch.is_none());
    assert_eq!(
        requested,
        vec![false],
        "vCard must not be queried after a disable"
    );
}

#[test]
fn request_avatar_pointer_only_metadata_does_not_disable_vcard_fallback() {
    let jid: BareJid = "alice@example.com".parse().expect("valid bare JID");
    let mut requested = Vec::new();
    let fetch = futures::executor::block_on(request_avatar_with_iq_skipping(
        &jid,
        &[],
        |stanza: Element| {
            let is_vcard = stanza.get_child("vCard", NS_VCARD_TEMP).is_some();
            requested.push(is_vcard);
            async move {
                if is_vcard {
                    Ok::<Element, AvatarRequestFailure<()>>(
                        "<iq xmlns='jabber:client' type='result'><vCard xmlns='vcard-temp'><PHOTO><TYPE>image/png</TYPE><BINVAL>AQID</BINVAL></PHOTO></vCard></iq>"
                            .parse()
                            .expect("valid vcard"),
                    )
                } else {
                    Ok("<iq xmlns='jabber:client' type='result'><pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='urn:xmpp:avatar:metadata'><item id='current'><metadata xmlns='urn:xmpp:avatar:metadata'><pointer/></metadata></item></items></pubsub></iq>"
                        .parse()
                        .expect("valid metadata"))
                }
            }
        },
    ))
    .expect("fetch")
    .expect("vcard fallback");
    assert!(matches!(fetch.id, AvatarId::VcardPhoto(_)));
    assert_eq!(requested, vec![false, true]);
}
