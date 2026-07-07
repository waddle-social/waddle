//! XEP-0448: Encryption for Stateless File Sharing.
//!
//! Layers end-to-end encryption metadata over XEP-0447 file sharing. The
//! ciphertext lives at the HTTP source URL (typically a XEP-0363 slot), and
//! the sender publishes the symmetric `cipher`/`key`/`iv`/`hashes` needed to
//! decrypt it once downloaded.
//!
//! Wire format:
//!
//! ```xml
//! <encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>
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
//! Waddle's chat sends `<encrypted/>` inside the XEP-0447
//! `<file-sharing><sources/>` list instead of exposing a bare URL source for
//! encrypted transfers. The matching `<file-sharing/>` element carries the
//! plaintext metadata such as filename, size, media-type and dimensions.

use minidom::Element;

/// XEP-0448 envelope namespace.
pub const NS_ESFS: &str = "urn:xmpp:esfs:0";
/// XEP-0300 hash element namespace.
pub const NS_HASHES: &str = "urn:xmpp:hashes:2";
/// XEP-0447 stateless file sharing namespace (re-used here for `<sources/>`).
pub const NS_SFS: &str = "urn:xmpp:sfs:0";
/// XEP-0103 URL data namespace (re-used here for `<url-data/>` source URLs).
pub const NS_URL_DATA: &str = "http://jabber.org/protocol/url-data";

/// Symmetric ciphers Waddle understands end-to-end. The XEP leaves the cipher
/// set open; we keep this closed enum to refuse unknown values at the type
/// boundary instead of dragging strings through the rest of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cipher {
    /// `urn:xmpp:ciphers:aes-128-gcm-nopadding:0`
    Aes128GcmNoPadding,
    /// `urn:xmpp:ciphers:aes-256-gcm-nopadding:0`
    Aes256GcmNoPadding,
}

