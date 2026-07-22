//! XEP-0448: Encryption for Stateless File Sharing — dedicated suite.
//!
//! Note: `server/TODO.md` previously claimed this suite existed; it did
//! not. This file is the actual dedicated coverage.
//!
//! Pins:
//! - the registrar namespace `urn:xmpp:esfs:0` and the two cipher URIs
//!   Waddle implements (`aes-128/256-gcm-nopadding`),
//! - the `<encrypted cipher>` wire shape carrying `<key/>`, `<iv/>`,
//!   XEP-0300 `<hash/>` children, and the XEP-0447 `<sources/>` list,
//! - build → serialize → reparse round-trips,
//! - the full typed error surface (`EncryptedFileError`): wrong
//!   element, missing attribute/child, unknown cipher, and the
//!   no-sources invariant on both build and parse.

use minidom::Element;
use waddle_xmpp::xep::xep0300::NS_HASHES;
use waddle_xmpp::xep::xep0446::NS_FILE_METADATA;
use waddle_xmpp::xep::xep0447::{NS_SFS, NS_URL_DATA};
use waddle_xmpp::xep::xep0448::{
    build_encrypted_element, extract_encrypted_file, is_encrypted_file_element,
    parse_encrypted_element, set_encrypted_file, Cipher, EncryptedFile, EncryptedFileError,
    NS_ESFS,
};
use xmpp_parsers::message::Message;

fn sample() -> EncryptedFile {
    EncryptedFile::new(Cipher::Aes256GcmNoPadding, "a2V5", "aXY=")
        .with_hash("sha-256", "aGFzaA==")
        .with_source("https://files.example.com/blob.enc")
}

fn message_with_file_sharing() -> Message {
    let mut msg = Message::new(None::<jid::Jid>);
    let file = Element::builder("file", NS_FILE_METADATA)
        .append(
            Element::builder("name", NS_FILE_METADATA)
                .append("plain.jpg")
                .build(),
        )
        .append(
            Element::builder("size", NS_FILE_METADATA)
                .append("1234")
                .build(),
        )
        .append(
            Element::builder("hash", NS_HASHES)
                .attr(minidom::rxml::xml_ncname!("algo").to_owned(), "sha-256")
                .append("cGxhaW4taGFzaA==")
                .build(),
        )
        .build();
    msg.payloads.push(
        Element::builder("file-sharing", NS_SFS)
            .append(file)
            .append(Element::builder("sources", NS_SFS).build())
            .build(),
    );
    msg
}

fn message_with_plain_url_source() -> Message {
    let mut msg = message_with_file_sharing();
    let sources = msg.payloads[0]
        .get_child_mut("sources", NS_SFS)
        .expect("sources");
    sources.append_child(
        Element::builder("url-data", NS_URL_DATA)
            .attr(
                minidom::rxml::xml_ncname!("target").to_owned(),
                "https://files.example.com/plain.jpg",
            )
            .build(),
    );
    msg
}

// ── Namespace + cipher URI exactness ─────────────────────────────────

#[test]
fn xep0448_namespace_matches_spec() {
    assert_eq!(NS_ESFS, "urn:xmpp:esfs:0");
}

#[test]
fn xep0448_cipher_uris_match_spec_registry() {
    // xep-0448.xml registers these cipher URIs verbatim.
    assert_eq!(
        Cipher::Aes128GcmNoPadding.as_uri(),
        "urn:xmpp:ciphers:aes-128-gcm-nopadding:0"
    );
    assert_eq!(
        Cipher::Aes256GcmNoPadding.as_uri(),
        "urn:xmpp:ciphers:aes-256-gcm-nopadding:0"
    );
    for c in [Cipher::Aes128GcmNoPadding, Cipher::Aes256GcmNoPadding] {
        assert_eq!(Cipher::from_uri(c.as_uri()), Some(c));
    }
    assert_eq!(Cipher::from_uri("urn:xmpp:ciphers:rot13:0"), None);
}

// ── Round-trips ──────────────────────────────────────────────────────

