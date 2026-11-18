//! XEP-0503 0.3.0 (xsf/xeps 4132c531487fe9450ae90b905b8df950d0d478b2),
//! "Spaces images": which Space node items are a Space's avatar and
//! banner, and what such an item may carry, a XEP-0084 `<metadata/>`
//! element (XEP-0084 1.1.5, "Metadata Element").

use minidom::Element;
use waddle_xmpp::xep::{
    space_image_payload, validate_space_image_metadata, SpaceImage, SpaceImageError,
    SpaceImageInfoPart as Part, ITEM_SPACE_AVATAR_METADATA, ITEM_SPACE_BANNER_METADATA,
    MAX_SPACE_IMAGE_FORMATS, MAX_SPACE_IMAGE_ID_LEN, MAX_SPACE_IMAGE_URL_LEN,
};

const NS_METADATA: &str = "urn:xmpp:avatar:metadata";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// An `<info/>` carrying `attributes`.
fn info(attributes: &[(&str, &str)]) -> Element {
    let mut info = Element::builder("info", NS_METADATA);
    for (name, value) in attributes {
        info = info.attr(
            minidom::rxml::NcName::try_from(*name).expect("an attribute name"),
            *value,
        );
    }
    info.build()
}

/// The `<info/>` of the avatar in XEP-0503's own example.
fn spec_avatar_info() -> Element {
    info(&[
        ("bytes", "420"),
        ("height", "64"),
        ("width", "64"),
        ("type", "image/jpeg"),
        (
            "url",
            "https://permanent-uploads.megacorp.example.com/dev/avatar.jpg",
        ),
        ("id", "sha1-of-dev.jpg"),
    ])
}

fn metadata(children: Vec<Element>) -> Element {
    let mut metadata = Element::builder("metadata", NS_METADATA);
    for child in children {
        metadata = metadata.append(child);
    }
    metadata.build()
}

fn item(payloads: Vec<Element>) -> Element {
    let mut item = Element::builder("item", NS_PUBSUB).attr(
        minidom::rxml::xml_ncname!("id").to_owned(),
        ITEM_SPACE_AVATAR_METADATA,
    );
    for payload in payloads {
        item = item.append(payload);
    }
    item.build()
}

fn refused_info(attributes: &[(&str, &str)]) -> Option<Part> {
    match validate_space_image_metadata(&metadata(vec![info(attributes)])) {
        Err(SpaceImageError::MalformedInfo(part)) => Some(part),
        Ok(()) => None,
        Err(other) => panic!("unexpected refusal {other:?}"),
    }
}

#[test]
fn the_two_image_ids_name_the_avatar_and_the_banner_and_no_other_id_does() {
    assert_eq!(
        SpaceImage::from_item_id(ITEM_SPACE_AVATAR_METADATA),
        Some(SpaceImage::Avatar)
    );
    assert_eq!(
        SpaceImage::from_item_id(ITEM_SPACE_BANNER_METADATA),
        Some(SpaceImage::Banner)
    );
    assert_eq!(SpaceImage::Avatar.item_id(), ITEM_SPACE_AVATAR_METADATA);
    assert_eq!(SpaceImage::Banner.item_id(), ITEM_SPACE_BANNER_METADATA);
    for other in [
        "dev@muc.example.com",
        "urn:xmpp:spaces:avatar:metadata:1",
        "urn:xmpp:avatar:metadata",
        "URN:XMPP:SPACES:AVATAR:METADATA:0",
        "",
    ] {
        assert_eq!(SpaceImage::from_item_id(other), None, "{other}");
    }
}

