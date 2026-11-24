//! XEP-0163 §4.3 / "Sending the Last Published Item": replay the last
//! published item of each PEP node to a newly-available resource.
//!
//! > a PEP service MUST send the last published item to all new
//! > subscribers and to all newly-available resources for each
//! > subscriber, including the account owner itself.
//!
//! The recipient's interest is the `<node>+notify` feature of its
//! XEP-0115 caps. The replay therefore runs once the caps of the
//! resource are known: right after the initial presence when the
//! advertised `(hash, ver)` is already cached, or when the disco#info
//! round-trip for an unknown `ver` completes.

use std::collections::HashSet;

use jid::{BareJid, FullJid, Jid};
use tracing::{debug, warn};
use waddle_xmpp::pubsub::{
    build_pubsub_event, PubSubEvent, PubSubItem, PubSubNode, SendLastPublishedItem,
};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Stanza;

use super::implementation::private_node_subscriber_allowed;
use super::WebSocketState;

/// Send `recipient` the last published item of every PEP node it may read
/// and is interested in (`<node>+notify` in its cached caps) whose
/// `pubsub#send_last_published_item` is `on_sub_and_presence`.
///
/// Sources are the account owner itself (§4.3: "including the account owner
/// itself") and every contact whose presence the recipient's account
/// receives. Nodes whose `<node>+notify` feature is in `already_notified`
/// are skipped, so a caps update replays only nodes it newly added.
/// XEP-0191 blocking is honoured in both directions, failing closed.
pub(crate) async fn replay_last_published_items(
    state: &WebSocketState,
    recipient: &FullJid,
    already_notified: &HashSet<String>,
) {
    let resolver = &state.deps.protocol.caps_resolver;
    let Some(features) = resolver
        .key_for_resource(recipient)
        .and_then(|key| resolver.cached(&key))
        .map(|cached| {
            cached
                .features
                .into_iter()
                .map(|feature| feature.0)
                .collect::<HashSet<String>>()
        })
    else {
        return;
    };
    if !features.iter().any(|feature| feature.ends_with("+notify")) {
        return;
    }

    let recipient_bare = recipient.to_bare();
    let roster = state.deps.app_state.db_pool.roster_storage();
    let contacts = match roster.get_presence_subscription_jids(&recipient_bare).await {
        Ok(contacts) => contacts,
        Err(error) => {
            warn!(
                error = %error,
                jid = %recipient,
                "Skipping contact last-item replay: failed to load presence subscriptions"
            );
            Vec::new()
        }
    };

    let blocking = state.deps.app_state.db_pool.blocking_storage();
    let recipient_blocklist = match blocking.load_blocklist(&recipient_bare).await {
        Ok(list) => Some(list),
        Err(error) => {
            warn!(
                error = %error,
                jid = %recipient,
                "Skipping contact last-item replay: failed to load recipient blocklist; \
                 failing closed"
            );
            None
        }
    };

    let mut sources: Vec<BareJid> = vec![recipient_bare.clone()];
    if let Some(recipient_blocklist) = &recipient_blocklist {
        for contact in contacts {
            if contact == recipient_bare
                || !state.serves(&contact)
                || recipient_blocklist.contains_jid(&Jid::from(contact.clone()))
            {
                continue;
            }
            match blocking.load_blocklist(&contact).await {
                Ok(list) if !list.contains_jid(&Jid::from(recipient.clone())) => {
                    sources.push(contact);
                }
                Ok(_) => {}
                Err(error) => warn!(
                    error = %error,
                    contact = %contact,
                    "Skipping last-item replay from contact: failed to load its blocklist; \
                     failing closed"
                ),
            }
        }
    }

    for owner in sources {
        replay_owner_nodes(state, recipient, &owner, &features, already_notified).await;
    }
}

