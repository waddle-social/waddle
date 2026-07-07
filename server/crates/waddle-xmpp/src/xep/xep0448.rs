//! XEP-0448: Encryption for Stateless File Sharing.
//!
//! Layers end-to-end encryption metadata over XEP-0447 file sharing: the
//! ciphertext lives at the HTTP source URL, and the sender publishes the
//! `cipher`/`key`/`iv`/`hashes` needed to decrypt it once downloaded.
//!
//! ## XML Format
//!
//! ```xml
//! <encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:aes-128-gcm-nopadding:0'>
//!   <key>BASE64(key)</key>
//!   <iv>BASE64(iv)</iv>
//!   <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>BASE64(digest)</hash>
//!   <sources xmlns='urn:xmpp:sfs:0'>
//!     <url-data xmlns='http://jabber.org/protocol/url-data'
//!               target='https://files.example.com/blob.enc'/>
//!   </sources>
//! </encrypted>
//! ```
//!
//! This module stays purely structural: it carries the metadata envelope as
//! a typed value; the transport pipe for the ciphertext is still the HTTP
//! upload slot from XEP-0363.

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

use super::xep0300::NS_HASHES;
use super::xep0447::{NS_SFS, NS_URL_DATA};

/// Namespace for XEP-0448 Encryption for Stateless File Sharing.
pub const NS_ESFS: &str = "urn:xmpp:esfs:0";

/// Symmetric ciphers callers may advertise. The XEP leaves the set open;
/// we enumerate the ones Waddle actually supports end-to-end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cipher {
    /// `urn:xmpp:ciphers:aes-128-gcm-nopadding:0`
    Aes128GcmNoPadding,
    /// `urn:xmpp:ciphers:aes-256-gcm-nopadding:0`
    Aes256GcmNoPadding,
}

impl Cipher {
    pub fn as_uri(&self) -> &'static str {
        match self {
            Cipher::Aes128GcmNoPadding => "urn:xmpp:ciphers:aes-128-gcm-nopadding:0",
            Cipher::Aes256GcmNoPadding => "urn:xmpp:ciphers:aes-256-gcm-nopadding:0",
        }
    }

    pub fn from_uri(raw: &str) -> Option<Cipher> {
        Some(match raw {
            "urn:xmpp:ciphers:aes-128-gcm-nopadding:0" => Cipher::Aes128GcmNoPadding,
            "urn:xmpp:ciphers:aes-256-gcm-nopadding:0" => Cipher::Aes256GcmNoPadding,
            _ => return None,
        })
    }
}

/// Hash algorithm used for XEP-0300 `<hash/>` elements nested under the
/// encrypted envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedHash {
    pub algo: String,
    pub value_b64: String,
}

