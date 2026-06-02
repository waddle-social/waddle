//! PubSub / PEP server module.
//!
//! Shared typed request, item, event, config, and stanza helpers now live in
//! `waddle-xmpp-core`. This module keeps server-only storage/runtime behavior
//! local while re-exporting the shared primitives for the server crate.

pub mod node;
pub mod pep;
pub mod stanzas;
pub mod storage;

pub use node::{
    AccessModel, NodeConfig, NodeConfigPatch, PublishModel, SendLastPublishedItem,
    PEP_BOOKMARK_MAX_ITEMS,
};
pub use pep::{
    build_pep_identity, is_pep_request, is_pep_request_to, pep_features, PepHandler,
    PEP_NODE_AVATAR_DATA, PEP_NODE_AVATAR_METADATA, PEP_NODE_BOOKMARKS,
};
pub use stanzas::{
    build_pubsub_affiliations_result, build_pubsub_configure_form_result, build_pubsub_error,
    build_pubsub_event, build_pubsub_items_result, build_pubsub_owner_subscriptions_result,
    build_pubsub_publish_result, build_pubsub_subscribe_result, build_pubsub_success,
    is_pubsub_event, is_pubsub_iq, parse_pubsub_event, parse_pubsub_iq, PubSubError, PubSubEvent,
    PubSubItem, PubSubRequest, NS_PUBSUB, NS_PUBSUB_ERRORS, NS_PUBSUB_EVENT, NS_PUBSUB_OWNER,
};
pub use storage::{InMemoryPubSubStorage, PubSubNode, PubSubStorage, PublishResult, StoredItem};

// Re-export typed payloads from core for convenience.
pub use waddle_xmpp_core::pubsub::{Affiliation, SubId, Subscription, SubscriptionState};
