//! Extension bot avatars, served in-band.
//!
//! A manifest names its bot's avatar as an immutable artifact: a URL plus
//! the SHA-256 of its bytes. Clients never fetch that URL, because doing so
//! would disclose each viewer's IP address to the artifact host. The server
//! fetches it instead (HTTPS only, non-global addresses refused, no
//! redirects, size-capped), checks the digest, normalises the image to PNG
//! (XEP-0084 §3.2) and keeps it in memory. Bots then answer XEP-0084,
//! vcard-temp and vCard4 with the bytes.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use minidom::Element;
use tokio::time::Instant;
use tracing::{debug, warn};
use waddle_extensions::{ArtifactReference, ExtensionManager, Sha256Digest};
use waddle_xmpp::xep::xep0054::VCardPhoto;
use waddle_xmpp::xep::xep0084::{self, AvatarInfo};

use crate::profile::{fetch_artifact_avatar_bytes, AvatarBytes, FetchError, FetchPolicy};

/// Cap on the fetched artifact and on the PNG served for it.
const MAX_BYTES: usize = 256 * 1024;
/// Wait before the first retry of a failed fetch. Doubles per failure up to
/// [`MAX_RETRY_DELAY`].
const RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

/// A bot avatar as served: PNG bytes and their XEP-0084 metadata.
#[derive(Debug)]
pub(crate) struct BotAvatar {
    info: AvatarInfo,
    base64: String,
}

impl BotAvatar {
    fn new(image: AvatarBytes) -> Result<Self, BotAvatarError> {
        // Reading the header also rejects a body that merely starts with a
        // PNG signature.
        let (width, height) = image::ImageReader::new(Cursor::new(&image.bytes))
            .with_guessed_format()
            .map_err(image::ImageError::IoError)?
            .into_dimensions()?;
        // XEP-0084 §4.2.1: width and height are unsignedShort, "if available".
        let side = |pixels: u32| (pixels <= u32::from(u16::MAX)).then_some(pixels);
        Ok(Self {
            info: AvatarInfo {
                id: xep0084::compute_avatar_hash(&image.bytes),
                mime_type: image.mime,
                width: side(width),
                height: side(height),
                bytes: Some(image.bytes.len() as u64),
                url: None,
            },
            base64: BASE64.encode(&image.bytes),
        })
    }

    /// The XEP-0084 item id: the SHA-1 hex of the PNG bytes.
    pub(crate) fn id(&self) -> &str {
        &self.info.id
    }

    /// XEP-0084 §4.2: one in-band `<info/>`, no URL.
    pub(crate) fn metadata(&self) -> Element {
        xep0084::build_avatar_metadata(&self.info)
    }

    /// XEP-0084 §4.1: the base64 PNG.
    pub(crate) fn data(&self) -> Element {
        xep0084::build_avatar_data(&self.base64)
    }

    /// XEP-0054 PHOTO with TYPE and BINVAL.
    pub(crate) fn vcard_photo(&self) -> VCardPhoto {
        VCardPhoto::Binary {
            mime_type: self.info.mime_type.clone(),
            data: self.base64.clone(),
        }
    }

    /// XEP-0292 photo: the PHOTO mapped to an RFC 2397 `data:` URI.
    pub(crate) fn vcard4_photo_uri(&self) -> String {
        format!("data:{};base64,{}", self.info.mime_type, self.base64)
    }
}

#[derive(Debug, thiserror::Error)]
enum BotAvatarError {
    #[error("invalid artifact URL: {0}")]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("unreadable image: {0}")]
    Image(#[from] image::ImageError),
}

enum Slot {
    Fetching { failures: u32 },
    Ready(Arc<BotAvatar>),
    Failed { failures: u32, retry_at: Instant },
}

/// Bot avatars by artifact digest. Content-addressed, so a manifest that
/// changes its avatar names a new entry.
pub(crate) struct BotAvatars {
    policy: FetchPolicy,
    slots: DashMap<Sha256Digest, Slot>,
}

impl Default for BotAvatars {
    fn default() -> Self {
        Self::new(FetchPolicy {
            max_bytes: MAX_BYTES,
            ..FetchPolicy::default()
        })
    }
}

impl BotAvatars {
    fn new(policy: FetchPolicy) -> Self {
        Self {
            policy,
            slots: DashMap::new(),
        }
    }

    /// Start fetching every installed bot's avatar.
    pub(crate) fn prefetch(self: &Arc<Self>, manager: &ExtensionManager) {
        for (manifest, _) in manager.configured_plugins() {
            if let Some(avatar) = manifest.profile.and_then(|profile| profile.avatar) {
                self.get(&avatar);
            }
        }
    }

