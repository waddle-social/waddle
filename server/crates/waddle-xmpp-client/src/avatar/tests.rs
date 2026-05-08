use super::*;

fn make_metadata_iq(id: &str, mime: &str) -> Element {
    let info = Element::builder("info", NS_AVATAR_METADATA)
        .attr("id", id)
        .attr("type", mime)
        .attr("bytes", "42")
        .attr("width", "64")
        .attr("height", "64")
        .build();
    let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(info)
        .build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", id)
        .append(metadata)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr("node", NS_AVATAR_METADATA)
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "result")
        .attr("id", "abc")
        .append(pubsub)
        .build()
}

fn make_metadata_url_iq(id: &str, mime: &str, url: &str) -> Element {
    let info = Element::builder("info", NS_AVATAR_METADATA)
        .attr("id", id)
        .attr("type", mime)
        .attr("bytes", "42")
        .attr("url", url)
        .build();
    let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(info)
        .build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", id)
        .append(metadata)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr("node", NS_AVATAR_METADATA)
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "result")
        .attr("id", "abc")
        .append(pubsub)
        .build()
}

fn make_data_iq(id: &str, base64_data: &str) -> Element {
    let data = Element::builder("data", NS_AVATAR_DATA)
        .append(minidom::Node::Text(base64_data.to_string()))
        .build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", id)
        .append(data)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr("node", NS_AVATAR_DATA)
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "result")
        .attr("id", "abc")
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
        .attr("type", "result")
        .attr("id", "abc")
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
        .attr("type", "result")
        .attr("id", "abc")
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
    assert_eq!(info.url, None);
}

#[test]
fn parse_metadata_extracts_url() {
    let iq = make_metadata_url_iq("deadbeef", "image/png", "https://example.test/a.png");
    let info = parse_metadata_response(&iq).expect("info");
    assert_eq!(info.url.as_deref(), Some("https://example.test/a.png"));
}

#[test]
fn parse_metadata_returns_none_without_info() {
    let empty_items = Element::builder("items", NS_PUBSUB)
        .attr("node", NS_AVATAR_METADATA)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(empty_items)
        .build();
    let iq = Element::builder("iq", NS_CLIENT)
        .attr("type", "result")
        .attr("id", "x")
        .append(pubsub)
        .build();
    assert!(parse_metadata_response(&iq).is_none());
}

#[test]
fn parse_metadata_rejects_wrong_node() {
    let items = Element::builder("items", NS_PUBSUB)
        .attr("node", "some:other:node")
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
    assert_eq!(photo.url, None);
}

#[test]
fn parse_vcard_photo_extracts_extval_url() {
    let iq = make_vcard_extval_iq("https://example.test/avatar.png");
    let photo = parse_vcard_photo_response(&iq).expect("photo");
    assert_eq!(photo.data, None);
    assert_eq!(
        photo.url.as_deref(),
        Some("https://example.test/avatar.png")
    );
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
    assert_eq!(avatar.url, None);
    assert_eq!(responses.borrow().len(), 1);
}

#[test]
fn request_avatar_returns_xep_0084_url_without_data_request() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![make_metadata_url_iq(
        "deadbeef",
        "image/png",
        "https://example.test/a.png",
    )]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |_stanza| {
        let response = responses.borrow_mut().remove(0);
        async move { Ok::<_, AvatarRequestFailure<()>>(response) }
    }))
    .unwrap()
    .expect("avatar");

    assert_eq!(avatar.id, "deadbeef");
    assert_eq!(avatar.data, Vec::<u8>::new());
    assert_eq!(avatar.url.as_deref(), Some("https://example.test/a.png"));
    assert!(responses.borrow().is_empty());
}

#[test]
fn request_avatar_rejects_plaintext_xep_0084_url() {
    let jid: BareJid = "alice@example.com".parse().unwrap();
    let responses = std::cell::RefCell::new(vec![make_metadata_url_iq(
        "deadbeef",
        "image/png",
        "http://example.test/a.png",
    )]);

    let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
        let is_metadata = stanza
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_METADATA));
        let response = is_metadata.then(|| responses.borrow_mut().remove(0));
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
    assert_eq!(avatar.url, None);
}

#[test]
fn request_avatar_falls_back_to_vcard_extval() {
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
    .unwrap()
    .expect("avatar");

    assert_eq!(avatar.data, Vec::<u8>::new());
    assert_eq!(
        avatar.url.as_deref(),
        Some("https://example.test/vcard.png")
    );
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