/// XEP-0503's example avatar, the federation interoperability harness's
/// image (an `http:` URL on a loopback port and an id that is no hash) and
/// an image without a URL are accepted. One `<metadata/>` lists several
/// formats.
#[test]
fn the_specifications_example_and_other_well_formed_metadata_are_accepted() {
    let spec = item(vec![metadata(vec![spec_avatar_info()])]);
    let payload = space_image_payload(&spec).expect("the example is accepted");
    assert!(payload.is("metadata", NS_METADATA));
    assert_eq!(
        payload
            .get_child("info", NS_METADATA)
            .and_then(|info| info.attr("url")),
        Some("https://permanent-uploads.megacorp.example.com/dev/avatar.jpg"),
        "the payload is kept as given"
    );

    let harness = metadata(vec![info(&[
        ("bytes", "420"),
        ("height", "64"),
        ("width", "64"),
        ("type", "image/png"),
        ("url", "http://127.0.0.1:41234/avatar.png"),
        ("id", "harness-image"),
    ])]);
    assert_eq!(validate_space_image_metadata(&harness), Ok(()));

    let in_band = metadata(vec![info(&[
        ("bytes", "12345"),
        ("id", "111f4b3c50d7b0df729d299bc6f8e9ef9066971f"),
        ("type", "image/png"),
    ])]);
    assert_eq!(validate_space_image_metadata(&in_band), Ok(()));

    let formats = metadata(
        ["image/png", "image/gif", "image/webp", "video/mp4"]
            .into_iter()
            .map(|content_type| info(&[("bytes", "1"), ("id", "a"), ("type", content_type)]))
            .collect(),
    );
    assert_eq!(validate_space_image_metadata(&formats), Ok(()));

    // Whitespace between elements is no content.
    let spaced = Element::builder("metadata", NS_METADATA)
        .append("\n  ")
        .append(info(&[("bytes", "1"), ("id", "a"), ("type", "image/png")]))
        .append("\n")
        .build();
    assert_eq!(validate_space_image_metadata(&spaced), Ok(()));
}

/// XEP-0060 §7.1.3.5: an item holds exactly one payload element. Several
/// `<metadata/>` elements are refused rather than cut to the first.
#[test]
fn an_item_without_one_payload_is_refused() {
    assert_eq!(
        space_image_payload(&item(Vec::new())).map(|_| ()),
        Err(SpaceImageError::MissingPayload)
    );
    let two = item(vec![
        metadata(vec![spec_avatar_info()]),
        metadata(vec![spec_avatar_info()]),
    ]);
    assert_eq!(
        space_image_payload(&two).map(|_| ()),
        Err(SpaceImageError::SeveralPayloads)
    );
}

#[test]
fn a_payload_that_is_no_space_image_metadata_is_refused() {
    let conference = Element::builder("conference", "urn:xmpp:bookmarks:1").build();
    assert_eq!(
        validate_space_image_metadata(&conference),
        Err(SpaceImageError::NotMetadata)
    );
    let wrong_namespace = Element::builder("metadata", "urn:xmpp:avatar:data").build();
    assert_eq!(
        validate_space_image_metadata(&wrong_namespace),
        Err(SpaceImageError::NotMetadata)
    );

    // XEP-0084's empty element stops an avatar; a Space's image is removed
    // by retracting its item.
    assert_eq!(
        validate_space_image_metadata(&metadata(Vec::new())),
        Err(SpaceImageError::NoImage)
    );

    let pointer = metadata(vec![
        spec_avatar_info(),
        Element::builder("pointer", NS_METADATA)
            .append(Element::builder("x", "http://example.com/virtualworlds").build())
            .build(),
    ]);
    assert_eq!(
        validate_space_image_metadata(&pointer),
        Err(SpaceImageError::Pointer)
    );

    let foreign_child = metadata(vec![
        spec_avatar_info(),
        Element::builder("info", "urn:example:other").build(),
    ]);
    assert_eq!(
        validate_space_image_metadata(&foreign_child),
        Err(SpaceImageError::UnexpectedContent)
    );

    let attribute = Element::builder("metadata", NS_METADATA)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "x")
        .append(spec_avatar_info())
        .build();
    assert_eq!(
        validate_space_image_metadata(&attribute),
        Err(SpaceImageError::UnexpectedContent)
    );

    let text = Element::builder("metadata", NS_METADATA)
        .append(spec_avatar_info())
        .append("text")
        .build();
    assert_eq!(
        validate_space_image_metadata(&text),
        Err(SpaceImageError::UnexpectedContent)
    );

    let too_many = metadata(
        (0..=MAX_SPACE_IMAGE_FORMATS)
            .map(|_| spec_avatar_info())
            .collect(),
    );
    assert_eq!(
        validate_space_image_metadata(&too_many),
        Err(SpaceImageError::TooManyFormats)
    );
    let most = metadata(
        (0..MAX_SPACE_IMAGE_FORMATS)
            .map(|_| spec_avatar_info())
            .collect(),
    );
    assert_eq!(validate_space_image_metadata(&most), Ok(()));
}

