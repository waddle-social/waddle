//! XEP-0447: Stateless File Sharing — dedicated suite.
//!
//! Pins:
//! - the registrar namespace `urn:xmpp:sfs:0` and the XEP-0103
//!   `http://jabber.org/protocol/url-data` source namespace,
//! - the `<file-sharing disposition='...'>` wire shape wrapping a
//!   XEP-0446 `<file/>` and a `<sources/>` list,
//! - build → serialize → reparse round-trips including multi-source
//!   ordering,
//! - parser robustness: missing `<file/>` child, foreign-namespace
//!   `url-data`, empty `target`, and unknown `disposition` values.

use minidom::Element;
use waddle_xmpp::xep::xep0446::FileMetadata;
use waddle_xmpp::xep::xep0447::{
    build_file_sharing_element, extract_file_sharing_from_message, has_file_sharing,
    is_file_sharing_element, parse_file_sharing_element, set_file_sharing, strip_file_sharing,
    Disposition, FileSharing, FileSharingCarrier, Source, NS_SFS, NS_URL_DATA,
};
use xmpp_parsers::message::Message;

fn sample_sharing() -> FileSharing {
    FileSharing::new(
        FileMetadata::new()
            .with_media_type("image/png")
            .with_name("photo.png")
            .with_size(12345),
    )
    .with_url("https://files.example.com/photo.png")
    .with_disposition(Disposition::Inline)
}

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0447_namespaces_match_spec() {
    // xep-0447.xml pins `urn:xmpp:sfs:0`; sources reuse the XEP-0103
    // url-data namespace.
    assert_eq!(NS_SFS, "urn:xmpp:sfs:0");
    assert_eq!(NS_URL_DATA, "http://jabber.org/protocol/url-data");
}

// ── Round-trips ──────────────────────────────────────────────────────

#[test]
fn xep0447_full_sharing_survives_serialize_reparse_round_trip() {
    let original = sample_sharing();
    let elem = build_file_sharing_element(&original);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    let parsed = parse_file_sharing_element(&reparsed).expect("parses");
    assert_eq!(parsed, original);
}

#[test]
fn xep0447_builder_nests_sources_and_metadata_in_spec_namespaces() {
    let elem = build_file_sharing_element(&sample_sharing());

    assert_eq!(elem.name(), "file-sharing");
    assert_eq!(elem.ns(), NS_SFS);
    assert_eq!(elem.attr("disposition"), Some("inline"));

    let file = elem
        .get_child("file", waddle_xmpp::xep::xep0446::NS_FILE_METADATA)
        .expect("<file/> child in the XEP-0446 namespace");
    assert!(file
        .get_child("name", waddle_xmpp::xep::xep0446::NS_FILE_METADATA)
        .is_some());

    let sources = elem
        .get_child("sources", NS_SFS)
        .expect("<sources/> child in the SFS namespace");
    let url_data = sources
        .get_child("url-data", NS_URL_DATA)
        .expect("<url-data/> in the url-data namespace");
    assert_eq!(
        url_data.attr("target"),
        Some("https://files.example.com/photo.png")
    );
}

#[test]
fn xep0447_multiple_sources_preserve_order() {
    let sharing = FileSharing::new(FileMetadata::new().with_name("file.zip"))
        .with_url("https://cdn1.example.com/file.zip")
        .with_url("https://cdn2.example.com/file.zip")
        .with_url("https://cdn3.example.com/file.zip");

    let elem = build_file_sharing_element(&sharing);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("reparses");
    let parsed = parse_file_sharing_element(&reparsed).expect("parses");

    let urls: Vec<&str> = parsed.sources.iter().filter_map(Source::as_url).collect();
    assert_eq!(
        urls,
        vec![
            "https://cdn1.example.com/file.zip",
            "https://cdn2.example.com/file.zip",
            "https://cdn3.example.com/file.zip",
        ],
        "source order is meaningful (preference order) and must survive"
    );
    assert_eq!(
        parsed.first_url(),
        Some("https://cdn1.example.com/file.zip")
    );
}

// ── Disposition semantics ────────────────────────────────────────────

#[test]
fn xep0447_missing_disposition_stays_unspecified() {
    // XEP-0447 §4: absence is its own state (receiver MAY display
    // inline) — it must not be collapsed into an asserted `inline`.
    let elem: Element = "<file-sharing xmlns='urn:xmpp:sfs:0'>\
            <file xmlns='urn:xmpp:file:metadata:0'><name>a.txt</name></file>\
        </file-sharing>"
        .parse()
        .expect("valid xml");
    let parsed = parse_file_sharing_element(&elem).expect("parses");
    assert_eq!(parsed.disposition, None);
    assert!(!parsed.is_inline());

    // Reserialization must not invent a disposition the sender never sent.
    let rebuilt = build_file_sharing_element(&parsed);
    assert_eq!(rebuilt.attr("disposition"), None);
}

