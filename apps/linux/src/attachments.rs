//! File attachments as the web client sends them: the file is encrypted
//! (XEP-0448, AES-GCM with a fresh key per file), uploaded to an XEP-0363
//! slot, and shared in the message (XEP-0447) with its key, IV and hashes.
//! Receivers fetch the ciphertext, check its SHA-256, and decrypt.
//!
//! WebCrypto's AES-GCM output is the ciphertext followed by the 128-bit
//! tag, which is also what `ring` produces, so files round-trip between
//! clients unchanged.

use std::net::IpAddr;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use thiserror::Error;
use url::{Host, Url};
use waddle_client_session::Attachment;

/// The web client's cap; the upload service may set a lower one.
pub const MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;
pub const AES_256_GCM: &str = "urn:xmpp:ciphers:aes-256-gcm-nopadding:0";
pub const AES_128_GCM: &str = "urn:xmpp:ciphers:aes-128-gcm-nopadding:0";
pub const SHA_256: &str = "sha-256";
const IV_BYTES: usize = 12;
/// Largest file fetched for display or saving: the web client's upload cap
/// leaves headroom for other clients' larger uploads.
pub const MAX_DOWNLOAD_BYTES: usize = 50 * 1024 * 1024;
/// XEP-0363 §5: the only headers a client may pass on from a slot.
const SLOT_HEADERS: [&str; 3] = ["authorization", "cookie", "expires"];
const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttachmentError {
    #[error("the file is larger than 10 MB")]
    TooLarge,
    #[error("the file is empty")]
    Empty,
    #[error("the attachment uses an unsupported cipher")]
    UnsupportedCipher,
    #[error("the attachment's key or IV is malformed")]
    MalformedKey,
    #[error("the attachment failed its integrity check")]
    Integrity,
    #[error("the attachment could not be decrypted")]
    Decrypt,
    #[error("no secure randomness is available")]
    Random,
}

/// An encrypted file ready to upload, with what the message must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub key_b64: String,
    pub iv_b64: String,
    /// SHA-256 of the ciphertext (XEP-0448 `<hash/>` under `<encrypted/>`).
    pub ciphertext_sha256_b64: String,
    /// SHA-256 of the plaintext (XEP-0446 `<hash/>` under `<file/>`).
    pub plaintext_sha256_b64: String,
}

/// Encrypt `plaintext` with a fresh AES-256-GCM key and IV.
pub fn seal(plaintext: &[u8]) -> Result<Sealed, AttachmentError> {
    if plaintext.is_empty() {
        return Err(AttachmentError::Empty);
    }
    if u64::try_from(plaintext.len()).map_or(true, |length| length > MAX_UPLOAD_BYTES) {
        return Err(AttachmentError::TooLarge);
    }
    let random = SystemRandom::new();
    let mut key = [0u8; 32];
    let mut iv = [0u8; IV_BYTES];
    random.fill(&mut key).map_err(|_| AttachmentError::Random)?;
    random.fill(&mut iv).map_err(|_| AttachmentError::Random)?;
    let sealing = LessSafeKey::new(
        UnboundKey::new(&aead::AES_256_GCM, &key).map_err(|_| AttachmentError::MalformedKey)?,
    );
    let mut ciphertext = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(iv),
            Aad::empty(),
            &mut ciphertext,
        )
        .map_err(|_| AttachmentError::Decrypt)?;
    Ok(Sealed {
        ciphertext_sha256_b64: sha256_b64(&ciphertext),
        plaintext_sha256_b64: sha256_b64(plaintext),
        key_b64: BASE64.encode(key),
        iv_b64: BASE64.encode(iv),
        ciphertext,
    })
}

