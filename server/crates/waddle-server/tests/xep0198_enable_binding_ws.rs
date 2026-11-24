#![cfg(feature = "test-support")]

use minidom::Element;
use std::path::Path;
use std::time::Duration;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;

const ACCOUNTS: &[(&str, &str)] = &[("alice", "enable-binding-alice")];
const JID: &str = "alice@localhost/phone";

async fn authenticate(server: &TestServer) -> WsXmppClient {
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("connect");
    client
        .authenticate("localhost", ACCOUNTS[0].0, ACCOUNTS[0].1)
        .await
        .expect("authenticate");
    client
}

async fn send_element(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize");
    client
        .send(&String::from_utf8(bytes).expect("UTF-8"))
        .await
        .expect("send");
}

/// The content of the file the server's test seam writes at `path`, once
/// it holds one. The seam writes with `std::fs::write`, which creates the
/// file before it writes the content, so an empty read is a write in
/// progress, not the content.
async fn wait_for_file(path: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(path) {
                if !value.is_empty() {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("test seam reached")
}

async fn assert_no_enabled_before_close(client: &mut WsXmppClient) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.recv_raw_timeout(Duration::from_secs(5)).await {
                Ok(Some(frame)) if frame.is_close() => return,
                Ok(Some(frame)) => {
                    if let Ok(text) = frame.to_text() {
                        let element: Element = text.parse().expect("server XML");
                        assert!(
                            !element.is("enabled", SM_NS),
                            "retired binding sent enabled"
                        );
                    }
                }
                Ok(None) => return,
                Err(error) if !error.contains("Timeout") => return,
                Err(error) => panic!("old connection did not close: {error}"),
            }
        }
    })
    .await
    .expect("old connection closed");
}

async fn stream_row_count(pool: &sqlx::SqlitePool, stream: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM ingress_sm_streams WHERE stream_id = ?")
        .bind(stream)
        .fetch_one(pool)
        .await
        .expect("read exact enrollment")
}

