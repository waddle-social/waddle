//! Push Notification Subscription Storage and Web Push Sender
//!
//! Provides a trait-based abstraction for managing push notification subscriptions
//! and sending Web Push notifications. Subscriptions are registered via XEP-0357
//! enable/disable IQ stanzas and stored for later notification delivery.

pub mod constants;
pub mod disco;
pub mod encrypt;
pub mod envelope;
pub mod limiter;
mod sender;
mod store;
pub mod types;
pub mod vapid;

pub use sender::{HttpWebPushSender, Urgency, WebPushRequest, WebPushSender};
/// In-memory push store kept reachable only behind `test-utils` (or
/// `cfg(test)` within this crate). Production code paths must depend on
/// a durable `PushSubscriptionStore` impl; PR-D2's cutover narrows the
/// public surface so accidental wiring against the in-memory fake is
/// caught at compile time outside of integration-test builds.
#[cfg(any(test, feature = "test-utils"))]
pub use store::InMemoryPushStore;
pub use store::PushSubscriptionStore;
pub use types::{
    AuthSecret, EncryptedPayload, EndpointHash, Kid, MailtoAddress, PushTopic, PushTopicParseError,
    SubscriptionKeys, SuppressionReason, TransientFailure, VapidJwt, VapidLoadError,
    VapidSignError, VapidSub, VapidSubParseError, WebPushCapability, WebPushCryptoError,
    WebPushOutcome,
};

use minidom::Element;
use thiserror::Error;

/// Errors that can occur during push operations.
#[derive(Debug, Error)]
pub enum PushError {
    /// The subscription was not found.
    #[error("push subscription not found")]
    NotFound,
    /// Failed to send a push notification.
    #[error("failed to send push notification: {0}")]
    SendFailed(String),
    /// The subscription endpoint is missing.
    #[error("push subscription endpoint is missing")]
    MissingEndpoint,
    /// An internal storage error occurred.
    #[error("push storage error: {0}")]
    StorageError(String),
    /// An HTTP request error occurred.
    #[error("HTTP request error: {0}")]
    HttpError(String),
}

/// A push notification subscription registered via XEP-0357.
#[derive(Debug, Clone)]
pub struct PushSubscription {
    /// The bare JID of the user who registered the subscription.
    pub user_jid: String,
    /// The JID of the push service.
    pub service_jid: String,
    /// The PubSub node on the push service.
    pub node: Option<String>,
    /// XEP-0060 publish-options form supplied via XEP-0357 enable.
    pub publish_options: Option<Element>,
    /// Legacy Web Push relay endpoint. Durable XEP-0357 registration storage
    /// must not persist this provider-specific value.
    pub endpoint: Option<String>,
    /// Legacy Web Push ECDH key. Durable XEP-0357 registration storage must
    /// not persist this provider-specific value.
    pub p256dh: Option<String>,
    /// Legacy Web Push auth secret. Durable XEP-0357 registration storage must
    /// not persist this provider-specific value.
    pub auth_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_error_display() {
        assert_eq!(
            PushError::NotFound.to_string(),
            "push subscription not found"
        );
        assert_eq!(
            PushError::SendFailed("timeout".into()).to_string(),
            "failed to send push notification: timeout"
        );
        assert_eq!(
            PushError::MissingEndpoint.to_string(),
            "push subscription endpoint is missing"
        );
    }

    #[test]
    fn test_push_subscription_clone_debug() {
        let sub = PushSubscription {
            user_jid: "alice@example.com".into(),
            service_jid: "push.example.com".into(),
            node: Some("web-push".into()),
            publish_options: None,
            endpoint: Some("https://push.example.com/abc".into()),
            p256dh: Some("key".into()),
            auth_key: Some("auth".into()),
        };
        let cloned = sub.clone();
        assert_eq!(cloned.user_jid, "alice@example.com");
        let debug = format!("{:?}", sub);
        assert!(debug.contains("alice@example.com"));
    }
}
