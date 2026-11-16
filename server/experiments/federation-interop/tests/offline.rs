//! Checks of the harness's own pieces that need no server: the stream
//! reader and writer, the ServerHello reader, the evidence rules and the
//! manifest. Run with the harness workspace's `cargo test`; they do not
//! start any service.

use std::collections::BTreeSet;

use waddle_federation_interop::evidence::{settle, Outcome, Status, Unverifiable};
use waddle_federation_interop::manifest::{self, Drive, Slice};
use waddle_federation_interop::net::tls_hello::parse_server_hello;
use waddle_federation_interop::xml::framer::{Framer, Item};
use waddle_federation_interop::xml::header::{self, StreamOpen};
use waddle_federation_interop::xml::{ns, stanza};

#[test]
fn a_written_header_and_stanza_read_back() {
    let mut bytes = StreamOpen::server(Some("peer-raw.test"), "home-a.test")
        .with_dialback()
        .encode()
        .expect("an encodable header");
    bytes.extend(header::element_bytes(&stanza::ping("p1", "home-a.test")));
    bytes.extend_from_slice(header::STREAM_CLOSE);
    let mut framer = Framer::new();
    framer.push(&bytes);
    let mut items = Vec::new();
    while let Some(item) = framer.next_item().expect("well-formed") {
        items.push(item);
    }
    match items.first() {
        Some(Item::Header(header)) => {
            assert_eq!(header.attribute("from").as_deref(), Some("peer-raw.test"));
            assert_eq!(header.attribute("to").as_deref(), Some("home-a.test"));
            assert_eq!(header.namespace, ns::STREAM);
        }
        other => panic!("expected a header first, got {other:?}"),
    }
    assert!(
        matches!(items.get(1), Some(Item::Element(element)) if element.attr("id") == Some("p1"))
    );
    assert!(matches!(items.last(), Some(Item::Close)));
}

#[test]
fn a_stream_split_at_every_byte_reads_the_same() {
    let mut bytes = StreamOpen::client("prosody.test")
        .encode()
        .expect("a header");
    bytes.extend(header::element_bytes(&stanza::chat(
        "m1",
        "alice@home-a.test",
        "hi",
    )));
    let mut framer = Framer::new();
    let mut elements = 0;
    for byte in &bytes {
        framer.push(std::slice::from_ref(byte));
        while let Some(item) = framer.next_item().expect("well-formed") {
            elements += usize::from(matches!(item, Item::Element(_)));
        }
    }
    assert_eq!(elements, 1);
}

#[test]
fn a_tls13_server_hello_reports_its_supported_version() {
    // ServerHello with legacy_version 0x0303 and supported_versions 0x0304.
    let mut body = vec![0x03, 0x03];
    body.extend([0u8; 32]);
    body.push(0);
    body.extend([0x13, 0x01, 0x00]);
    body.extend([0x00, 0x06, 0x00, 0x2b, 0x00, 0x02, 0x03, 0x04]);
    let mut handshake = vec![0x02, 0x00, 0x00, u8::try_from(body.len()).expect("short")];
    handshake.extend(&body);
    let mut record = vec![
        0x16,
        0x03,
        0x03,
        0x00,
        u8::try_from(handshake.len()).expect("short"),
    ];
    record.extend(&handshake);
    let hello = parse_server_hello(&record).expect("a ServerHello");
    assert_eq!(hello.version, 0x0304);
    assert_eq!(hello.cipher_suite, 0x1301);
}

#[test]
fn a_row_passes_only_when_every_check_ran_and_passed() {
    let empty = Outcome::new("nobody").finish();
    assert_eq!(empty.status, Status::Unverified);
    let mut mixed = Outcome::new("harness");
    mixed.check("a", true, "");
    mixed.unverified("b", "missing");
    assert_eq!(mixed.finish().status, Status::Unverified);
    let mut failed = Outcome::new("harness");
    failed.check("a", true, "");
    failed.check("b", false, "");
    assert_eq!(failed.finish().status, Status::Fail);
    let prerequisite = settle(Err(Unverifiable("no Prosody".to_owned()).into()), "driver");
    assert_eq!(prerequisite.status, Status::Unverified);
    let error = settle(Err(anyhow::anyhow!("refused")), "driver");
    assert_eq!(error.status, Status::Fail);
}

#[test]
fn manifest_ids_are_unique_and_unlanded_slices_gate_rows() {
    let mut ids = BTreeSet::new();
    for row in manifest::ROWS {
        assert!(ids.insert(row.id), "duplicate row {}", row.id);
        assert!(!row.peers.is_empty(), "row {} runs against no peer", row.id);
        if let Drive::NotDriven(reason) = row.drive {
            assert!(!reason.is_empty(), "row {} gives no reason", row.id);
        }
    }
    assert!(!manifest::landed(Slice::W6c) || std::env::var("INTEROP_LANDED").is_ok());
}