/// A typed encrypted stateless file-sharing envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedFile {
    pub cipher: Cipher,
    /// Base64-encoded symmetric key.
    pub key_b64: String,
    /// Base64-encoded initialization vector / nonce.
    pub iv_b64: String,
    /// One or more content hashes (typically the ciphertext digest).
    pub hashes: Vec<EncryptedHash>,
    /// HTTPS URLs the ciphertext can be fetched from.
    pub sources: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EncryptedFileError {
    #[error("expected <encrypted/> in '{NS_ESFS}'")]
    WrongElement,
    #[error("missing '{0}' child")]
    MissingChild(&'static str),
    #[error("missing '{0}' attribute")]
    MissingAttribute(&'static str),
    #[error("unknown cipher '{0}'")]
    UnknownCipher(String),
    #[error("no download sources present")]
    NoSources,
}

impl EncryptedFile {
    pub fn new(cipher: Cipher, key_b64: impl Into<String>, iv_b64: impl Into<String>) -> Self {
        Self {
            cipher,
            key_b64: key_b64.into(),
            iv_b64: iv_b64.into(),
            hashes: Vec::new(),
            sources: Vec::new(),
        }
    }

    pub fn with_hash(mut self, algo: impl Into<String>, value_b64: impl Into<String>) -> Self {
        self.hashes.push(EncryptedHash {
            algo: algo.into(),
            value_b64: value_b64.into(),
        });
        self
    }

    pub fn with_source(mut self, url: impl Into<String>) -> Self {
        self.sources.push(url.into());
        self
    }
}

/// Build the `<encrypted/>` payload element from a typed value.
pub fn build_encrypted_element(enc: &EncryptedFile) -> Result<Element, EncryptedFileError> {
    if enc.sources.is_empty() {
        return Err(EncryptedFileError::NoSources);
    }
    let mut builder = Element::builder("encrypted", NS_ESFS).attr(
        minidom::rxml::xml_ncname!("cipher").to_owned(),
        enc.cipher.as_uri(),
    );
    builder = builder.append(
        Element::builder("key", NS_ESFS)
            .append(enc.key_b64.as_str())
            .build(),
    );
    builder = builder.append(
        Element::builder("iv", NS_ESFS)
            .append(enc.iv_b64.as_str())
            .build(),
    );
    for hash in &enc.hashes {
        builder = builder.append(
            Element::builder("hash", NS_HASHES)
                .attr(
                    minidom::rxml::xml_ncname!("algo").to_owned(),
                    hash.algo.as_str(),
                )
                .append(hash.value_b64.as_str())
                .build(),
        );
    }
    let mut sources = Element::builder("sources", NS_SFS);
    for url in &enc.sources {
        sources = sources.append(
            Element::builder("url-data", NS_URL_DATA)
                .attr(
                    minidom::rxml::xml_ncname!("target").to_owned(),
                    url.as_str(),
                )
                .build(),
        );
    }
    builder = builder.append(sources.build());
    Ok(builder.build())
}

pub fn parse_encrypted_element(elem: &Element) -> Result<EncryptedFile, EncryptedFileError> {
    if !elem.is("encrypted", NS_ESFS) {
        return Err(EncryptedFileError::WrongElement);
    }
    let cipher_raw = elem
        .attr("cipher")
        .ok_or(EncryptedFileError::MissingAttribute("cipher"))?;
    let cipher = Cipher::from_uri(cipher_raw)
        .ok_or_else(|| EncryptedFileError::UnknownCipher(cipher_raw.to_string()))?;

    let key_b64 = elem
        .get_child("key", NS_ESFS)
        .ok_or(EncryptedFileError::MissingChild("key"))?
        .text();
    let iv_b64 = elem
        .get_child("iv", NS_ESFS)
        .ok_or(EncryptedFileError::MissingChild("iv"))?
        .text();

    let mut hashes = Vec::new();
    for child in elem.children() {
        if child.is("hash", NS_HASHES) {
            hashes.push(EncryptedHash {
                algo: child
                    .attr("algo")
                    .ok_or(EncryptedFileError::MissingAttribute("algo"))?
                    .to_string(),
                value_b64: child.text(),
            });
        }
    }

    let sources_elem = elem
        .get_child("sources", NS_SFS)
        .ok_or(EncryptedFileError::MissingChild("sources"))?;
    let mut sources = Vec::new();
    for child in sources_elem.children() {
        if child.is("url-data", NS_URL_DATA) {
            let target = child
                .attr("target")
                .ok_or(EncryptedFileError::MissingAttribute("target"))?
                .to_string();
            sources.push(target);
        }
    }
    if sources.is_empty() {
        return Err(EncryptedFileError::NoSources);
    }

    Ok(EncryptedFile {
        cipher,
        key_b64,
        iv_b64,
        hashes,
        sources,
    })
}

pub fn is_encrypted_file_element(elem: &Element) -> bool {
    elem.is("encrypted", NS_ESFS)
}

/// Attach an `<encrypted/>` envelope inside a XEP-0447
/// `<file-sharing><sources/>` payload.
pub fn set_encrypted_file(
    msg: &mut Message,
    enc: &EncryptedFile,
) -> Result<(), EncryptedFileError> {
    let encrypted = build_encrypted_element(enc)?;
    msg.payloads
        .retain(|payload| !is_encrypted_file_element(payload));

    if let Some(file_sharing) = msg
        .payloads
        .iter_mut()
        .find(|payload| payload.is("file-sharing", NS_SFS))
    {
        if let Some(sources) = file_sharing.get_child_mut("sources", NS_SFS) {
            sources.remove_child("encrypted", NS_ESFS);
            sources.append_child(encrypted);
        } else {
            let mut sources = Element::builder("sources", NS_SFS).build();
            sources.append_child(encrypted);
            file_sharing.append_child(sources);
        }
        return Ok(());
    }

    Err(EncryptedFileError::MissingChild("file-sharing"))
}

/// Extract the first `<encrypted/>` element from a XEP-0447 source list.
pub fn extract_encrypted_file(msg: &Message) -> Option<Result<EncryptedFile, EncryptedFileError>> {
    msg.payloads
        .iter()
        .filter(|payload| payload.is("file-sharing", NS_SFS))
        .filter_map(|file_sharing| file_sharing.get_child("sources", NS_SFS))
        .flat_map(Element::children)
        .find(|child| is_encrypted_file_element(child))
        .map(parse_encrypted_element)
}

#[cfg(test)]
mod tests {
    use super::super::xep0446::NS_FILE_METADATA;
    use super::*;

    fn sample() -> EncryptedFile {
        EncryptedFile::new(Cipher::Aes256GcmNoPadding, "a2V5", "aXY=")
            .with_hash("sha-256", "aGFzaA==")
            .with_source("https://files.example.com/blob.enc")
    }

    fn message_with_file_sharing(to: jid::Jid) -> Message {
        let mut msg = Message::new(Some(to));
        let file = Element::builder("file", NS_FILE_METADATA)
            .append(
                Element::builder("name", NS_FILE_METADATA)
                    .append("plain.bin")
                    .build(),
            )
            .append(
                Element::builder("size", NS_FILE_METADATA)
                    .append("123")
                    .build(),
            )
            .append(
                Element::builder("hash", NS_HASHES)
                    .attr(minidom::rxml::xml_ncname!("algo").to_owned(), "sha-256")
                    .append("cGxhaW4=")
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

    #[test]
    fn test_round_trip() {
        let enc = sample();
        let elem = build_encrypted_element(&enc).unwrap();
        let parsed = parse_encrypted_element(&elem).unwrap();
        assert_eq!(parsed, enc);
    }

    #[test]
    fn test_parse_requires_sources() {
        let elem = Element::builder("encrypted", NS_ESFS)
            .attr(
                minidom::rxml::xml_ncname!("cipher").to_owned(),
                Cipher::Aes256GcmNoPadding.as_uri(),
            )
            .append(Element::builder("key", NS_ESFS).append("k").build())
            .append(Element::builder("iv", NS_ESFS).append("v").build())
            .append(Element::builder("sources", NS_SFS).build())
            .build();
        assert_eq!(
            parse_encrypted_element(&elem),
            Err(EncryptedFileError::NoSources)
        );
    }

    #[test]
    fn test_parse_rejects_unknown_cipher() {
        let elem = Element::builder("encrypted", NS_ESFS)
            .attr(
                minidom::rxml::xml_ncname!("cipher").to_owned(),
                "urn:xmpp:ciphers:rot13:0",
            )
            .build();
        assert!(matches!(
            parse_encrypted_element(&elem),
            Err(EncryptedFileError::UnknownCipher(_))
        ));
    }

    #[test]
    fn test_is_encrypted_file_element() {
        assert!(is_encrypted_file_element(
            &Element::builder("encrypted", NS_ESFS).build()
        ));
        assert!(!is_encrypted_file_element(
            &Element::builder("encrypted", "other").build()
        ));
    }

    #[test]
    fn test_set_and_extract_on_message() {
        use jid::BareJid;
        let mut msg = message_with_file_sharing(jid::Jid::from(
            "bob@example.com".parse::<BareJid>().unwrap(),
        ));
        let enc = sample();
        set_encrypted_file(&mut msg, &enc).unwrap();
        let extracted = extract_encrypted_file(&msg).unwrap().unwrap();
        assert_eq!(extracted, enc);
    }

    #[test]
    fn test_build_requires_sources() {
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

    #[test]
    fn test_cipher_round_trip() {
        for c in [Cipher::Aes128GcmNoPadding, Cipher::Aes256GcmNoPadding] {
            assert_eq!(Cipher::from_uri(c.as_uri()), Some(c));
        }
    }
}