async fn replay_owner_nodes(
    state: &WebSocketState,
    recipient: &FullJid,
    owner: &BareJid,
    features: &HashSet<String>,
    already_notified: &HashSet<String>,
) {
    let storage = &state.deps.protocol.pubsub_storage;
    let nodes = match storage.list_nodes(owner).await {
        Ok(nodes) => nodes,
        Err(error) => {
            warn!(owner = %owner, error = %error, "Failed to list PEP nodes for last-item replay");
            return;
        }
    };
    for node in nodes {
        let filter = format!("{node}+notify");
        if !features.contains(&filter) || already_notified.contains(&filter) {
            continue;
        }
        let record = match storage.get_node(owner, &node).await {
            Ok(Some(record)) => record,
            Ok(None) => continue,
            Err(error) => {
                warn!(owner = %owner, node, error = %error, "Failed to load node for last-item replay");
                continue;
            }
        };
        if record.config.send_last_published_item != SendLastPublishedItem::OnSubAndPresence {
            continue;
        }
        if !recipient_may_read(state, owner, &node, &record, &recipient.to_bare()).await {
            continue;
        }
        let stored = match storage.get_items(owner, &node, Some(1), &[]).await {
            Ok(mut items) => match items.pop() {
                Some(item) => item,
                None => continue,
            },
            Err(error) => {
                warn!(owner = %owner, node, error = %error, "Failed to read last item for replay");
                continue;
            }
        };

        let mut item: PubSubItem = stored.to_pubsub_item();
        item.publisher = item.publisher.filter(|publisher| publisher != owner);
        if !record.config.deliver_payloads {
            item.payload = None;
        }
        let from = Jid::from(owner.clone());
        let mut message = build_pubsub_event(
            &from,
            &Jid::from(recipient.clone()),
            &PubSubEvent::new(node.clone(), vec![item]),
        );
        // XEP-0163 example: the replayed notification carries the
        // publication time as a XEP-0203 delay.
        message
            .payloads
            .push(minidom::Element::from(xmpp_parsers::delay::Delay {
                from: Some(from),
                stamp: xmpp_parsers::date::DateTime(stored.published_at.fixed_offset()),
                data: None,
            }));

        match state
            .deps
            .protocol
            .connection_registry
            .try_send_to(recipient, Stanza::Message(message))
        {
            BroadcastOutcome::Delivered => {}
            outcome => debug!(
                jid = %recipient,
                owner = %owner,
                node,
                ?outcome,
                "Last-item replay not delivered"
            ),
        }
    }
}

/// Whether `reader` may be told of the node's items. The owner's own
/// account goes through the node's subscribe authorization; a contact
/// (whose presence subscription is established by the caller) additionally
/// follows the same entitlement rules as publish fan-out.
async fn recipient_may_read(
    state: &WebSocketState,
    owner: &BareJid,
    node: &str,
    record: &PubSubNode,
    reader: &BareJid,
) -> bool {
    if reader == owner {
        return match crate::pubsub_authz::effective_affiliation(
            &state.deps.protocol.pubsub_storage,
            owner,
            node,
            reader,
            true,
        )
        .await
        {
            Ok(affiliation) => crate::pubsub_authz::can_subscribe_with_config(
                &record.config,
                affiliation,
                owner,
                node,
                reader,
                true,
            ),
            Err(_) => false,
        };
    }
    if crate::pubsub_authz::is_owner_only_pep_node(node) {
        return false;
    }
    match record.config.access_model {
        waddle_xmpp::pubsub::AccessModel::Whitelist
        | waddle_xmpp::pubsub::AccessModel::Authorize => {
            private_node_subscriber_allowed(state, owner, node, &record.config, reader).await
        }
        waddle_xmpp::pubsub::AccessModel::Open => true,
        // The same decision as item retrieval and subscribe: a presence
        // subscription for `presence`, an allowed roster group for
        // `roster` (XEP-0060 §4.5.1; XEP-0163 §4).
        waddle_xmpp::pubsub::AccessModel::Presence | waddle_xmpp::pubsub::AccessModel::Roster => {
            matches!(
                crate::pubsub_authz::pep_read_decision(
                    &state.deps.protocol.pubsub_storage,
                    &crate::pubsub_authz::ContactDirectory::new(&state.deps.app_state.db_pool),
                    owner,
                    node,
                    reader,
                )
                .await,
                Ok(crate::pubsub_authz::ReadDecision::Allowed)
            )
        }
    }
}