#[test]
fn xep0448_envelope_survives_serialize_reparse_round_trip() {
    let original = sample();
    let elem = build_encrypted_element(&original).expect("has sources");
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    assert!(is_encrypted_file_element(&reparsed));
    let parsed = parse_encrypted_element(&reparsed).expect("parses");
    assert_eq!(parsed, original);
}

#[test]
fn xep0448_builder_nests_children_in_spec_namespaces() {
    let elem = build_encrypted_element(&sample()).expect("has sources");

    assert_eq!(elem.name(), "encrypted");
    assert_eq!(elem.ns(), NS_ESFS);
    assert_eq!(
        elem.attr("cipher"),
        Some("urn:xmpp:ciphers:aes-256-gcm-nopadding:0")
    );
    assert_eq!(
        elem.get_child("key", NS_ESFS).map(|k| k.text()),
        Some("a2V5".to_owned())
    );
    assert_eq!(
        elem.get_child("iv", NS_ESFS).map(|iv| iv.text()),
        Some("aXY=".to_owned())
    );

    // The hash child follows XEP-0300, not the esfs namespace.
    let hash = elem
        .get_child("hash", NS_HASHES)
        .expect("<hash/> in the XEP-0300 namespace");
    assert_eq!(hash.attr("algo"), Some("sha-256"));
    assert_eq!(hash.text(), "aGFzaA==");

    // The sources wrapper follows XEP-0447.
    assert!(elem.get_child("sources", NS_SFS).is_some());
}

#[test]
fn xep0448_multiple_hashes_and_sources_round_trip() {
    let original = EncryptedFile::new(Cipher::Aes128GcmNoPadding, "aw==", "dg==")
        .with_hash("sha-256", "aGFzaDE=")
        .with_hash("sha3-256", "aGFzaDI=")
        .with_source("https://cdn1.example.com/blob.enc")
        .with_source("https://cdn2.example.com/blob.enc");

    let elem = build_encrypted_element(&original).expect("has sources");
    let parsed = parse_encrypted_element(&elem).expect("parses");
    assert_eq!(parsed.hashes.len(), 2);
    assert_eq!(parsed.sources.len(), 2);
    assert_eq!(parsed, original);
}

// ── Typed error surface ──────────────────────────────────────────────

#[test]
fn xep0448_parse_rejects_wrong_element() {
    let wrong_name = Element::builder("cyphertext", NS_ESFS).build();
    assert_eq!(
        parse_encrypted_element(&wrong_name),
        Err(EncryptedFileError::WrongElement)
    );

    let wrong_ns = Element::builder("encrypted", "urn:xmpp:esfs:1").build();
    assert_eq!(
        parse_encrypted_element(&wrong_ns),
        Err(EncryptedFileError::WrongElement)
    );
    assert!(!is_encrypted_file_element(&wrong_ns));
}

#[test]
fn xep0448_parse_requires_cipher_attribute() {
    let elem: Element = "<encrypted xmlns='urn:xmpp:esfs:0'>\
            <key>a2V5</key><iv>aXY=</iv>\
        </encrypted>"
        .parse()
        .expect("valid xml");
    assert_eq!(
        parse_encrypted_element(&elem),
        Err(EncryptedFileError::MissingAttribute("cipher"))
    );
}

#[test]
fn xep0448_parse_rejects_unknown_cipher_with_typed_error() {
    let elem: Element = "<encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:rot13:0'/>"
        .parse()
        .expect("valid xml");
    assert_eq!(
        parse_encrypted_element(&elem),
        Err(EncryptedFileError::UnknownCipher(
            "urn:xmpp:ciphers:rot13:0".to_owned()
        ))
    );
}