    /// The avatar `reference` names, once fetched. Never waits on the
    /// network, so answers stay off the serial frame loop's critical path:
    /// with nothing cached and no retry pending, a fetch starts in the
    /// background and this answers `None`.
    pub(crate) fn get(self: &Arc<Self>, reference: &ArtifactReference) -> Option<Arc<BotAvatar>> {
        match self.slots.entry(reference.sha256.clone()) {
            Entry::Occupied(mut slot) => match *slot.get() {
                Slot::Ready(ref avatar) => return Some(Arc::clone(avatar)),
                Slot::Failed { failures, retry_at } if retry_at <= Instant::now() => {
                    slot.insert(Slot::Fetching { failures });
                }
                Slot::Fetching { .. } | Slot::Failed { .. } => return None,
            },
            Entry::Vacant(slot) => {
                slot.insert(Slot::Fetching { failures: 0 });
            }
        }
        let avatars = Arc::clone(self);
        let reference = reference.clone();
        tokio::spawn(async move { avatars.fetch(&reference).await });
        None
    }

    /// Fetch, verify and cache `reference`. A failure is logged with its
    /// reason (as a warning the first time) and retried on a later
    /// [`Self::get`] after a capped exponential backoff.
    pub(crate) async fn fetch(&self, reference: &ArtifactReference) -> Option<Arc<BotAvatar>> {
        let result = fetch_bot_avatar(reference, &self.policy).await;
        let failures = match self.slots.get(&reference.sha256).as_deref() {
            Some(Slot::Fetching { failures } | Slot::Failed { failures, .. }) => *failures,
            _ => 0,
        };
        match result {
            Ok(avatar) => {
                let avatar = Arc::new(avatar);
                self.slots
                    .insert(reference.sha256.clone(), Slot::Ready(Arc::clone(&avatar)));
                Some(avatar)
            }
            Err(error) => {
                let failures = failures.saturating_add(1);
                let retry_in = retry_delay(failures);
                if failures == 1 {
                    warn!(artifact = %reference.uri, %error, ?retry_in, "Extension bot avatar unavailable");
                } else {
                    debug!(artifact = %reference.uri, %error, failures, ?retry_in, "Extension bot avatar still unavailable");
                }
                self.slots.insert(
                    reference.sha256.clone(),
                    Slot::Failed {
                        failures,
                        retry_at: Instant::now() + retry_in,
                    },
                );
                None
            }
        }
    }
}

fn retry_delay(failures: u32) -> Duration {
    RETRY_DELAY
        .saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(MAX_RETRY_DELAY)
}

async fn fetch_bot_avatar(
    reference: &ArtifactReference,
    policy: &FetchPolicy,
) -> Result<BotAvatar, BotAvatarError> {
    let url = reference.uri.as_str().parse()?;
    let image = fetch_artifact_avatar_bytes(&url, &reference.sha256, policy).await?;
    BotAvatar::new(image)
}

#[cfg(test)]
impl BotAvatars {
    /// Fetches from a plain-HTTP test server on loopback.
    pub(crate) fn loopback() -> Self {
        Self::new(FetchPolicy {
            max_bytes: MAX_BYTES,
            block_non_global_ips: false,
            allow_http_for_tests: true,
            ..FetchPolicy::default()
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) fn encode_image(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let pixels = image::ImageBuffer::from_pixel(width, height, image::Rgb([0u8, 128, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(pixels)
            .write_to(&mut Cursor::new(&mut out), format)
            .expect("encode test image");
        out
    }

    /// Serve `body` as `mime` at an artifact URL pinned to the digest of
    /// `pinned`.
    pub(crate) async fn serve_artifact(
        server: &MockServer,
        pinned: &[u8],
        body: Vec<u8>,
        mime: &str,
    ) -> ArtifactReference {
        let digest = hex::encode(Sha256::digest(pinned));
        let artifact_path = format!("/sha256/{digest}");
        Mock::given(method("GET"))
            .and(path(artifact_path.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, mime))
            .mount(server)
            .await;
        ArtifactReference::new(format!("{}{artifact_path}", server.uri()), digest, None)
            .expect("artifact reference")
    }

    #[tokio::test]
    async fn serves_the_verified_avatar_as_png_with_its_sha1_id() {
        let server = MockServer::start().await;
        let png = encode_image(3, 2, image::ImageFormat::Png);
        let reference = serve_artifact(&server, &png, png.clone(), "image/png").await;

        let avatar = fetch_bot_avatar(&reference, &BotAvatars::loopback().policy)
            .await
            .expect("verified avatar");
        assert_eq!(avatar.id(), xep0084::compute_avatar_hash(&png));
        let metadata = xep0084::parse_avatar_metadata(&avatar.metadata()).expect("metadata");
        assert_eq!(metadata.id, avatar.id());
        assert_eq!(metadata.mime_type, "image/png");
        assert_eq!(
            (metadata.width, metadata.height, metadata.bytes),
            (Some(3), Some(2), Some(png.len() as u64))
        );
        assert_eq!(metadata.url, None, "served in-band only");
        let data = xep0084::parse_avatar_data(&avatar.data()).expect("data");
        assert_eq!(BASE64.decode(data).expect("base64"), png);
        assert!(matches!(
            avatar.vcard_photo(),
            VCardPhoto::Binary { ref mime_type, ref data }
                if mime_type == "image/png" && BASE64.decode(data).ok() == Some(png.clone())
        ));
        assert_eq!(
            avatar.vcard4_photo_uri(),
            format!("data:image/png;base64,{}", BASE64.encode(&png))
        );
    }

    /// XEP-0084 §3.2/§4.1: the data node carries PNG and the id is the
    /// SHA-1 of those PNG bytes, so another format is transcoded first.
    #[tokio::test]
    async fn serves_a_jpeg_artifact_as_png() {
        let server = MockServer::start().await;
        let jpeg = encode_image(4, 4, image::ImageFormat::Jpeg);
        let reference = serve_artifact(&server, &jpeg, jpeg.clone(), "image/jpeg").await;

        let avatar = fetch_bot_avatar(&reference, &BotAvatars::loopback().policy)
            .await
            .expect("transcoded avatar");
        let png = BASE64
            .decode(xep0084::parse_avatar_data(&avatar.data()).expect("data"))
            .expect("base64");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(avatar.id(), xep0084::compute_avatar_hash(&png));
        assert_ne!(avatar.id(), xep0084::compute_avatar_hash(&jpeg));
    }

    #[tokio::test]
    async fn rejects_artifacts_that_are_not_the_pinned_image() {
        let server = MockServer::start().await;
        let png = encode_image(2, 2, image::ImageFormat::Png);
        let policy = BotAvatars::loopback().policy;

        let other = serve_artifact(&server, b"another avatar", png.clone(), "image/png").await;
        assert!(matches!(
            fetch_bot_avatar(&other, &policy).await,
            Err(BotAvatarError::Fetch(FetchError::DigestMismatch))
        ));

        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
        let svg = serve_artifact(&server, &svg, svg.clone(), "image/svg+xml").await;
        assert!(matches!(
            fetch_bot_avatar(&svg, &policy).await,
            Err(BotAvatarError::Fetch(FetchError::MimeRejected(_)))
        ));

        let mislabelled = serve_artifact(&server, &png, png.clone(), "image/gif").await;
        assert!(matches!(
            fetch_bot_avatar(&mislabelled, &policy).await,
            Err(BotAvatarError::Fetch(FetchError::MagicByteMismatch))
        ));

        let truncated = png[..16].to_vec();
        let truncated = serve_artifact(&server, &truncated, truncated.clone(), "image/png").await;
        assert!(matches!(
            fetch_bot_avatar(&truncated, &policy).await,
            Err(BotAvatarError::Image(_))
        ));

        let mut oversized = png;
        oversized.resize(MAX_BYTES + 1, 0);
        let oversized = serve_artifact(&server, &oversized, oversized.clone(), "image/png").await;
        assert!(matches!(
            fetch_bot_avatar(&oversized, &policy).await,
            Err(BotAvatarError::Fetch(FetchError::SizeExceeded(MAX_BYTES)))
        ));
    }

    #[test]
    fn retry_delay_doubles_up_to_a_cap() {
        assert_eq!(retry_delay(1), RETRY_DELAY);
        assert_eq!(retry_delay(2), RETRY_DELAY * 2);
        assert_eq!(retry_delay(3), RETRY_DELAY * 4);
        assert_eq!(retry_delay(u32::MAX), MAX_RETRY_DELAY);
    }

    /// The first `get` starts the fetch in the background and answers
    /// without an avatar; a later one has it.
    #[tokio::test]
    async fn get_fetches_lazily_without_waiting() {
        let server = MockServer::start().await;
        let png = encode_image(2, 2, image::ImageFormat::Png);
        let reference = serve_artifact(&server, &png, png.clone(), "image/png").await;
        let avatars = Arc::new(BotAvatars::loopback());

        assert!(avatars.get(&reference).is_none(), "nothing cached yet");
        let avatar = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(avatar) = avatars.get(&reference) {
                    return avatar;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the background fetch caches the avatar");
        assert_eq!(avatar.id(), xep0084::compute_avatar_hash(&png));
        assert_eq!(server.received_requests().await.map(|r| r.len()), Some(1));
    }

    /// A failed fetch is not retried until its backoff elapses.
    #[tokio::test]
    async fn get_retries_a_failed_fetch_only_after_its_backoff() {
        let server = MockServer::start().await;
        let png = encode_image(2, 2, image::ImageFormat::Png);
        let reference = serve_artifact(&server, b"pinned elsewhere", png, "image/png").await;
        let avatars = Arc::new(BotAvatars::loopback());

        assert!(avatars.fetch(&reference).await.is_none());
        assert!(avatars.get(&reference).is_none());
        assert!(matches!(
            avatars.slots.get(&reference.sha256).as_deref(),
            Some(Slot::Failed { failures: 1, retry_at }) if *retry_at > Instant::now()
        ));

        avatars.slots.insert(
            reference.sha256.clone(),
            Slot::Failed {
                failures: 1,
                retry_at: Instant::now(),
            },
        );
        assert!(avatars.get(&reference).is_none());
        assert!(matches!(
            avatars.slots.get(&reference.sha256).as_deref(),
            Some(Slot::Fetching { failures: 1 })
        ));
    }
}