/// XEP-0084, "Info Element": `bytes`, `id` and `type` are required, the
/// element is empty, and its attributes are the ones it defines.
#[test]
fn an_info_element_that_xep0084_does_not_define_is_refused() {
    let required = [("bytes", "420"), ("id", "a"), ("type", "image/png")];
    for missing in ["bytes", "id", "type"] {
        let attributes: Vec<_> = required
            .iter()
            .copied()
            .filter(|(name, _)| *name != missing)
            .collect();
        let expected = match missing {
            "bytes" => Part::Bytes,
            "id" => Part::Id,
            _ => Part::Type,
        };
        assert_eq!(refused_info(&attributes), Some(expected), "{missing}");
    }

    let with = |name: &'static str, value: &'static str| {
        let mut attributes: Vec<(&str, &str)> = required
            .iter()
            .copied()
            .filter(|(present, _)| *present != name)
            .collect();
        attributes.push((name, value));
        refused_info(&attributes)
    };
    assert_eq!(with("bytes", "-1"), Some(Part::Bytes));
    assert_eq!(with("bytes", "4294967296"), Some(Part::Bytes));
    assert_eq!(with("bytes", "many"), Some(Part::Bytes));
    assert_eq!(with("height", "65536"), Some(Part::Height));
    assert_eq!(with("width", "wide"), Some(Part::Width));
    assert_eq!(with("id", ""), Some(Part::Id));
    assert_eq!(with("id", "a\u{7}b"), Some(Part::Id));
    assert_eq!(with("type", "text/html"), Some(Part::Type));
    assert_eq!(with("type", "image"), Some(Part::Type));
    assert_eq!(with("type", "image/"), Some(Part::Type));
    assert_eq!(with("type", "image/png; charset=x"), Some(Part::Type));
    assert_eq!(with("url", "ftp://example.com/a.png"), Some(Part::Url));
    assert_eq!(with("url", "javascript:alert(1)"), Some(Part::Url));
    assert_eq!(with("url", "data:image/png;base64,AAAA"), Some(Part::Url));
    assert_eq!(with("url", "/relative/a.png"), Some(Part::Url));
    assert_eq!(with("url", "file:///etc/passwd"), Some(Part::Url));
    assert_eq!(with("status", "x"), Some(Part::Attribute));

    assert_eq!(with("height", "65535"), None);
    assert_eq!(with("type", "IMAGE/PNG"), None);
    assert_eq!(with("type", "image/svg+xml"), None);
    assert_eq!(with("url", "HTTPS://example.com/a.png"), None);

    let long_id = "a".repeat(MAX_SPACE_IMAGE_ID_LEN + 1);
    let longest_id = "a".repeat(MAX_SPACE_IMAGE_ID_LEN);
    let base = [("bytes", "1"), ("type", "image/png")];
    assert_eq!(
        refused_info(&[base[0], base[1], ("id", long_id.as_str())]),
        Some(Part::Id)
    );
    assert_eq!(
        refused_info(&[base[0], base[1], ("id", longest_id.as_str())]),
        None
    );

    let prefix = "https://example.com/";
    let long_url = [
        prefix,
        &"a".repeat(MAX_SPACE_IMAGE_URL_LEN - prefix.len() + 1),
    ]
    .concat();
    let longest_url = [prefix, &"a".repeat(MAX_SPACE_IMAGE_URL_LEN - prefix.len())].concat();
    let with_url = |url: &str| refused_info(&[base[0], base[1], ("id", "a"), ("url", url)]);
    assert_eq!(with_url(&long_url), Some(Part::Url));
    assert_eq!(with_url(&longest_url), None);

    let with_child = metadata(vec![Element::builder("info", NS_METADATA)
        .attr(minidom::rxml::xml_ncname!("bytes").to_owned(), "1")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "a")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "image/png")
        .append("text")
        .build()]);
    assert_eq!(
        validate_space_image_metadata(&with_child),
        Err(SpaceImageError::MalformedInfo(Part::Content))
    );
}
