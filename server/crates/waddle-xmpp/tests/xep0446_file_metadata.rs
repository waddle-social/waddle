//! XEP-0446: File Metadata Element — dedicated suite.
//!
//! Pins:
//! - the registrar namespace `urn:xmpp:file:metadata:0`,
//! - the `<file/>` wire shape with namespaced children
//!   (`media-type`, `name`, `size`, `width`, `height`, `desc`),
//! - build → serialize → reparse round-trips,
//! - parser robustness: malformed numerics, foreign-namespace
//!   children, and empty text values all degrade to `None` instead of
//!   panicking or fabricating values,
//! - single-payload replace semantics of `set_file_metadata`.

use minidom::Element;
use waddle_xmpp::xep::xep0446::{
    build_file_metadata_element, extract_file_metadata_from_message, has_file_metadata,
    is_file_metadata_element, parse_file_metadata_element, set_file_metadata, strip_file_metadata,
    FileMetadata, FileMetadataCarrier, NS_FILE_METADATA,
};
use xmpp_parsers::message::Message;

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0446_namespace_matches_spec() {
    // xep-0446.xml registrar entry.
    assert_eq!(NS_FILE_METADATA, "urn:xmpp:file:metadata:0");
}

// ── Round-trips ──────────────────────────────────────────────────────

#[test]
fn xep0446_full_metadata_survives_serialize_reparse_round_trip() {
    let original = FileMetadata::new()
        .with_media_type("image/png")
        .with_name("summit.png")
        .with_size(6144)
        .with_dimensions(800, 600)
        .with_desc("Picture of 24th XSF Summit")
        .with_duration(0);

    let elem = build_file_metadata_element(&original);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    assert!(is_file_metadata_element(&reparsed));
    let parsed = parse_file_metadata_element(&reparsed);
    assert_eq!(parsed.media_type.as_deref(), Some("image/png"));
    assert_eq!(parsed.name.as_deref(), Some("summit.png"));
    assert_eq!(parsed.size, Some(6144));
    assert_eq!(parsed.width, Some(800));
    assert_eq!(parsed.height, Some(600));
    assert_eq!(parsed.desc.as_deref(), Some("Picture of 24th XSF Summit"));
}

#[test]
fn xep0446_builder_emits_namespaced_children_only() {
    // Every child of `<file/>` lives in the file-metadata namespace —
    // a conformant peer parser matches on (name, ns) pairs.
    let meta = FileMetadata::new()
        .with_media_type("application/pdf")
        .with_name("report.pdf")
        .with_size(1048576);
    let elem = build_file_metadata_element(&meta);

    assert_eq!(elem.name(), "file");
    assert_eq!(elem.ns(), NS_FILE_METADATA);
    for child in elem.children() {
        assert_eq!(
            child.ns(),
            NS_FILE_METADATA,
            "child <{}> must live in the file-metadata namespace",
            child.name()
        );
    }
}

#[test]
fn xep0446_empty_metadata_builds_childless_element() {
    let elem = build_file_metadata_element(&FileMetadata::new());
    assert!(is_file_metadata_element(&elem));
    assert_eq!(elem.children().count(), 0);

    let parsed = parse_file_metadata_element(&elem);
    assert_eq!(parsed, FileMetadata::new());
}

// ── Parser robustness ────────────────────────────────────────────────

#[test]
fn xep0446_malformed_size_degrades_to_none() {
    let elem: Element = "<file xmlns='urn:xmpp:file:metadata:0'>\
            <name>bad.bin</name>\
            <size>not-a-number</size>\
        </file>"
        .parse()
        .expect("valid xml");
    let parsed = parse_file_metadata_element(&elem);
    assert_eq!(parsed.name.as_deref(), Some("bad.bin"));
    assert_eq!(parsed.size, None, "unparseable size must become None");
}

#[test]
fn xep0446_negative_size_degrades_to_none() {
    // Sizes are u64 on the wire; a negative value must not wrap.
    let elem: Element = "<file xmlns='urn:xmpp:file:metadata:0'><size>-42</size></file>"
        .parse()
        .expect("valid xml");
    assert_eq!(parse_file_metadata_element(&elem).size, None);
}

