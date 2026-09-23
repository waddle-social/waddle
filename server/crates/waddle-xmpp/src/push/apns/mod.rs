//! Apple Push Notification service (APNs) provider transport.
//!
//! The Push Service (`push.<domain>`) is the only component that holds
//! APNs device tokens and the `.p8` provider key (#529). This module is
//! the typed seam between the publish-job worker and Apple's HTTP/2
//! provider API:
//!
//! - [`environment`]: production vs sandbox host selection.
//! - [`identifiers`]: validated device token, topic (bundle id), team
//!   id, key id, and collapse id newtypes.
//! - [`provider_token`]: ES256 provider authentication token signer
//!   with a 50-minute reuse cache and an injectable clock.
//! - [`payload`]: the minimal alert payload (no sender, no body) built
//!   from serde structs and bounded by Apple's 4096-byte limit.
//! - [`outcome`]: Apple's documented rejection reasons and the typed
//!   delivery outcome the worker maps to attempt statuses.
//! - [`sender`]: the object-safe [`ApnsSender`] trait and the reqwest
//!   HTTP/2 implementation.

pub mod environment;
pub mod identifiers;
pub mod outcome;
pub mod payload;
pub mod provider_token;
pub mod sender;

pub use environment::ApnsEnvironment;
pub use identifiers::{
    ApnsCollapseId, ApnsDeviceToken, ApnsIdentifierError, ApnsKeyId, ApnsTeamId, ApnsTopic,
};
pub use outcome::{ApnsId, ApnsOutcome, ApnsReason, ApnsTransient};
pub use payload::{
    ApnsAlertLocKey, ApnsPayload, ApnsPayloadError, ApnsPayloadFields, EncodedApnsPayload,
    MAX_APNS_PAYLOAD_BYTES,
};
pub use provider_token::{
    ApnsClock, ApnsKeyError, ApnsProviderJwt, ApnsProviderTokenSource, ApnsSignError,
    CachingApnsTokenSigner, SystemApnsClock, APNS_PROVIDER_TOKEN_MIN_REFRESH,
    APNS_PROVIDER_TOKEN_REUSE,
};
pub use sender::{
    ApnsExpiration, ApnsPriority, ApnsRequest, ApnsSender, ApnsSenderBuildError, HttpApnsSender,
};