/// Check and decrypt a fetched ciphertext. `hashes` are the message's
/// SHA-256 ciphertext hashes (base64); when any are given, one must match.
pub fn open(
    ciphertext: &[u8],
    cipher: &str,
    key_b64: &str,
    iv_b64: &str,
    hashes: &[String],
) -> Result<Vec<u8>, AttachmentError> {
    let algorithm = match cipher {
        AES_256_GCM => &aead::AES_256_GCM,
        AES_128_GCM => &aead::AES_128_GCM,
        _ => return Err(AttachmentError::UnsupportedCipher),
    };
    if !hashes.is_empty() {
        let actual = sha256_b64(ciphertext);
        if !hashes.contains(&actual) {
            return Err(AttachmentError::Integrity);
        }
    }
    let key = BASE64
        .decode(key_b64)
        .map_err(|_| AttachmentError::MalformedKey)?;
    let iv: [u8; IV_BYTES] = BASE64
        .decode(iv_b64)
        .map_err(|_| AttachmentError::MalformedKey)?
        .try_into()
        .map_err(|_| AttachmentError::MalformedKey)?;
    if key.len() != algorithm.key_len() {
        return Err(AttachmentError::MalformedKey);
    }
    let opening = LessSafeKey::new(
        UnboundKey::new(algorithm, &key).map_err(|_| AttachmentError::MalformedKey)?,
    );
    let mut buffer = ciphertext.to_vec();
    let plaintext = opening
        .open_in_place(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut buffer)
        .map_err(|_| AttachmentError::Decrypt)?;
    Ok(plaintext.to_vec())
}

/// XEP-0446 disposition the web client gives a file: shown in the
/// message for images, video, audio and PDFs, otherwise a download.
pub fn disposition(media_type: &str) -> &'static str {
    let inline = ["image/", "video/", "audio/"]
        .iter()
        .any(|prefix| media_type.starts_with(prefix))
        || media_type == "application/pdf";
    if inline {
        "inline"
    } else {
        "attachment"
    }
}

/// The name the ciphertext is uploaded under, as the web client names it.
pub fn upload_name(name: &str) -> String {
    if name.ends_with(".enc") {
        name.to_string()
    } else {
        format!("{name}.enc")
    }
}

fn sha256_b64(bytes: &[u8]) -> String {
    BASE64.encode(digest(&SHA256, bytes).as_ref())
}

#[derive(Debug, Error)]
pub enum TransferError {
    #[error(transparent)]
    Attachment(#[from] AttachmentError),
    #[error("the server offers no file uploads")]
    NoUploadService,
    #[error("the server refused the upload")]
    SlotRefused,
    #[error("the file address is not secure")]
    InsecureUrl,
    #[error("the file is too large to fetch")]
    TooLarge,
    #[error("the file server answered {0}")]
    Status(reqwest::StatusCode),
    #[error("the file could not be transferred: {0}")]
    Network(#[from] reqwest::Error),
    #[error("the transfer stopped unexpectedly")]
    Interrupted,
}

/// Whether a URL's host is this machine.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain == "localhost",
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    }
}

/// An address on this machine or its network, written as an IP literal.
fn is_local_ip(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => {
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
        }
        Some(Host::Ipv6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
        Some(Host::Domain(domain)) => domain == "localhost",
        None => false,
    }
}

/// An upload or download address: HTTPS (XEP-0363 requires it), or HTTP
/// on loopback when the account's own server is on loopback (a local
/// server). Remote senders choose these URLs, so unless the account's
/// server is local, addresses written as local or private IPs are refused
/// over HTTPS too. A host name that resolves to one is not caught here;
/// TLS certificate checks stand in the way of most such services.
pub fn transfer_url(raw: &str, allow_loopback: bool) -> Result<Url, TransferError> {
    let url = Url::parse(raw).map_err(|_| TransferError::InsecureUrl)?;
    if !allow_loopback && is_local_ip(&url) {
        return Err(TransferError::InsecureUrl);
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if allow_loopback && is_loopback(&url) => Ok(url),
        _ => Err(TransferError::InsecureUrl),
    }
}

