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
    build_pubsub_affiliations_result, build_pubsub_configure_form_result,
    build_pubsub_delete_event, build_pubsub_error, build_pubsub_event, build_pubsub_items_result,
    build_pubsub_owner_subscriptions_result, build_pubsub_publish_result,
    build_pubsub_retract_event, build_pubsub_subscribe_result, build_pubsub_success,
};

mod implementation;
pub use implementation::is_pubsub_event;
pub use implementation::is_pubsub_iq;
pub use implementation::parse_publish_options_form;
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
pub use implementation::PUBLISH_OPTIONS_FORM_TYPE;

/// Match a server-generated deletion notice before handing protected bytes to a transport.
#[must_use]
pub fn matches_pubsub_delete_event(
    element: &minidom::Element,
    from: &str,
    to: &str,
    node: &str,
    id: &str,
) -> bool {
    let ns = NS_PUBSUB_EVENT;
    element.is("message", "jabber:client")
        && element.attr("from") == Some(from)
        && element.attr("to") == Some(to)
        && element.attr("type") == Some("headline")
        && element.attr("id") == Some(id)
        && element.attrs().len() == 4
        && element.children().count() == 1
        && element.text().trim().is_empty()
        && element.get_child("event", ns).is_some_and(|event| {
            event.attrs().is_empty()
                && event.children().count() == 1
                && event.text().trim().is_empty()
                && event.get_child("delete", ns).is_some_and(|delete| {
                    delete.attrs().len() == 1
                        && delete.attr("node") == Some(node)
                        && delete.children().next().is_none()
                        && delete.text().trim().is_empty()
                })
        })
}