impl Cipher {
    pub fn as_uri(self) -> &'static str {
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

/// A `<hash xmlns='urn:xmpp:hashes:2' algo='…'>BASE64</hash>` entry nested
/// inside the encrypted envelope. Algorithms are kept as `String` to allow
/// servers and peers to advertise hashes Waddle does not itself produce.
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

/// Build the `<encrypted/>` payload element from a typed value. Returns
/// `None` if no source URLs were provided — XEP-0448 mandates at least one.
pub fn build_encrypted_element(enc: &EncryptedFile) -> Option<Element> {
    if enc.sources.is_empty() {
        return None;
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
    Some(builder.build())
}

/// Parse an `<encrypted/>` element. Returns `None` if the element is in the
/// wrong namespace, advertises a cipher Waddle does not implement, or is
/// missing any of the required `<key>`, `<iv>`, or `<sources>` children.
pub fn parse_encrypted_element(elem: &Element) -> Option<EncryptedFile> {
    if !is_encrypted_element(elem) {
        return None;
    }
    let cipher = Cipher::from_uri(elem.attr("cipher")?)?;
    // XML pretty-printing can introduce surrounding whitespace inside
    // `<key/>`, `<iv/>`, and `<hash/>` elements. Strict base64 decoders
    // (including the browser's `atob`) reject that whitespace, so trim
    // before handing the value off.
    let key_b64 = elem.get_child("key", NS_ESFS)?.text().trim().to_string();
    let iv_b64 = elem.get_child("iv", NS_ESFS)?.text().trim().to_string();

    let mut hashes = Vec::new();
    for child in elem.children() {
        if child.is("hash", NS_HASHES) {
            let Some(algo) = child.attr("algo") else {
                continue;
            };
            hashes.push(EncryptedHash {
                algo: algo.to_string(),
                value_b64: child.text().trim().to_string(),
            });
        }
    }

    let sources_elem = elem.get_child("sources", NS_SFS)?;
    let mut sources = Vec::new();
    for child in sources_elem.children() {
        if child.is("url-data", NS_URL_DATA) {
            if let Some(target) = child.attr("target") {
                sources.push(target.to_string());
            }
        }
    }
    if sources.is_empty() {
        return None;
    }

    Some(EncryptedFile {
        cipher,
        key_b64,
        iv_b64,
        hashes,
        sources,
    })
}

/// Returns `true` for `<encrypted xmlns='urn:xmpp:esfs:0'/>`.
pub fn is_encrypted_element(elem: &Element) -> bool {
    elem.is("encrypted", NS_ESFS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EncryptedFile {
        EncryptedFile {
            cipher: Cipher::Aes256GcmNoPadding,
            key_b64: "a2V5".to_string(),
            iv_b64: "aXY=".to_string(),
            hashes: vec![EncryptedHash {
                algo: "sha-256".to_string(),
                value_b64: "aGFzaA==".to_string(),
            }],
            sources: vec!["https://files.example.com/blob.enc".to_string()],
        }
    }

    #[test]
    fn round_trips_through_minidom() {
        let enc = sample();
        let elem = build_encrypted_element(&enc).expect("at least one source");
        let parsed = parse_encrypted_element(&elem).expect("round-trip parses");
        assert_eq!(parsed, enc);
    }

    #[test]
    fn build_returns_none_when_no_sources() {
        let enc = EncryptedFile {
            cipher: Cipher::Aes256GcmNoPadding,
            key_b64: "a2V5".to_string(),
            iv_b64: "aXY=".to_string(),
            hashes: Vec::new(),
            sources: Vec::new(),
        };
        assert!(build_encrypted_element(&enc).is_none());
    }

    #[test]
    fn parse_rejects_unknown_cipher() {
        let xml = "<encrypted xmlns='urn:xmpp:esfs:0' \
                       cipher='urn:xmpp:ciphers:rot13:0'>\
                     <key>k</key><iv>v</iv>\
                     <sources xmlns='urn:xmpp:sfs:0'>\
                       <url-data xmlns='http://jabber.org/protocol/url-data' target='https://x'/>\
                     </sources>\
                   </encrypted>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(parse_encrypted_element(&elem).is_none());
    }

    #[test]
    fn parse_rejects_missing_sources() {
        let xml = "<encrypted xmlns='urn:xmpp:esfs:0' \
                       cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
                     <key>k</key><iv>v</iv>\
                     <sources xmlns='urn:xmpp:sfs:0'/>\
                   </encrypted>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(parse_encrypted_element(&elem).is_none());
    }

    #[test]
    fn parse_rejects_wrong_namespace() {
        let xml = "<encrypted xmlns='urn:waddle:other'/>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(parse_encrypted_element(&elem).is_none());
        assert!(!is_encrypted_element(&elem));
    }

    #[test]
    fn cipher_round_trips() {
        for c in [Cipher::Aes128GcmNoPadding, Cipher::Aes256GcmNoPadding] {
            assert_eq!(Cipher::from_uri(c.as_uri()), Some(c));
        }
        assert!(Cipher::from_uri("urn:xmpp:ciphers:rot13:0").is_none());
    }

    #[test]
    fn parse_trims_whitespace_around_base64_text() {
        // Pretty-printed XML often wraps the base64 payload with newlines
        // and indentation; the resulting text content must round-trip
        // cleanly to a strict base64 decoder.
        let xml = "<encrypted xmlns='urn:xmpp:esfs:0' \
                       cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
                     <key>\n  a2V5\n</key>\
                     <iv>\n  aXY=\n</iv>\
                     <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>\n  aGFzaA==\n</hash>\
                     <sources xmlns='urn:xmpp:sfs:0'>\
                       <url-data xmlns='http://jabber.org/protocol/url-data' target='https://x'/>\
                     </sources>\
                   </encrypted>";
        let elem: Element = xml.parse().expect("valid xml");
        let parsed = parse_encrypted_element(&elem).expect("parses");
        assert_eq!(parsed.key_b64, "a2V5");
        assert_eq!(parsed.iv_b64, "aXY=");
        assert_eq!(parsed.hashes[0].value_b64, "aGFzaA==");
    }

    #[test]
    fn parses_multiple_hashes() {
        let xml = "<encrypted xmlns='urn:xmpp:esfs:0' \
                       cipher='urn:xmpp:ciphers:aes-128-gcm-nopadding:0'>\
                     <key>k</key><iv>v</iv>\
                     <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>aaa</hash>\
                     <hash xmlns='urn:xmpp:hashes:2' algo='sha3-256'>bbb</hash>\
                     <sources xmlns='urn:xmpp:sfs:0'>\
                       <url-data xmlns='http://jabber.org/protocol/url-data' target='https://x'/>\
                     </sources>\
                   </encrypted>";
        let elem: Element = xml.parse().expect("valid xml");
        let parsed = parse_encrypted_element(&elem).expect("parses");
        assert_eq!(parsed.hashes.len(), 2);
        assert_eq!(parsed.hashes[0].algo, "sha-256");
        assert_eq!(parsed.hashes[1].algo, "sha3-256");
    }
}