/// The slot's headers a client may send (XEP-0363 §5), others dropped.
pub fn slot_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, value)| {
            SLOT_HEADERS.contains(&name.to_ascii_lowercase().as_str())
                && !value.contains(['\r', '\n'])
        })
        .cloned()
        .collect()
}

/// HTTP for file sharing. Every redirect hop is held to the same rule as
/// the first request, and a stalled server cannot hang a transfer.
#[derive(Clone)]
pub struct Transfers {
    http: reqwest::Client,
    allow_loopback: bool,
}

impl Transfers {
    /// `server` is the account's own server; loopback HTTP is allowed only
    /// when it is itself on loopback.
    pub fn new(server: &Url) -> Result<Self, reqwest::Error> {
        let allow_loopback = is_loopback(server);
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error(TransferError::InsecureUrl)
            } else if transfer_url(attempt.url().as_str(), allow_loopback).is_ok() {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let http = reqwest::Client::builder()
            .user_agent(concat!("Waddle-Linux/", env!("CARGO_PKG_VERSION")))
            .redirect(policy)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .timeout(TRANSFER_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            allow_loopback,
        })
    }

    pub fn check(&self, raw: &str) -> Result<Url, TransferError> {
        transfer_url(raw, self.allow_loopback)
    }

    /// PUT the ciphertext to an XEP-0363 slot.
    pub async fn put(
        &self,
        put_url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<(), TransferError> {
        let mut request = self
            .http
            .put(self.check(put_url)?)
            // A large file on a slow link; the read timeout still catches a
            // stall.
            .timeout(UPLOAD_TIMEOUT)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream");
        for (name, value) in slot_headers(headers) {
            request = request.header(name, value);
        }
        let response = request.body(body).send().await?;
        if !response.status().is_success() {
            return Err(TransferError::Status(response.status()));
        }
        Ok(())
    }

    /// GET a file, refusing anything larger than [`MAX_DOWNLOAD_BYTES`].
    pub async fn fetch(&self, url: &str) -> Result<Vec<u8>, TransferError> {
        let mut response = self.http.get(self.check(url)?).send().await?;
        if !response.status().is_success() {
            return Err(TransferError::Status(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_DOWNLOAD_BYTES as u64)
        {
            return Err(TransferError::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > MAX_DOWNLOAD_BYTES {
                return Err(TransferError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

impl TransferError {
    /// Worth trying again later: the network or the server, not the file.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Status(_) | Self::Interrupted)
    }
}

/// What identifies a shared file's content: its address and everything
/// needed to decrypt it. Two messages naming one URL with different keys
/// are different files.
pub fn cache_key(attachment: &Attachment) -> String {
    let mut parts: Vec<&str> = vec![attachment.url.as_str()];
    if let Some(encryption) = &attachment.encryption {
        parts.extend([
            encryption.cipher.as_str(),
            encryption.key_b64.as_str(),
            encryption.iv_b64.as_str(),
        ]);
        parts.extend(encryption.sha256_b64.iter().map(String::as_str));
        parts.extend(encryption.sources.iter().map(String::as_str));
    }
    sha256_b64(parts.join("\u{0}").as_bytes())
}

/// A sender-chosen file name made safe to propose for saving: its last
/// path component, without control characters.
pub fn safe_file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    // Control characters, and the invisible ones that reorder or hide text
    // (U+202E makes "invoice\u{202E}fdp.exe" read as "invoiceexe.pdf").
    let invisible = |c: &char| {
        matches!(
            *c,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
    };
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !invisible(c))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.');
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Human file size: "512 B", "1.4 MB".
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_files_open_with_their_key_and_hash() {
        let sealed = seal(b"a picture of a penguin").expect("sealed");
        assert_eq!(sealed.ciphertext.len(), 22 + 16, "ciphertext then tag");
        assert_eq!(BASE64.decode(&sealed.key_b64).map(|key| key.len()), Ok(32));
        assert_eq!(BASE64.decode(&sealed.iv_b64).map(|iv| iv.len()), Ok(12));
        let opened = open(
            &sealed.ciphertext,
            AES_256_GCM,
            &sealed.key_b64,
            &sealed.iv_b64,
            std::slice::from_ref(&sealed.ciphertext_sha256_b64),
        );
        assert_eq!(opened.as_deref(), Ok(&b"a picture of a penguin"[..]));
    }

    /// Produced by WebCrypto (`crypto.subtle.encrypt` with AES-GCM), the
    /// engine the web client encrypts with, from a fixed key and IV.
    #[test]
    fn files_encrypted_by_the_web_client_open() {
        let ciphertext = BASE64
            .decode("kXkYSSmuItYMEeKhaArguBnULWXg0vOJ1aC8JRxMI2JMjeeVmvM=")
            .expect("fixture");
        let opened = open(
            &ciphertext,
            AES_256_GCM,
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
            "oKGio6Slpqeoqaqr",
            &["on40k9m4R7S7aNsJ5w8eYSy6hg5D22HV7tNCVzvzp84=".to_string()],
        );
        assert_eq!(opened.as_deref(), Ok(&b"waddle interop fixture"[..]));
    }

    #[test]
    fn every_file_gets_a_fresh_key_and_iv() {
        let (first, second) = (
            seal(b"same").expect("sealed"),
            seal(b"same").expect("sealed"),
        );
        assert_ne!(first.key_b64, second.key_b64);
        assert_ne!(first.iv_b64, second.iv_b64);
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_eq!(first.plaintext_sha256_b64, second.plaintext_sha256_b64);
    }

    #[test]
    fn tampering_is_caught_by_the_hash_or_the_tag() {
        let sealed = seal(b"payload").expect("sealed");
        let mut tampered = sealed.ciphertext.clone();
        tampered[0] ^= 1;
        let hashes = [sealed.ciphertext_sha256_b64.clone()];
        assert_eq!(
            open(
                &tampered,
                AES_256_GCM,
                &sealed.key_b64,
                &sealed.iv_b64,
                &hashes
            ),
            Err(AttachmentError::Integrity)
        );
        // Without a hash to check, the GCM tag still refuses it.
        assert_eq!(
            open(&tampered, AES_256_GCM, &sealed.key_b64, &sealed.iv_b64, &[]),
            Err(AttachmentError::Decrypt)
        );
    }

    #[test]
    fn key_cipher_and_iv_mismatches_are_refused() {
        let sealed = seal(b"payload").expect("sealed");
        assert_eq!(
            open(
                &sealed.ciphertext,
                AES_128_GCM,
                &sealed.key_b64,
                &sealed.iv_b64,
                &[]
            ),
            Err(AttachmentError::MalformedKey)
        );
        assert_eq!(
            open(
                &sealed.ciphertext,
                "urn:example:rot13",
                &sealed.key_b64,
                &sealed.iv_b64,
                &[]
            ),
            Err(AttachmentError::UnsupportedCipher)
        );
        assert_eq!(
            open(
                &sealed.ciphertext,
                AES_256_GCM,
                &sealed.key_b64,
                "AAAA",
                &[]
            ),
            Err(AttachmentError::MalformedKey)
        );
    }

    #[test]
    fn empty_and_oversized_files_are_refused() {
        assert_eq!(seal(b""), Err(AttachmentError::Empty));
        let large = vec![0u8; usize::try_from(MAX_UPLOAD_BYTES).expect("fits") + 1];
        assert_eq!(seal(&large), Err(AttachmentError::TooLarge));
    }

    #[test]
    fn dispositions_and_upload_names_match_the_web_client() {
        assert_eq!(disposition("image/png"), "inline");
        assert_eq!(disposition("application/pdf"), "inline");
        assert_eq!(disposition("application/zip"), "attachment");
        assert_eq!(upload_name("cat.png"), "cat.png.enc");
        assert_eq!(upload_name("cat.png.enc"), "cat.png.enc");
    }

    #[test]
    fn transfers_need_https_and_loopback_only_for_a_local_server() {
        assert!(transfer_url("https://up.waddle.social/f", false).is_ok());
        for local in [
            "http://127.0.0.1:5280/f",
            "http://localhost/f",
            "http://[::1]/f",
        ] {
            assert!(transfer_url(local, true).is_ok(), "{local}");
            assert!(
                matches!(transfer_url(local, false), Err(TransferError::InsecureUrl)),
                "a remote sender's {local} must not reach this machine"
            );
        }
        for local in [
            "https://127.0.0.1/f",
            "https://192.168.1.1/f",
            "https://10.0.0.1/f",
            "https://169.254.169.254/latest",
            "https://[fd00::1]/f",
            "https://localhost/f",
        ] {
            assert!(transfer_url(local, true).is_ok(), "{local}");
            assert!(
                matches!(transfer_url(local, false), Err(TransferError::InsecureUrl)),
                "{local} is local to the user, not the sender"
            );
        }
        for insecure in [
            "http://up.waddle.social/f",
            "file:///etc/passwd",
            "ftp://x/y",
        ] {
            assert!(matches!(
                transfer_url(insecure, true),
                Err(TransferError::InsecureUrl)
            ));
        }
    }

    #[test]
    fn the_same_url_with_another_key_is_another_file() {
        let attachment = |key: &str| Attachment {
            url: "https://up.example/a".into(),
            name: None,
            media_type: None,
            size: None,
            inline: true,
            encryption: Some(waddle_client_session::Encryption {
                cipher: AES_256_GCM.into(),
                key_b64: key.into(),
                iv_b64: "iv".into(),
                sha256_b64: vec!["h".into()],
                sources: vec!["https://up.example/a".into()],
            }),
        };
        assert_eq!(cache_key(&attachment("k1")), cache_key(&attachment("k1")));
        assert_ne!(cache_key(&attachment("k1")), cache_key(&attachment("k2")));
    }

    #[test]
    fn sender_file_names_cannot_choose_a_folder() {
        assert_eq!(
            safe_file_name("../../.config/autostart/cat.png.desktop"),
            "cat.png.desktop"
        );
        assert_eq!(safe_file_name("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name("a\u{0}b\nc.txt"), "abc.txt");
        assert_eq!(safe_file_name("../"), "file");
        assert_eq!(safe_file_name("photo.jpg"), "photo.jpg");
        assert_eq!(safe_file_name("invoice\u{202E}fdp.exe"), "invoicefdp.exe");
    }

    #[tokio::test]
    async fn redirects_are_held_to_the_same_rule() {
        use std::io::{Read, Write};
        // A server that redirects to plain http on another host.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://192.0.2.1/x\r\nContent-Length: 0\r\n\r\n",
                );
            }
        });
        crate::install_crypto_provider();
        let server = Url::parse(&format!("http://{address}/")).expect("url");
        let transfers = Transfers::new(&server).expect("client");
        let result = transfers.fetch(&format!("http://{address}/file")).await;
        assert!(
            matches!(result, Err(TransferError::Status(status)) if status.as_u16() == 302),
            "the insecure hop was followed: {result:?}"
        );
    }

    #[test]
    fn only_the_headers_xep_0363_allows_are_sent() {
        let headers = slot_headers(&[
            ("Authorization".into(), "Basic abc".into()),
            ("Cookie".into(), "a=b".into()),
            ("Expires".into(), "Tue".into()),
            ("Host".into(), "evil.example".into()),
            ("Cookie".into(), "a=b\r\nX-Injected: 1".into()),
        ]);
        let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Authorization", "Cookie", "Expires"]);
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1_400_000), "1.4 MB");
        assert_eq!(format_size(2_000), "2.0 KB");
    }
}