#[test]
fn xep0448_parse_requires_key_iv_and_sources_children() {
    let cipher = Cipher::Aes256GcmNoPadding.as_uri();

    let no_key: Element =
        format!("<encrypted xmlns='urn:xmpp:esfs:0' cipher='{cipher}'><iv>aXY=</iv></encrypted>")
            .parse()
            .expect("valid xml");
    assert_eq!(
        parse_encrypted_element(&no_key),
        Err(EncryptedFileError::MissingChild("key"))
    );

    let no_iv: Element =
        format!("<encrypted xmlns='urn:xmpp:esfs:0' cipher='{cipher}'><key>a2V5</key></encrypted>")
            .parse()
            .expect("valid xml");
    assert_eq!(
        parse_encrypted_element(&no_iv),
        Err(EncryptedFileError::MissingChild("iv"))
    );

    let no_sources: Element = format!(
        "<encrypted xmlns='urn:xmpp:esfs:0' cipher='{cipher}'><key>a2V5</key><iv>aXY=</iv></encrypted>"
    )
    .parse()
    .expect("valid xml");
    assert_eq!(
        parse_encrypted_element(&no_sources),
        Err(EncryptedFileError::MissingChild("sources"))
    );
}

#[test]
fn xep0448_parse_rejects_empty_sources_list() {
    let cipher = Cipher::Aes256GcmNoPadding.as_uri();
    let elem: Element = format!(
        "<encrypted xmlns='urn:xmpp:esfs:0' cipher='{cipher}'>\
            <key>a2V5</key><iv>aXY=</iv>\
            <sources xmlns='urn:xmpp:sfs:0'/>\
        </encrypted>"
    )
    .parse()
    .expect("valid xml");
    assert_eq!(
        parse_encrypted_element(&elem),
        Err(EncryptedFileError::NoSources)
    );
}

#[test]
fn xep0448_parse_requires_algo_on_hash_children() {
    let cipher = Cipher::Aes256GcmNoPadding.as_uri();
    let elem: Element = format!(
        "<encrypted xmlns='urn:xmpp:esfs:0' cipher='{cipher}'>\
            <key>a2V5</key><iv>aXY=</iv>\
            <hash xmlns='urn:xmpp:hashes:2'>aGFzaA==</hash>\
            <sources xmlns='urn:xmpp:sfs:0'>\
                <url-data xmlns='http://jabber.org/protocol/url-data' target='https://x.example/b'/>\
            </sources>\
        </encrypted>"
    )
    .parse()
    .expect("valid xml");
    assert_eq!(
        parse_encrypted_element(&elem),
        Err(EncryptedFileError::MissingAttribute("algo"))
    );
}

#[test]
fn xep0448_build_refuses_envelope_without_sources() {
    let enc = EncryptedFile::new(Cipher::Aes128GcmNoPadding, "a2V5", "aXY=");
    assert_eq!(
        build_encrypted_element(&enc),
        Err(EncryptedFileError::NoSources),
        "an encrypted envelope with no download source is undeliverable"
    );
}

// ── Message-level behaviour ──────────────────────────────────────────

#[test]
fn xep0448_message_set_and_extract_round_trip() {
    let mut msg = message_with_file_sharing();
    let enc = sample();
    set_encrypted_file(&mut msg, &enc).expect("has sources");

    assert!(
        msg.payloads
            .iter()
            .all(|payload| !is_encrypted_file_element(payload)),
        "XEP-0448 encrypted must not be a direct message child"
    );
    let file_sharing = msg
        .payloads
        .iter()
        .find(|payload| payload.is("file-sharing", NS_SFS))
        .expect("file-sharing payload");
    let sources = file_sharing
        .get_child("sources", NS_SFS)
        .expect("file-sharing sources");
    assert!(
        sources.get_child("url-data", NS_URL_DATA).is_none(),
        "encrypted file-sharing must not expose a direct outer url-data source"
    );
    assert!(
        sources.children().any(is_encrypted_file_element),
        "encrypted is nested inside file-sharing sources"
    );
    let inner_url_data = sources
        .get_child("encrypted", NS_ESFS)
        .and_then(|encrypted| encrypted.get_child("sources", NS_SFS))
        .and_then(|encrypted_sources| encrypted_sources.get_child("url-data", NS_URL_DATA))
        .expect("ciphertext url-data inside encrypted sources");
    assert_eq!(
        inner_url_data.attr("target"),
        Some("https://files.example.com/blob.enc")
    );

    let extracted = extract_encrypted_file(&msg)
        .expect("payload present")
        .expect("payload parses");
    assert_eq!(extracted, enc);
}

