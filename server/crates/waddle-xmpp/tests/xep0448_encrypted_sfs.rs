//! XEP-0448: Encryption for Stateless File Sharing — dedicated suite.

use jid::BareJid;
use minidom::Element;
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

#[test]
fn encrypted_sfs_round_trip() {
    let enc = sample();
    let elem = build_encrypted_element(&enc);
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
    set_encrypted_file(&mut msg, &enc);
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
