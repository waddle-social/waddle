use jid::Jid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Message, MessageType};

use crate::pubsub::{Affiliation, NodeConfig, SubId};

use super::{
    PubSubError, PubSubEvent, PubSubItem, NS_PUBSUB, NS_PUBSUB_ERRORS, NS_PUBSUB_EVENT,
    NS_PUBSUB_OWNER,
};

/// Build a PubSub event notification message.
///
/// The message type is `headline` per XEP-0060 §12.18 (default
/// `pubsub#notification_type`) and XEP-0163 §4.3 (PEP MUST be headline).
/// Headline messages are not stored offline (RFC 6121 §8.5.2.1.4), which
/// matches PubSub's catch-up-via-`<items/>` recovery model.
pub fn build_pubsub_event(from: &Jid, to: &Jid, event: &PubSubEvent) -> Message {
    let mut items_elem = Element::builder("items", NS_PUBSUB_EVENT).attr("node", &event.node);

    for item in &event.items {
        items_elem = items_elem.append(item.to_element(NS_PUBSUB_EVENT));
    }

    let event_elem = Element::builder("event", NS_PUBSUB_EVENT)
        .append(items_elem.build())
        .build();

    let mut message = Message::new_with_type(MessageType::Headline, Some(to.clone()));
    message.from = Some(from.clone());
    message.payloads.push(event_elem);
    message
}

/// Build a PubSub items result IQ.
pub fn build_pubsub_items_result(original_iq: &Iq, node: &str, items: &[PubSubItem]) -> Iq {
    let mut items_elem = Element::builder("items", NS_PUBSUB).attr("node", node);

    for item in items {
        items_elem = items_elem.append(item.to_element(NS_PUBSUB));
    }

    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(items_elem.build())
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build a PubSub publish result IQ.
pub fn build_pubsub_publish_result(original_iq: &Iq, node: &str, item_id: &str) -> Iq {
    let item_elem = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id)
        .build();

    let publish_elem = Element::builder("publish", NS_PUBSUB)
        .attr("node", node)
        .append(item_elem)
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish_elem)
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build a PubSub error IQ response.
pub fn build_pubsub_error(original_iq: &Iq, error: PubSubError) -> Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let (error_type, defined_condition) = match error {
        PubSubError::NodeNotFound | PubSubError::ItemNotFound => {
            (ErrorType::Cancel, DefinedCondition::ItemNotFound)
        }
        PubSubError::Forbidden => (ErrorType::Auth, DefinedCondition::Forbidden),
        PubSubError::NodeExists | PubSubError::PreconditionNotMet => {
            (ErrorType::Cancel, DefinedCondition::Conflict)
        }
        PubSubError::InvalidJid => (ErrorType::Modify, DefinedCondition::BadRequest),
        PubSubError::NotSubscribed => (ErrorType::Cancel, DefinedCondition::UnexpectedRequest),
        PubSubError::InternalServerError => {
            (ErrorType::Wait, DefinedCondition::InternalServerError)
        }
        PubSubError::UnsupportedFeature(_) => {
            (ErrorType::Cancel, DefinedCondition::FeatureNotImplemented)
        }
    };

    let mut stanza_error = StanzaError::new(error_type, defined_condition, "en", "");
    if let PubSubError::UnsupportedFeature(feature) = error {
        stanza_error.other = Some(
            Element::builder("unsupported", NS_PUBSUB_ERRORS)
                .attr("feature", feature.as_str())
                .build(),
        );
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Error(stanza_error),
    }
}

/// Build an empty result IQ for successful requests without a payload.
pub fn build_pubsub_success(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(None),
    }
}

/// Build a `<subscribe/>` result IQ that carries `subscription` (XEP-0060 §6.1.6).
pub fn build_pubsub_subscribe_result(
    original_iq: &Iq,
    node: &str,
    subscriber: &jid::Jid,
    subid: &SubId,
) -> Iq {
    let subscription = Element::builder("subscription", NS_PUBSUB)
        .attr("node", node)
        .attr("jid", subscriber.to_string())
        .attr("subid", subid.to_string())
        .attr("subscription", "subscribed")
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(subscription)
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build an `<affiliations/>` result IQ for `<affiliations node='...'/>` get.
pub fn build_pubsub_affiliations_result(
    original_iq: &Iq,
    node: &str,
    rows: &[(jid::BareJid, Affiliation)],
) -> Iq {
    let mut affs = Element::builder("affiliations", NS_PUBSUB_OWNER).attr("node", node);
    for (entity, aff) in rows {
        affs = affs.append(
            Element::builder("affiliation", NS_PUBSUB_OWNER)
                .attr("jid", entity.to_string())
                .attr("affiliation", aff.to_string())
                .build(),
        );
    }
    let pubsub = Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(affs.build())
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}

/// Build the result for a `<configure/>` get carrying current node config
/// as a `<x type='form'/>` data form (XEP-0060 §6.4).
pub fn build_pubsub_configure_form_result(original_iq: &Iq, node: &str, config: &NodeConfig) -> Iq {
    fn field(var: &str, value: &str) -> Element {
        Element::builder("field", "jabber:x:data")
            .attr("var", var)
            .append(
                Element::builder("value", "jabber:x:data")
                    .append(value)
                    .build(),
            )
            .build()
    }
    /// Build a `type='hidden'` field per XEP-0004 §3.2.
    fn hidden_field(var: &str, value: &str) -> Element {
        Element::builder("field", "jabber:x:data")
            .attr("var", var)
            .attr("type", "hidden")
            .append(
                Element::builder("value", "jabber:x:data")
                    .append(value)
                    .build(),
            )
            .build()
    }
    let form = Element::builder("x", "jabber:x:data")
        .attr("type", "form")
        .append(hidden_field(
            "FORM_TYPE",
            "http://jabber.org/protocol/pubsub#node_config",
        ))
        .append(field(
            "pubsub#access_model",
            &config.access_model.to_string(),
        ))
        .append(field(
            "pubsub#publish_model",
            &config.publish_model.to_string(),
        ))
        .append(field("pubsub#max_items", &config.max_items.to_string()))
        .append(field(
            "pubsub#persist_items",
            if config.persist_items { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#deliver_payloads",
            if config.deliver_payloads { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#notify_retract",
            if config.notify_retract { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#notify_delete",
            if config.notify_delete { "1" } else { "0" },
        ))
        .append(field(
            "pubsub#send_last_published_item",
            &config.send_last_published_item.to_string(),
        ))
        .build();
    let configure = Element::builder("configure", NS_PUBSUB_OWNER)
        .attr("node", node)
        .append(form)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(configure)
        .build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(pubsub)),
    }
}