#[tokio::test]
async fn replacement_revokes_before_enable_start_and_orphan_is_eventually_retired() {
    let dir = tempfile::tempdir().expect("test directory");
    let db = dir.path().join("database.sqlite");
    let db_url = format!("sqlite://{}?mode=rwc", db.display());
    let seam = dir.path().to_str().expect("temp path");
    let server = TestServer::start_persistent_with_extra_envs(
        &db_url,
        ACCOUNTS,
        &[
            ("WADDLE_TEST_SEAM_SM_ENABLE_DIRECTORY", seam),
            ("WADDLE_TEST_SEAM_SM_ENABLE_JID", JID),
        ],
    );
    let mut old = authenticate(&server).await;
    assert_eq!(old.bind("phone").await.expect("old bind"), JID);
    send_element(
        &mut old,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let stream = wait_for_file(&dir.path().join("held")).await;
    let pool = sqlx::SqlitePool::connect(&db_url)
        .await
        .expect("observe SQLite");
    assert_eq!(
        stream_row_count(&pool, &stream).await,
        1,
        "real enrollment committed before the hold of {stream:?}"
    );

    let mut replacement = authenticate(&server).await;
    let bind = tokio::spawn(async move {
        replacement.bind("phone").await.expect("replacement bind");
        replacement
    });
    let revoked_stream = wait_for_file(&dir.path().join("revoked")).await;
    assert_eq!(revoked_stream, stream);
    std::fs::write(dir.path().join("release"), "continue").expect("release old start");
    assert_no_enabled_before_close(&mut old).await;
    let mut replacement = tokio::time::timeout(Duration::from_secs(10), bind)
        .await
        .expect("replacement did not deadlock")
        .expect("task");
    send_element(
        &mut replacement,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled: Element = replacement
        .recv()
        .await
        .expect("successor enabled")
        .parse()
        .expect("XML");
    assert!(enabled.is("enabled", SM_NS));
    assert_ne!(enabled.attr("id"), Some(stream.as_str()));

    tokio::time::timeout(Duration::from_secs(80), async {
        while stream_row_count(&pool, &stream).await != 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("SM janitor retired the refused enable's exact enrollment");
}

#[tokio::test]
async fn successful_enable_starts_before_later_replacement() {
    let dir = tempfile::tempdir().expect("test directory");
    let seam = dir.path().to_str().expect("temp path");
    let server = TestServer::start_with_extra_envs(
        ACCOUNTS,
        &[
            ("WADDLE_TEST_SEAM_SM_ENABLE_DIRECTORY", seam),
            ("WADDLE_TEST_SEAM_SM_ENABLE_JID", JID),
        ],
    );
    let mut old = authenticate(&server).await;
    old.bind("phone").await.expect("bind");
    send_element(
        &mut old,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    wait_for_file(&dir.path().join("held")).await;
    std::fs::write(dir.path().join("release"), "continue").expect("release");
    let enabled: Element = old.recv().await.expect("enabled").parse().expect("XML");
    assert!(enabled.is("enabled", SM_NS));
    let mut replacement = authenticate(&server).await;
    tokio::time::timeout(Duration::from_secs(10), replacement.bind("phone"))
        .await
        .expect("replacement did not deadlock")
        .expect("bind");
    assert_no_enabled_before_close(&mut old).await;
}

#[tokio::test]
async fn prebind_failure_and_nonresumable_enable_keep_their_normal_responses() {
    let dir = tempfile::tempdir().expect("test directory");
    let seam = dir.path().to_str().expect("temp path");
    let server = TestServer::start_with_extra_envs(
        ACCOUNTS,
        &[
            ("WADDLE_TEST_SEAM_SM_ENABLE_DIRECTORY", seam),
            ("WADDLE_TEST_SEAM_SM_ENABLE_JID", JID),
        ],
    );
    let mut client = authenticate(&server).await;
    send_element(
        &mut client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let failed: Element = client
        .recv()
        .await
        .expect("prebind failure")
        .parse()
        .expect("XML");
    assert!(failed.is("failed", SM_NS));
    assert_eq!(client.bind("phone").await.expect("bind"), JID);
    send_element(
        &mut client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "false")
            .build(),
    )
    .await;
    let enabled: Element = client
        .recv()
        .await
        .expect("non-resumable enabled")
        .parse()
        .expect("XML");
    assert!(enabled.is("enabled", SM_NS));
    assert!(
        !dir.path().join("held").exists(),
        "non-resumable enable does not enter the resumable seam"
    );
}

const STANZA_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const STREAM_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-streams";

/// Every text frame the server sends until it closes the connection.
async fn frames_until_close(client: &mut WsXmppClient) -> Vec<Element> {
    let mut frames = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.recv_raw_timeout(Duration::from_secs(5)).await {
                Ok(Some(frame)) if frame.is_close() => return,
                Ok(Some(frame)) => {
                    if let Ok(text) = frame.to_text() {
                        frames.push(text.parse().expect("server XML"));
                    }
                }
                Ok(None) => return,
                Err(error) if !error.contains("Timeout") => return,
                Err(error) => panic!("connection did not close: {error}"),
            }
        }
    })
    .await
    .expect("connection closed");
    frames
}

fn stream_error_condition(frames: &[Element]) -> String {
    let error = frames
        .iter()
        .find(|frame| frame.name() == "error")
        .expect("a stream error was sent");
    error
        .children()
        .find(|child| child.ns() == STREAM_ERRORS)
        .expect("a defined stream error condition")
        .name()
        .to_owned()
}

fn assert_failed(frame: &Element, condition: &str) {
    assert!(frame.is("failed", SM_NS), "expected <failed/>: {frame:?}");
    assert!(
        frame.get_child(condition, STANZA_ERRORS).is_some(),
        "expected <{condition}/> in {frame:?}"
    );
}

/// XEP-0198 §5 requires 'previd' and 'h' on `<resume/>`; §9 says an error
/// with regard to `<resume/>` MUST be answered with `<failed/>`. A malformed
/// request must not leave the client waiting, and the stream stays in its
/// pre-bind state so the client can still bind.
#[tokio::test]
async fn malformed_resume_is_answered_with_failed_and_the_stream_stays_usable() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let mut client = authenticate(&server).await;
    for malformed in [
        "<resume xmlns='urn:xmpp:sm:3' previd='some-id'/>",
        "<resume xmlns='urn:xmpp:sm:3' previd='some-id' h='abc'/>",
        "<resume xmlns='urn:xmpp:sm:3' previd='some-id' h='-1'/>",
        "<resume xmlns='urn:xmpp:sm:3' previd='some-id' h=''/>",
        "<resume xmlns='urn:xmpp:sm:3' h='0'/>",
        "<resume xmlns='urn:xmpp:sm:3'/>",
    ] {
        client.send(malformed).await.expect("send");
        let reply: Element = client
            .recv()
            .await
            .unwrap_or_else(|error| panic!("no reply to {malformed}: {error}"))
            .parse()
            .expect("XML");
        assert_failed(&reply, "bad-request");
    }
    assert_eq!(client.bind("phone").await.expect("bind"), JID);
}

/// XEP-0198 §4: "An <a/> element MUST possess an 'h' attribute." There is no
/// `<failed/>` for an ack, so the violation closes the stream.
#[tokio::test]
async fn ack_without_a_valid_h_closes_the_stream() {
    for malformed in [
        "<a xmlns='urn:xmpp:sm:3'/>",
        "<a xmlns='urn:xmpp:sm:3' h='abc'/>",
    ] {
        let server = TestServer::start_with_extra_accounts(ACCOUNTS);
        let mut client = authenticate(&server).await;
        assert_eq!(client.bind("phone").await.expect("bind"), JID);
        send_element(
            &mut client,
            Element::builder("enable", SM_NS)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "false")
                .build(),
        )
        .await;
        let enabled: Element = client.recv().await.expect("enabled").parse().expect("XML");
        assert!(enabled.is("enabled", SM_NS));

        client.send(malformed).await.expect("send");
        let frames = frames_until_close(&mut client).await;
        assert_eq!(stream_error_condition(&frames), "bad-format", "{malformed}");
    }
}

/// XEP-0198 §3: "a client SHALL only make at most one attempt to enable
/// stream management. If a server receives a second <enable/> element it
/// SHOULD respond with a stream error, thus terminating the client
/// connection." The spec's example answers it with `<failed/>` first.
#[tokio::test]
async fn second_enable_is_answered_with_failed_then_a_stream_error_and_close() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    let mut client = authenticate(&server).await;
    assert_eq!(client.bind("phone").await.expect("bind"), JID);
    let enable = || {
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "false")
            .build()
    };
    send_element(&mut client, enable()).await;
    let enabled: Element = client.recv().await.expect("enabled").parse().expect("XML");
    assert!(enabled.is("enabled", SM_NS));

    send_element(&mut client, enable()).await;
    let frames = frames_until_close(&mut client).await;
    assert_failed(&frames[0], "unexpected-request");
    assert_eq!(stream_error_condition(&frames), "policy-violation");
}
