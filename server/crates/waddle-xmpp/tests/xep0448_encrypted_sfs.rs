#![recursion_limit = "512"]

//! XEP-0448: Encryption for Stateless File Sharing — dedicated suite.

mod common;

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::parser::message_to_string;
use waddle_xmpp::xep::xep0446::FileMetadata;
use waddle_xmpp::xep::xep0447::{set_file_sharing, FileSharing};
use waddle_xmpp::xep::xep0448::{
    build_encrypted_element, extract_encrypted_file, is_encrypted_file_element,
    parse_encrypted_element, set_encrypted_file, Cipher, EncryptedFile, EncryptedFileError,
    NS_ESFS,
};
use xmpp_parsers::message::Message;

use common::{establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT};

fn sample() -> EncryptedFile {
    EncryptedFile::new(Cipher::Aes256GcmNoPadding, "a2V5", "aXY=")
        .with_hash("sha-256", "aGFzaA==")
        .with_source("https://files.example.com/blob.enc")
}

async fn read_iq_response(client: &mut RawXmppClient) -> std::io::Result<String> {
    let start = std::time::Instant::now();
    let mut response = String::new();
    loop {
        if start.elapsed() > DEFAULT_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Timeout waiting for IQ response",
            ));
        }
        response.push_str(&client.read(DEFAULT_TIMEOUT).await?);
        if response.contains("</iq>")
            || (response.contains("<iq") && response.contains("/>") && !response.contains("</iq>"))
        {
            return Ok(response);
        }
    }
}

#[test]
fn encrypted_sfs_round_trip() {
    let enc = sample();
    let elem = build_encrypted_element(&enc).unwrap();
    let parsed = parse_encrypted_element(&elem).unwrap();
    assert_eq!(parsed, enc);
}

#[test]
fn encrypted_sfs_requires_sources() {
    let elem = Element::builder("encrypted", NS_ESFS)
        .attr("cipher", Cipher::Aes256GcmNoPadding.as_uri())
        .append(Element::builder("key", NS_ESFS).append("k").build())
        .append(Element::builder("iv", NS_ESFS).append("v").build())
        .append(Element::builder("sources", waddle_xmpp::xep::xep0447::NS_SFS).build())
        .build();
    assert_eq!(
        parse_encrypted_element(&elem),
        Err(EncryptedFileError::NoSources)
    );
}

#[test]
fn encrypted_sfs_rejects_unknown_cipher() {
    let elem = Element::builder("encrypted", NS_ESFS)
        .attr("cipher", "urn:xmpp:ciphers:rot13:0")
        .build();
    assert!(matches!(
        parse_encrypted_element(&elem),
        Err(EncryptedFileError::UnknownCipher(_))
    ));
}

#[test]
fn encrypted_sfs_cipher_round_trip() {
    for c in [Cipher::Aes128GcmNoPadding, Cipher::Aes256GcmNoPadding] {
        assert_eq!(Cipher::from_uri(c.as_uri()), Some(c));
    }
}

#[test]
fn encrypted_sfs_attach_to_message() {
    let mut msg = Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<BareJid>().unwrap(),
    )));
    let enc = sample();
    set_encrypted_file(&mut msg, &enc).unwrap();
    let extracted = extract_encrypted_file(&msg).unwrap().unwrap();
    assert_eq!(extracted, enc);
}

#[test]
fn encrypted_sfs_is_detected_by_namespace() {
    assert!(is_encrypted_file_element(
        &Element::builder("encrypted", NS_ESFS).build()
    ));
    assert!(!is_encrypted_file_element(
        &Element::builder("encrypted", "other").build()
    ));
}

#[test]
fn encrypted_sfs_builder_rejects_missing_sources() {
    let enc = EncryptedFile {
        cipher: Cipher::Aes256GcmNoPadding,
        key_b64: "a2V5".into(),
        iv_b64: "aXY=".into(),
        hashes: Vec::new(),
        sources: Vec::new(),
    };
    assert!(matches!(
        build_encrypted_element(&enc),
        Err(EncryptedFileError::NoSources)
    ));
}

#[tokio::test]
async fn encrypted_sfs_passes_through_tcp_runtime_and_updates_inbox() {
    init_test_env();

    let server = TestServer::start().await;
    let mut alice = RawXmppClient::connect(server.addr)
        .await
        .expect("alice connect");
    let mut bob = RawXmppClient::connect(server.addr)
        .await
        .expect("bob connect");

    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("alice session");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bob session");
    alice
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("alice initial presence");
    bob.send("<presence xmlns='jabber:client'/>")
        .await
        .expect("bob initial presence");

    let mut msg = Message::new(Some(jid::Jid::from(
        "bob@localhost/mobile"
            .parse::<jid::FullJid>()
            .expect("recipient jid"),
    )));
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.id = Some("esfs-runtime-1".into());
    set_file_sharing(
        &mut msg,
        &FileSharing::new(
            FileMetadata::new()
                .with_name("secret.enc")
                .with_media_type("application/octet-stream"),
        )
        .with_url("https://files.example.com/secret.enc"),
    );
    set_encrypted_file(&mut msg, &sample()).expect("attach encrypted payload");

    alice
        .send(&message_to_string(&msg).expect("serialize message"))
        .await
        .expect("send encrypted sfs message");

    let delivered = bob
        .read_until("</message>", DEFAULT_TIMEOUT)
        .await
        .expect("recipient gets encrypted sfs");
    assert!(
        delivered.contains(NS_ESFS),
        "encrypted payload should pass through unchanged: {delivered}"
    );
    assert!(
        delivered.contains("urn:xmpp:sfs:0"),
        "file-sharing payload should pass through unchanged: {delivered}"
    );
    bob.clear();

    bob.send(
        "<iq xmlns='jabber:client' type='get' to='bob@localhost' id='esfs-inbox-1'>\
            <query xmlns='urn:xmpp:inbox:0'/>\
         </iq>",
    )
    .await
    .expect("send inbox query");
    let inbox = read_iq_response(&mut bob)
        .await
        .expect("read inbox response");
    assert!(
        inbox.contains("partner=\"alice@localhost\"")
            || inbox.contains("partner='alice@localhost'"),
        "encrypted sfs should still project into the inbox: {inbox}"
    );
    assert!(
        inbox.contains("unread=\"1\"") || inbox.contains("unread='1'"),
        "encrypted sfs should increment unread counts: {inbox}"
    );
    assert!(
        !inbox.contains("<preview"),
        "bodyless encrypted SFS should not invent a preview: {inbox}"
    );
}
