//! XEP-0060: Publish-Subscribe & XEP-0163: Personal Eventing Protocol
//!
//! This module implements PubSub and PEP functionality for the XMPP server.
//!
//! ## Overview
//!
//! PubSub (XEP-0060) provides a publish-subscribe framework where:
//! - Publishers can publish items to nodes
//! - Subscribers receive notifications when items are published
//! - Nodes can have various access models and configurations
//!
//! PEP (XEP-0163) is a simplified profile of PubSub that:
//! - Uses the bare JID as the service address (no separate pubsub subdomain)
//! - Auto-creates nodes on first publish
//! - Uses presence-based subscriptions by default
//!
//! ## Supported Features
//!
//! - Auto-create nodes on first publish (PEP)
//! - Access models: open, presence, roster, whitelist
//! - Item publishing and retrieval
//! - Basic event notifications
//!
//! ## XML Namespaces
//!
//! - `http://jabber.org/protocol/pubsub` - Main PubSub namespace
//! - `http://jabber.org/protocol/pubsub#event` - Event notifications
//! - `http://jabber.org/protocol/pubsub#owner` - Node owner operations
//! - `http://jabber.org/protocol/pubsub#errors` - PubSub-specific errors

pub mod node;
pub mod pep;
pub mod stanzas;
pub mod storage;

pub use node::{AccessModel, NodeConfig, PublishModel};
pub use pep::{is_pep_request, PepHandler};
pub use stanzas::{
    is_pubsub_iq, parse_pubsub_iq, build_pubsub_event, build_pubsub_items_result,
    build_pubsub_publish_result, build_pubsub_error, build_pubsub_success,
    PubSubError, PubSubItem, PubSubRequest, NS_PUBSUB, NS_PUBSUB_EVENT, NS_PUBSUB_OWNER,
};
pub use storage::{InMemoryPubSubStorage, PubSubNode, PubSubStorage, PublishResult, StoredItem};
