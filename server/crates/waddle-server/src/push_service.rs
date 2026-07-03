//! First-party XMPP Push Service storage and fake provider dispatch.
//!
//! This module is the Push Service side of XEP-0357. It deliberately does not
//! store user-server `<enable/>` registration state; that remains in
//! [`crate::push_registrations`]. Provider endpoints and tokens live here,
//! behind the `push.<domain>` service boundary.

pub mod commands;
mod devices;
pub(crate) mod dispatch;
mod nodes;
mod publish;
mod publish_jobs;
mod pubsub_backing;
mod registration;
mod secrets;
mod store;
#[cfg(test)]
mod test_support;
mod types;
pub mod vapid_storage;
mod worker;

pub use pubsub_backing::ensure_xep0060_push_node;
pub(crate) use secrets::PushSecretCipher;
pub use store::DatabasePushServiceStore;
pub use types::{
    PushDeliveryAttempt, PushDevicePlatform, PushDeviceRegistration, PushFanoutResult,
    PushPublishJob, PushPublishJobEnqueue, PushServiceDevice, PushServiceNode,
};
