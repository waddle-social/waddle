use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use waddle_xmpp::stream_management::persistence::SmReplayPayload;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

use super::transport_xml::stanza_to_xml;
use super::WebSocketState;
use crate::db::local_space_subscriptions::LocalSpaceSubscriptions;
use crate::permissions::host_grant::{ScopeKey, SpaceBookmarkVersion};

const MAX_LOCAL_RECIPIENTS: usize = 256;
const FANOUT_DEADLINE: Duration = Duration::from_secs(5);

pub(super) fn queue(
    state: &WebSocketState,
    service: &BareJid,
    node: &str,
    event: &Message,
    version: &SpaceBookmarkVersion,
) {
    let Ok(permit) = Arc::clone(&state.local_space_publications).try_acquire_owned() else {
        return;
    };
    let Ok(key) = ScopeKey::new(service.clone(), node) else {
        return;
    };
    let store = LocalSpaceSubscriptions::new(state.deps.app_state.db_pool.global().clone());
    let registry = Arc::clone(&state.deps.protocol.connection_registry);
    let gate = state.deps.app_state.host_grants.clone();
    let event = event.clone();
    let version = version.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _ = tokio::time::timeout(FANOUT_DEADLINE, async {
            let Ok(subscriptions) = store.of_scope(&key, version.lifecycle).await else {
                return;
            };
            let subscriptions: BTreeMap<_, _> = subscriptions
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
                        &key,
                        &jid,
                        event.id.as_ref().map(|id| id.0.as_str()),
                        subscription,
                        &version,
                    )
                    .await
                else {
                    continue;
                };
                let mut message = event.clone();
                message.to = Some(jid.clone().into());
                let xml = stanza_to_xml(&Stanza::Message(message));
                if let Ok(payload) = SmReplayPayload::from_protected_space_reply(xml, descriptor) {
                    let _ =
                        registry.try_send_protected_space_event_if_owner(&jid, binding, payload);
                }
            }
        })
        .await;
    });
}