#[test]
fn xep0446_foreign_namespace_children_are_ignored() {
    // A `<name>` in an attacker-chosen namespace must not override
    // the real metadata namespace lookup.
    let elem: Element = "<file xmlns='urn:xmpp:file:metadata:0'>\
            <name xmlns='urn:xmpp:evil:0'>evil.exe</name>\
            <size>10</size>\
        </file>"
        .parse()
        .expect("valid xml");
    let parsed = parse_file_metadata_element(&elem);
    assert_eq!(parsed.name, None, "foreign-ns <name> child must be ignored");
    assert_eq!(parsed.size, Some(10));
}

#[test]
fn xep0446_empty_text_children_become_none() {
    // `<name/>` with no text must round-trip to `None`, not
    // `Some("")` — an empty string would falsely satisfy "has name".
    let elem: Element = "<file xmlns='urn:xmpp:file:metadata:0'><name/><desc></desc></file>"
        .parse()
        .expect("valid xml");
    let parsed = parse_file_metadata_element(&elem);
    assert_eq!(parsed.name, None);
    assert_eq!(parsed.desc, None);
}

#[test]
fn xep0446_classifier_rejects_wrong_namespace_and_name() {
    let wrong_ns = Element::builder("file", "jabber:client").build();
    assert!(!is_file_metadata_element(&wrong_ns));

    let wrong_name = Element::builder("metadata", NS_FILE_METADATA).build();
    assert!(!is_file_metadata_element(&wrong_name));
}

// ── Message-level carrier behaviour ──────────────────────────────────

#[test]
fn xep0446_message_round_trip_via_carrier_trait() {
    let mut msg = Message::new(None::<jid::Jid>);
    let meta = FileMetadata::new()
        .with_media_type("audio/ogg")
        .with_name("voice.ogg")
        .with_duration(37);
    set_file_metadata(&mut msg, &meta);

    assert!(has_file_metadata(&msg));
    assert!(msg.has_file_metadata());
    let extracted = msg.file_metadata().expect("metadata present");
    assert_eq!(extracted, meta);
    assert!(extracted.is_audio());
}

#[test]
fn xep0446_set_replaces_prior_payload_keeping_exactly_one() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_file_metadata(&mut msg, &FileMetadata::new().with_name("v1.txt"));
    set_file_metadata(&mut msg, &FileMetadata::new().with_name("v2.txt"));

    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| e.ns() == NS_FILE_METADATA)
            .count(),
        1,
        "set_file_metadata must keep exactly one <file/> payload"
    );
    assert_eq!(
        extract_file_metadata_from_message(&msg)
            .expect("metadata present")
            .name
            .as_deref(),
        Some("v2.txt")
    );
}

#[test]
fn xep0446_strip_removes_all_metadata_payloads() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_file_metadata(&mut msg, &FileMetadata::new().with_name("x"));
    strip_file_metadata(&mut msg);
    assert!(!has_file_metadata(&msg));
    assert!(extract_file_metadata_from_message(&msg).is_none());
}

// ── Media-type helpers and display ───────────────────────────────────

#[test]
fn xep0446_media_type_prefix_matching_is_exact() {
    // "imageX/foo" must not be classified as an image — the check is
    // on the "image/" prefix, not a loose substring.
    let not_image = FileMetadata::new().with_media_type("imageX/foo");
    assert!(!not_image.is_image());

    let svg = FileMetadata::new().with_media_type("image/svg+xml");
    assert!(svg.is_image());
}

#[test]
fn xep0446_human_size_boundaries() {
    let cases: [(u64, &str); 5] = [
        (1023, "1023 B"),
        (1024, "1.0 KB"),
        (1024 * 1024 - 1, "1024.0 KB"),
        (1024 * 1024, "1.0 MB"),
        (3 * 1024 * 1024 * 1024, "3.0 GB"),
    ];
    for (size, expected) in cases {
        assert_eq!(
            FileMetadata::new().with_size(size).human_size().as_deref(),
            Some(expected),
            "human_size({size})"
        );
    }
}
