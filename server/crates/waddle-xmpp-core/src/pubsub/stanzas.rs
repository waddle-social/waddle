//! Shared PubSub stanza parsing and building helpers.

use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

use crate::pubsub::{Affiliation, NodeConfigPatch, SubscriptionState};
use crate::{CoreError, CoreResult};

mod builders;
#[cfg(test)]
mod tests;

pub use builders::{
    build_pubsub_affiliations_result, build_pubsub_configure_form_result, build_pubsub_error,
    build_pubsub_event, build_pubsub_items_result, build_pubsub_owner_subscriptions_result,
    build_pubsub_publish_result, build_pubsub_retract_event, build_pubsub_subscribe_result,
    build_pubsub_success,
};

mod implementation;
pub use implementation::is_pubsub_event;
pub use implementation::is_pubsub_iq;
pub use implementation::parse_pubsub_event;
pub use implementation::parse_pubsub_iq;
pub use implementation::PubSubError;
pub use implementation::PubSubEvent;
pub use implementation::PubSubItem;
pub use implementation::PubSubRequest;
pub use implementation::PubSubUnsupportedFeature;
pub use implementation::NS_PUBSUB;
pub use implementation::NS_PUBSUB_ERRORS;
pub use implementation::NS_PUBSUB_EVENT;
pub use implementation::NS_PUBSUB_OWNER;
