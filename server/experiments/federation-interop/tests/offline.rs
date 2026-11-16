//! Checks of the harness's own pieces that need no server: the stream
//! reader and writer, the ServerHello reader, the evidence rules and the
//! manifest. Run with the harness workspace's `cargo test`; they do not
//! start any service.

use std::collections::BTreeSet;

use waddle_federation_interop::evidence::{scrub, settle, Outcome, Status, Unverifiable};
use waddle_federation_interop::manifest::{self, Drive, Slice};
use waddle_federation_interop::net::tls_hello::parse_server_hello;
use waddle_federation_interop::scenarios::support::count_matching;
use waddle_federation_interop::watch::OpenWatch;
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

#[test]
fn evidence_text_loses_paths_connection_strings_and_secrets() {
    let tail = "error: open /tmp/waddle-federation-interop-1/pki/home-a.test.key: denied\n\
        WADDLE_DATABASE_URL=postgres://waddle:hunter2@127.0.0.1:47111/waddle\n\
        dialing wss://home-a.test:47112/ws as user@host failed\n\
        {\"password\":\"0123456789abcdef0123456789abcdef\"} config=/home/someone/s2s.toml\n\
        relay 127.0.0.1:47150/x kept password: hunter3 next";
    let scrubbed = scrub(tail);
    for leaked in [
        "/tmp/",
        "/home/someone",
        "hunter2",
        "hunter3",
        "postgres://",
        "0123456789abcdef0123456789abcdef",
    ] {
        assert!(!scrubbed.contains(leaked), "{leaked} kept in {scrubbed}");
    }
    assert!(scrubbed.contains("[path]/home-a.test.key"));
    assert!(scrubbed.contains("[path]/s2s.toml"));
    assert!(scrubbed.contains("relay 127.0.0.1:47150/x kept"));
    assert!(scrubbed.contains("password: [redacted] next"));
    assert!(scrubbed.contains("wss://home-a.test:47112/ws"));
    let long = "x".repeat(10_000);
    assert!(scrub(&long).chars().count() < 2_000);
    let mut outcome = Outcome::new("driver at /var/lib/harness/run");
    outcome.note("session_key=abcdef");
    assert!(!outcome.driven_by.contains("/var/lib"));
    assert_eq!(outcome.notes, vec!["session_key=[redacted]".to_owned()]);
}

#[test]
fn log_needles_match_waddle_json_lines() {
    // The shape of Waddle's JSON log lines (tracing-subscriber `json()`
    // with targets) for the events the rows count.
    let log = [
        r#"{"timestamp":"t","level":"DEBUG","fields":{"message":"s2s connection ended","connection":4,"direction":"Inbound","outcome":"Capacity"},"target":"waddle_server::s2s::conn::session"}"#,
        r#"{"timestamp":"t","level":"DEBUG","fields":{"message":"s2s peer certificate refused","code":"identity_mismatch"},"target":"waddle_server::s2s::conn::session"}"#,
        r#"{"timestamp":"t","level":"DEBUG","fields":{"message":"s2s stream established","connection":5,"direction":"Inbound","strength":"pkix"},"target":"waddle_server::s2s::conn::session"}"#,
        r#"{"timestamp":"t","level":"INFO","fields":{"message":"listening"},"target":"waddle_server::server"}"#,
    ]
    .join("\n");
    assert_eq!(
        count_matching(&log, &["s2s connection ended", "\"capacity\""]),
        1
    );
    assert_eq!(
        count_matching(&log, &["s2s connection ended", "\"evicted\""]),
        0
    );
    assert_eq!(
        count_matching(
            &log,
            &["s2s peer certificate refused", "\"identity_mismatch\""]
        ),
        1
    );
    assert_eq!(
        count_matching(&log, &["s2s peer certificate refused", "\"path\""]),
        0
    );
    assert_eq!(
        count_matching(&log, &["s2s stream established", "inbound", "pkix"]),
        1
    );
    assert_eq!(count_matching(&log, &["\"waddle_server::s2s"]), 3);
}

#[test]
fn an_open_watch_sees_reads_of_watched_files_only() {
    let directory =
        std::env::temp_dir().join(format!("federation-interop-watch-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a directory");
    let watched = directory.join("watched");
    let other = directory.join("other");
    std::fs::write(&watched, b"x").expect("a file");
    std::fs::write(&other, b"y").expect("a file");
    let mut watch = OpenWatch::new(std::slice::from_ref(&watched)).expect("inotify");
    assert!(watch.touched().expect("events").is_empty());
    std::fs::read(&other).expect("read");
    assert!(watch.touched().expect("events").is_empty());
    std::fs::read(&watched).expect("read");
    assert_eq!(watch.touched().expect("events"), vec!["watched".to_owned()]);
    std::fs::remove_dir_all(&directory).ok();
}
