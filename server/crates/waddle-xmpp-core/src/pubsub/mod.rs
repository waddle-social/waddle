//! Shared PubSub / PEP primitives and stanza helpers.

pub mod affiliation;
pub mod node;
pub mod pep;
pub mod stanzas;
pub mod subscription;

pub use affiliation::Affiliation;
pub use node::{AccessModel, NodeConfig, PublishModel, SendLastPublishedItem};
pub use pep::{build_pep_identity, is_pep_request, is_pep_request_to, pep_features, PepHandler};
pub use stanzas::{
    build_pubsub_error, build_pubsub_event, build_pubsub_items_result, build_pubsub_publish_result,
    build_pubsub_success, is_pubsub_event, is_pubsub_iq, parse_pubsub_event, parse_pubsub_iq,
    PubSubError, PubSubEvent, PubSubItem, PubSubRequest, NS_PUBSUB, NS_PUBSUB_ERRORS,
    NS_PUBSUB_EVENT, NS_PUBSUB_OWNER,
};
pub use subscription::{SubId, Subscription, SubscriptionState};
