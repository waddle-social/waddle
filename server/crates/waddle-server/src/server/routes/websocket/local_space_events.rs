use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::stream_management::persistence::SmReplayPayload;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

use super::transport_xml::stanza_to_xml;
use super::WebSocketState;
use crate::db::local_space_subscriptions::LocalSpaceSubscription;
use crate::permissions::host_grant::{HostGrantGate, ScopeKey, SpaceBookmarkVersion};

const MAX_LOCAL_RECIPIENTS: usize = 256;
const FANOUT_DEADLINE: Duration = Duration::from_secs(5);

pub(super) fn queue(
    state: &WebSocketState,
    service: &BareJid,
    node: &str,
    event: &Message,
    version: &SpaceBookmarkVersion,
    audience: Arc<[LocalSpaceSubscription]>,
) {
    if audience.is_empty() {
        return;
    }
    let Ok(permit) = Arc::clone(&state.local_space_publications).try_acquire_owned() else {
        return;
    };
    let Ok(key) = ScopeKey::new(service.clone(), node) else {
        return;
    };
    let registry = Arc::clone(&state.deps.protocol.connection_registry);
    let gate = state.deps.app_state.host_grants.clone();
    #[cfg(feature = "clustering")]
    let bridge = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .clone();
    let event = event.clone();
    let version = version.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let fanout = deliver(&registry, &gate, &key, &event, &version, &audience, None);
        #[cfg(not(feature = "clustering"))]
        fanout.await;
        #[cfg(feature = "clustering")]
        {
            let peers = async {
                if let Some(bridge) = bridge {
                    bridge
                        .offer_space_bookmark_publication(key.service(), key.node(), &version)
                        .await;
                }
            };
            tokio::join!(fanout, peers);
        }
    });
}

type NodeFence<'a> = (
    &'a waddle_xmpp::ownership::SharedNodeIdentity,
    &'a waddle_xmpp::ownership::NodeIdentityStamp,
);

async fn deliver(
    registry: &ConnectionRegistry,
    gate: &HostGrantGate,
    key: &ScopeKey,
    event: &Message,
    version: &SpaceBookmarkVersion,
    audience: &[LocalSpaceSubscription],
    node_fence: Option<NodeFence<'_>>,
) {
    let _ = tokio::time::timeout(FANOUT_DEADLINE, async {
        let subscriptions: BTreeMap<_, _> = audience
            .iter()
            .map(|subscription| (subscription.account.jid(), subscription))
            .collect();
        let recipients = registry.list_connections().into_iter().filter_map(|jid| {
            let subscription = subscriptions.get(&jid.to_bare())?;
            let entry = registry
                .entry(&jid)
                .filter(|entry| entry.is_locally_hosted())?;
            Some((jid, entry.binding_id(), *subscription))
        });
        for (jid, binding, subscription) in recipients.take(MAX_LOCAL_RECIPIENTS) {
            let Some(descriptor) = gate
                .local_space_bookmark_event(
                    key,
                    &jid,
                    event.id.as_ref().map(|id| id.0.as_str()),
                    subscription,
                    version,
                )
                .await
            else {
                continue;
            };
            let mut message = event.clone();
            message.to = Some(jid.clone().into());
            let xml = stanza_to_xml(&Stanza::Message(message));
            let Ok(payload) = SmReplayPayload::from_protected_space_reply(xml, descriptor) else {
                continue;
            };
            let offer = || registry.try_send_protected_space_event_if_owner(&jid, binding, payload);
            if let Some((identity, stamp)) = node_fence {
                if identity.with_current_stamp(stamp, |_| offer()).is_none() {
                    return;
                }
            } else {
                let _ = offer();
            }
        }
    })
    .await;
}

#[cfg(feature = "clustering")]
pub(crate) async fn deliver_hint(
    state: &WebSocketState,
    service: &BareJid,
    node: &str,
    notification: crate::permissions::host_grant::SpaceBookmarkNotification,
    identity: &waddle_xmpp::ownership::SharedNodeIdentity,
    stamp: &waddle_xmpp::ownership::NodeIdentityStamp,
) {
    use waddle_xmpp::pubsub::{build_pubsub_event, PubSubEvent};
    use waddle_xmpp_core::pubsub::stanzas::build_pubsub_retract_event;

    let Ok(key) = ScopeKey::new(service.clone(), node) else {
        return;
    };
    let sender: jid::Jid = service.clone().into();
    let event = if notification.version.present {
        let Some(mut item) = notification.item else {
            return;
        };
        item.publisher = None;
        if !notification.deliver_payloads {
            item.payload = None;
        }
        build_pubsub_event(
            &sender,
            &sender,
            &PubSubEvent::new(node.to_owned(), vec![item]),
        )
    } else if notification.notify_retract {
        build_pubsub_retract_event(&sender, &sender, node, notification.version.room.as_str())
    } else {
        return;
    };
    deliver(
        &state.deps.protocol.connection_registry,
        &state.deps.app_state.host_grants,
        &key,
        &event,
        &notification.version,
        &notification.local_audience,
        Some((identity, stamp)),
    )
    .await;
}
