use super::*;
use crate::pubsub_event::{PubsubEvent, PubsubEventItem, PubsubEventPayload};

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
    let iq = make_metadata_iq("deadbeef", "image/png");
    let info = parse_metadata_response(&iq).expect("info");
    assert_eq!(info.id, "deadbeef");
    assert_eq!(info.mime_type, "image/png");
    assert_eq!(info.width, Some(64));
    assert_eq!(info.height, Some(64));
    assert_eq!(info.bytes, Some(42));
}

#[test]
fn parse_metadata_ignores_url_only_info() {
    let iq = make_metadata_url_iq("deadbeef", "image/png", "https://example.test/a.png");
    assert!(parse_metadata_response(&iq).is_none());
}

#[test]
fn parse_metadata_prefers_in_band_info_over_url_info() {
    let url_info = Element::builder("info", NS_AVATAR_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "remote")
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "https://example.test/avatar.png",
        )
        .build();
    let in_band_info = Element::builder("info", NS_AVATAR_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "in-band")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "image/png")
        .build();
    let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
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
        Some("in-band".to_string())
    );
}

#[test]
fn parse_metadata_event_reports_set_disable_retract_and_bare_jid() {
    let full_jid: jid::Jid = "alice@example.com/desktop".parse().expect("valid full JID");
    let set = PubsubEvent {
        from: Some(full_jid.clone()),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some("avatar-1".to_string()),
            retracted: false,
            payload: PubsubEventPayload::Opaque {
                element: Element::builder("metadata", NS_AVATAR_METADATA)
                    .append(
                        Element::builder("info", NS_AVATAR_METADATA)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "avatar-1")
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
            avatar_id: Some("avatar-1".to_string()),
        })
    );

    let url_only = PubsubEvent {
        from: Some("alice@example.com".parse().expect("valid bare JID")),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some("remote-avatar".to_string()),
            retracted: false,
            payload: PubsubEventPayload::Opaque {
                element: Element::builder("metadata", NS_AVATAR_METADATA)
                    .append(
                        Element::builder("info", NS_AVATAR_METADATA)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "remote-avatar")
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
        Some("remote-avatar".to_string())
    );

    let empty_metadata = PubsubEvent {
        from: Some(full_jid.clone()),
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
        from: Some(full_jid),
        node: NS_AVATAR_METADATA.to_string(),
        items: vec![PubsubEventItem {
            id: Some("avatar-1".to_string()),
            retracted: true,
            payload: PubsubEventPayload::Empty,
        }],
    };
    assert_eq!(
        parse_metadata_event(&retract).and_then(|event| event.avatar_id),
        None
    );
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
    let iq = build_data_request_iq(&jid, "cafef00d");
    let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
    let items = pubsub.get_child("items", NS_PUBSUB).expect("items");
    assert_eq!(items.attr("node"), Some(NS_AVATAR_DATA));
    let item = items.get_child("item", NS_PUBSUB).expect("item");
    assert_eq!(item.attr("id"), Some("cafef00d"));
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
        make_metadata_iq("deadbeef", "image/png"),
        make_data_iq("deadbeef", "aGVsbG8="),
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

    assert_eq!(avatar.id, "deadbeef");
    assert_eq!(avatar.mime_type, "image/png");
    assert_eq!(avatar.data, b"hello");
    assert_eq!(responses.borrow().len(), 1);
}

#[test]
fn request_avatar_url_only_metadata_falls_back_to_vcard_binval() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_url_iq("deadbeef", "image/png", "https://example.test/a.png"),
        make_vcard_binval_iq("image/jpeg", "d29ybGQ="),
    ]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |_stanza| {
        let response = responses.borrow_mut().remove(0);
        async move { Ok::<_, AvatarRequestFailure<()>>(response) }
    }))
    .unwrap()
    .expect("avatar");

    assert_eq!(avatar.id, "vcard-photo");
    assert_eq!(avatar.data, b"world");
    assert!(responses.borrow().is_empty());
}

#[test]
fn request_avatar_url_only_metadata_and_vcard_returns_none() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_url_iq("deadbeef", "image/png", "https://example.test/a.png"),
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

    assert_eq!(avatar.id, "vcard-photo");
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
    let responses = std::cell::RefCell::new(vec![make_metadata_iq("deadbeef", "image/png")]);
    let known = vec!["deadbeef".to_string()];

    let fetch =
        futures::executor::block_on(request_avatar_with_iq_skipping(&jid, &known, |_stanza| {
            let response = responses.borrow_mut().remove(0);
            async move { Ok::<_, AvatarRequestFailure<()>>(response) }
        }))
        .unwrap()
        .expect("fetch outcome");

    assert_eq!(fetch.id, "deadbeef");
    assert_eq!(fetch.avatar, None);
    assert!(responses.borrow().is_empty());
}

#[test]
fn skipping_fetch_retrieves_data_for_an_unknown_metadata_id() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![
        make_metadata_iq("deadbeef", "image/png"),
        make_data_iq("deadbeef", "aGVsbG8="),
    ]);
    let known = vec!["someoldid".to_string()];

    let fetch =
        futures::executor::block_on(request_avatar_with_iq_skipping(&jid, &known, |_stanza| {
            let response = responses.borrow_mut().remove(0);
            async move { Ok::<_, AvatarRequestFailure<()>>(response) }
        }))
        .unwrap()
        .expect("fetch outcome");

    assert_eq!(fetch.id, "deadbeef");
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
        compute_avatar_item_id(b"abc"),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        compute_avatar_item_id(b""),
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
        id: "a9993e364706816aba3e25717850c26c9cd0d89d".to_string(),
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
        id: "cafef00d".to_string(),
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
