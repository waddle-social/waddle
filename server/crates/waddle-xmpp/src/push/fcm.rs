//! Firebase Cloud Messaging (FCM) HTTP v1 provider transport.
//!
//! The Push Service (`push.<domain>`) is the only component that holds
//! FCM registration tokens and the service-account key (#530). This
//! module is the typed seam between the publish-job worker and Google's
//! `messages:send` API:
//!
//! - [`identifiers`]: validated project id, registration token and
//!   Android application id newtypes.
//! - [`access_token`]: the service-account key and the cached,
//!   single-flight OAuth 2.0 JWT-bearer access-token source.
//! - [`message`]: the data-only message whose single field carries the
//!   RFC 8291 ciphertext of the v1 push envelope, bounded by FCM's
//!   4096-byte data limit.
//! - [`outcome`]: FCM's error-body classification and the typed delivery
//!   outcome the worker maps to attempt statuses.
//! - [`sender`]: the object-safe [`FcmSender`] trait and the reqwest
//!   implementation.

pub mod access_token;
pub mod identifiers;
pub mod message;
pub mod outcome;
pub mod sender;

pub use access_token::{
    FcmAccessToken, FcmAccessTokenSource, FcmClock, FcmServiceAccountError, FcmServiceAccountKey,
    FcmTokenError, FcmTokenSourceBuildError, OAuthErrorCode, OAuthFcmTokenSource, SystemFcmClock,
    FCM_ACCESS_TOKEN_REFRESH_MARGIN, FCM_OAUTH_SCOPE, GOOGLE_OAUTH_TOKEN_URL,
};
pub use identifiers::{FcmAppId, FcmIdentifierError, FcmProjectId, FcmRegistrationToken};
pub use message::{
    EncodedFcmMessage, FcmMessage, FcmMessageError, FcmPriority, FcmTtl, FCM_DATA_KEY_PAYLOAD,
    MAX_FCM_DATA_BYTES,
};
pub use outcome::{FcmErrorCode, FcmErrorDetail, FcmOutcome, FcmTransient, GoogleRpcStatus};
pub use sender::{FcmRequest, FcmSender, FcmSenderBuildError, HttpFcmSender, FCM_API_BASE_URL};
