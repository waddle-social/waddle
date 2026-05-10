//! Typed profile/avatar publish chain.
//!
//! `ensure_pep_profile_published` is the single entry point the OIDC
//! bridge calls to materialize a conformant PEP avatar + vCard set
//! for a user. The chain follows
//! XEP-0084 §4.1, XEP-0292, and XEP-0398 §3.
//!
//! Steps (a subset runs depending on which fields are set in
//! `ProfileSource`; PHOTO and FN are independent):
//!
//! 1. **(PHOTO)** Fetch the bytes from `avatar_url` per the typed
//!    fetch policy (HTTPS-only, RFC1918/loopback blocked, MIME
//!    allowlist, 100 KB cap, short timeouts).
//! 2. **(PHOTO)** `compute_hash(HashAlgo::Sha1, &bytes) -> HashValue`.
//!    Use `HashValue::to_hex()` as the item id.
//! 3. **(PHOTO)** Publish base64-encoded bytes to `urn:xmpp:avatar:data`.
//! 4. **(PHOTO)** Publish `<metadata><info id type bytes/></metadata>`
//!    to `urn:xmpp:avatar:metadata`. No `url` attribute.
//! 5. **(PHOTO and/or FN)** RMW vcard-temp: replace/insert `<PHOTO>`
//!    if PHOTO ran; replace/insert `<FN>` if FN sync ran.
//! 6. **(PHOTO and/or FN)** RMW XEP-0292 `urn:xmpp:vcard4` PEP item.
//!
//! XEP-0153 §4 (`vcard-temp:x:update` advertisement on presence) is
//! intentionally out of scope here. Modern XEP-0084-aware peers learn
//! about the avatar through the PEP fan-out triggered by step 4 / 6.
//! Legacy-only clients require server-side auto-stamping on every
//! outbound presence — tracked as a follow-up.

pub mod avatar_source;
pub mod backfill;
mod fetch;
mod publish;
mod source;
mod vcard_rmw;

pub use avatar_source::{
    acquire_per_jid_lock, read_avatar_source, record_oidc_managed, record_self_published,
    AvatarSource, AvatarSourceStorageError,
};
pub use backfill::{run_startup_backfill, spawn_startup_backfill, BackfillError, BackfillReport};
pub use fetch::{fetch_avatar_bytes, AvatarBytes, FetchError, FetchPolicy};
pub use publish::{ensure_pep_profile_published, ProfilePublishDeps};
pub use source::{NameIntent, PhotoIntent, ProfileSource, ProfileSyncError};