#[test]
fn xep0448_set_preserves_the_mandatory_plaintext_hash_in_file_metadata() {
    // XEP-0448 §2.1: the `<file/>` metadata of an encrypted transfer MUST
    // carry at least one plaintext `<hash/>` (distinct from the ciphertext
    // hash nested inside `<encrypted/>`). Pin that embedding the envelope
    // leaves the plaintext hash in place alongside the ciphertext hash.
    let mut msg = message_with_file_sharing();
    set_encrypted_file(&mut msg, &sample()).expect("has sources");

    let file_sharing = msg
        .payloads
        .iter()
        .find(|payload| payload.is("file-sharing", NS_SFS))
        .expect("file-sharing payload");
    let plaintext_hash = file_sharing
        .get_child("file", NS_FILE_METADATA)
        .and_then(|file| file.get_child("hash", NS_HASHES))
        .expect("plaintext <hash/> inside <file/>");
    assert_eq!(plaintext_hash.attr("algo"), Some("sha-256"));
    assert_eq!(plaintext_hash.text(), "cGxhaW4taGFzaA==");

    let ciphertext_hash = file_sharing
        .get_child("sources", NS_SFS)
        .and_then(|sources| sources.get_child("encrypted", NS_ESFS))
        .and_then(|encrypted| encrypted.get_child("hash", NS_HASHES))
        .expect("ciphertext <hash/> inside <encrypted/>");
    assert_eq!(ciphertext_hash.text(), "aGFzaA==");
}

#[test]
fn xep0448_set_removes_stale_outer_url_data_for_encrypted_file() {
    let mut msg = message_with_plain_url_source();
    set_encrypted_file(&mut msg, &sample()).expect("has sources");

    let file_sharing = msg
        .payloads
        .iter()
        .find(|payload| payload.is("file-sharing", NS_SFS))
        .expect("file-sharing payload");
    let sources = file_sharing
        .get_child("sources", NS_SFS)
        .expect("file-sharing sources");
    assert!(
        sources.get_child("url-data", NS_URL_DATA).is_none(),
        "outer XEP-0447 sources must not contain a bare URL for encrypted files"
    );
    let nested_url = sources
        .get_child("encrypted", NS_ESFS)
        .and_then(|encrypted| encrypted.get_child("sources", NS_SFS))
        .and_then(|encrypted_sources| encrypted_sources.get_child("url-data", NS_URL_DATA))
        .and_then(|url_data| url_data.attr("target"));
    assert_eq!(nested_url, Some("https://files.example.com/blob.enc"));
}

#[test]
fn xep0448_set_refuses_to_fabricate_file_sharing_payload() {
    let mut msg = Message::new(None::<jid::Jid>);
    let enc = sample();

    assert_eq!(
        set_encrypted_file(&mut msg, &enc),
        Err(EncryptedFileError::MissingChild("file-sharing"))
    );
    assert!(
        msg.payloads.is_empty(),
        "failed set must not create placeholder file-sharing"
    );
}

#[test]
fn xep0448_extract_absent_returns_none() {
    let msg = Message::new(None::<jid::Jid>);
    assert!(extract_encrypted_file(&msg).is_none());
}

#[test]
fn xep0448_set_propagates_no_sources_error() {
    let mut msg = Message::new(None::<jid::Jid>);
    let enc = EncryptedFile::new(Cipher::Aes256GcmNoPadding, "a2V5", "aXY=");
    assert_eq!(
        set_encrypted_file(&mut msg, &enc),
        Err(EncryptedFileError::NoSources)
    );
    assert!(
        msg.payloads.is_empty(),
        "failed set must not leave a partial payload behind"
    );
}
