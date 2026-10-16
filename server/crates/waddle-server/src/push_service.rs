//! First-party XMPP Push Service storage and provider dispatch (Web Push,
//! APNs and FCM).
//!
//! This module is the Push Service side of XEP-0357. It deliberately does not
//! store user-server `<enable/>` registration state; that remains in
//! [`crate::push_registrations`]. Provider endpoints and tokens live here,
//! behind the `push.<domain>` service boundary.

mod apns_config;
mod apns_dispatch;
pub mod commands;
mod devices;
pub(crate) mod dispatch;
mod fcm_config;
mod fcm_dispatch;
mod nodes;
mod publish;
mod publish_jobs;
mod pubsub_backing;
mod registration;
mod secrets;
mod store;
#[cfg(test)]
pub(crate) mod test_support;
mod types;
pub mod vapid_storage;
mod worker;

pub use apns_config::{ApnsConfig, ApnsConfigError, ApnsConfigVar};
pub use fcm_config::{FcmConfig, FcmConfigError, FcmConfigVar, FcmTokenSourceLoadError};
pub(crate) use publish_jobs::acknowledge_completed_outbox_tx;
pub use pubsub_backing::ensure_xep0060_push_node;
pub(crate) use secrets::PushSecretCipher;
pub use store::DatabasePushServiceStore;
pub(crate) use types::{CanonicalNotificationTerminalProof, PushQueueAcceptanceError};
pub use types::{
    PushDeliveryAttempt, PushDevicePlatform, PushDeviceRegistration, PushFanoutResult,
    PushPublishJob, PushPublishJobEnqueue, PushServiceDevice, PushServiceNode,
};