#[test]
fn xep0447_unknown_disposition_value_is_treated_as_unspecified() {
    // An unrecognized attribute value must not fail the whole parse;
    // the receiver treats it like an absent disposition.
    let elem: Element = "<file-sharing xmlns='urn:xmpp:sfs:0' disposition='sideways'>\
            <file xmlns='urn:xmpp:file:metadata:0'><name>a.txt</name></file>\
        </file-sharing>"
        .parse()
        .expect("valid xml");
    let parsed = parse_file_sharing_element(&elem).expect("parses");
    assert_eq!(parsed.disposition, None);
}

#[test]
fn xep0447_disposition_attribute_round_trip() {
    assert_eq!(
        Disposition::from_str_attr("inline"),
        Some(Disposition::Inline)
    );
    assert_eq!(
        Disposition::from_str_attr("attachment"),
        Some(Disposition::Attachment)
    );
    assert_eq!(Disposition::from_str_attr("Inline"), None, "case-sensitive");
    for d in [Disposition::Inline, Disposition::Attachment] {
        assert_eq!(Disposition::from_str_attr(d.as_str()), Some(d));
    }
}

// ── Parser robustness ────────────────────────────────────────────────

#[test]
fn xep0447_parse_requires_file_metadata_child() {
    // Without the XEP-0446 `<file/>` there is nothing being shared;
    // the parser must reject rather than fabricate empty metadata.
    let elem: Element = "<file-sharing xmlns='urn:xmpp:sfs:0'>\
            <sources><url-data xmlns='http://jabber.org/protocol/url-data' target='https://x.example/f'/></sources>\
        </file-sharing>"
        .parse()
        .expect("valid xml");
    assert!(parse_file_sharing_element(&elem).is_none());
}

#[test]
fn xep0447_parse_rejects_wrong_wrapper_namespace() {
    let elem: Element = "<file-sharing xmlns='urn:xmpp:sfs:1'>\
            <file xmlns='urn:xmpp:file:metadata:0'><name>a</name></file>\
        </file-sharing>"
        .parse()
        .expect("valid xml");
    assert!(!is_file_sharing_element(&elem));
    assert!(parse_file_sharing_element(&elem).is_none());
}

#[test]
fn xep0447_foreign_namespace_url_data_is_ignored() {
    let elem: Element = "<file-sharing xmlns='urn:xmpp:sfs:0'>\
            <file xmlns='urn:xmpp:file:metadata:0'><name>a.txt</name></file>\
            <sources>\
                <url-data xmlns='urn:xmpp:evil:0' target='https://evil.example/x'/>\
                <url-data xmlns='http://jabber.org/protocol/url-data' target='https://good.example/a.txt'/>\
            </sources>\
        </file-sharing>"
        .parse()
        .expect("valid xml");
    let parsed = parse_file_sharing_element(&elem).expect("parses");
    assert_eq!(
        parsed.sources,
        vec![Source::url("https://good.example/a.txt")],
        "foreign-ns url-data must be dropped"
    );
}

#[test]
fn xep0447_empty_target_source_is_dropped() {
    let elem: Element = "<file-sharing xmlns='urn:xmpp:sfs:0'>\
            <file xmlns='urn:xmpp:file:metadata:0'><name>a.txt</name></file>\
            <sources>\
                <url-data xmlns='http://jabber.org/protocol/url-data' target=''/>\
            </sources>\
        </file-sharing>"
        .parse()
        .expect("valid xml");
    let parsed = parse_file_sharing_element(&elem).expect("parses");
    assert!(parsed.sources.is_empty());
    assert_eq!(parsed.first_url(), None);
}

// ── Message-level behaviour ──────────────────────────────────────────

#[test]
fn xep0447_message_round_trip_via_carrier_trait() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_file_sharing(&mut msg, &sample_sharing());

    assert!(has_file_sharing(&msg));
    assert!(msg.has_file_sharing());
    let sharing = msg.file_sharing().expect("sharing present");
    assert_eq!(sharing, sample_sharing());
}

#[test]
fn xep0447_set_replaces_prior_payload_and_strip_clears() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_file_sharing(&mut msg, &sample_sharing());
    set_file_sharing(
        &mut msg,
        &FileSharing::new(FileMetadata::new().with_name("replacement.jpg")),
    );

    assert_eq!(msg.payloads.iter().filter(|e| e.ns() == NS_SFS).count(), 1);
    assert_eq!(
        extract_file_sharing_from_message(&msg)
            .expect("sharing present")
            .metadata
            .name
            .as_deref(),
        Some("replacement.jpg")
    );

    strip_file_sharing(&mut msg);
    assert!(!has_file_sharing(&msg));
}
