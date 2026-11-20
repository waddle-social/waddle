use jid::{BareJid, Jid};
use waddle_xmpp::pubsub::{
    build_pubsub_error, build_pubsub_event, build_pubsub_publish_result,
    build_pubsub_success, Affiliation, PubSubError, PubSubEvent, PubSubItem, PubSubRequest, NS_PUBSUB,
};
use waddle_xmpp_core::pubsub::stanzas::{build_pubsub_retract_event, PubSubUnsupportedFeature};
use xmpp_parsers::iq::Iq;

use super::{Session, WebSocketState};
use crate::permissions::host_grant::{
    GrantSubject, HostGrantError, ScopeKey, SpaceBookmarkAction, SpaceBookmarkChange,
    SpaceBookmarkOutcome,
};
use crate::server::routes::websocket::s2s_spaces::queue_remote_notification;

pub(super) async fn write(
    iq: &Iq,
    state: &WebSocketState,
    service: &BareJid,
    user: &BareJid,
    session: Option<&Session>,
    request: PubSubRequest,
) -> Iq {
    let Iq::Set { payload, .. } = iq else {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    };
    if iq.to().map(Jid::as_str) != Some(service.as_str()) || !payload.is("pubsub", NS_PUBSUB) {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    }
    if session.is_none_or(|session| session.user_jid != user.as_str()) {
        return build_pubsub_error(iq, PubSubError::Forbidden);
    }
    let (node, id, published, notify, operation) = match request {
        PubSubRequest::Publish {
            node,
            item,
            publish_options: None,
        } => {
            let Some(id) = item.id.clone().filter(|_| item.payload.is_some()) else {
                return build_pubsub_error(iq, PubSubError::BadRequest);
            };
            (node, id, Some(item), false, "publish")
        }
        PubSubRequest::Publish {
            publish_options: Some(_),
            ..
        } => {
            return build_pubsub_error(
                iq,
                PubSubError::UnsupportedFeature(PubSubUnsupportedFeature::PublishOptions),
            );
        }
        PubSubRequest::Retract {
            node,
            item_id,
            notify,
        } => (node, item_id, None, notify, "retract"),
        _ => return build_pubsub_error(iq, PubSubError::BadRequest),
    };
    let mut operations = payload.children();
    if !operations.next().is_some_and(|request| {
        request.is(operation, NS_PUBSUB)
            && request.children().count() == 1
            && request
                .children()
                .next()
                .is_some_and(|item| item.is("item", NS_PUBSUB))
    }) || operations.next().is_some()
    {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    }
    let (Ok(room), Ok(key), Ok(initiator)) = (
        id.parse::<BareJid>(),
        ScopeKey::new(service.clone(), &node),
        GrantSubject::new(user.clone()),
    ) else {
        return build_pubsub_error(iq, PubSubError::InvalidJid);
    };
    if room.as_str() != id || room.domain().as_str() != state.deps.service_domains.muc.as_str() {
        return build_pubsub_error(iq, PubSubError::InvalidJid);
    }
    let gate = &state.deps.app_state.host_grants;
    let (Some(store), Some(attachment)) = (gate.store(), gate.attachment()) else {
        return build_pubsub_error(iq, PubSubError::InternalServerError);
    };
    let Some(policy) = state
        .deps
        .service_domains
        .host_grant_issuance_policy(attachment.principals())
    else {
        return build_pubsub_error(iq, PubSubError::InternalServerError);
    };
    let Some(decision) = gate.space_read_local(&key, &initiator).await else {
        return build_pubsub_error(iq, PubSubError::Forbidden);
    };
    if decision.admit(|affiliation| affiliation == Affiliation::Owner) != Some(true) {
        return build_pubsub_error(iq, PubSubError::Forbidden);
    }
    let binding = decision.subscription_grant();
    let Ok(Some(scope)) = store.scope(binding.lifecycle).await else {
        return build_pubsub_error(iq, PubSubError::InternalServerError);
    };
    if scope.authority_incarnation != binding.incarnation {
        return build_pubsub_error(iq, PubSubError::NodeNotFound);
    }
    let action = published
        .as_ref()
        .and_then(|item| item.payload.clone())
        .map_or(SpaceBookmarkAction::Retract, |payload| {
            SpaceBookmarkAction::Publish { payload }
        });
    let result = store
        .write_space_bookmark(
            &SpaceBookmarkChange {
                key,
                lifecycle: binding.lifecycle,
                expected_generation: scope.generation,
                expected_incarnation: binding.incarnation,
                initiator,
                room,
                action,
            },
            &policy,
        )
        .await;
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(HostGrantError::InitiatorNotAuthorized | HostGrantError::Issuance(_)) => {
            return build_pubsub_error(iq, PubSubError::Forbidden)
        }
        Err(HostGrantError::InvalidSpaceBookmark) => {
            return build_pubsub_error(iq, PubSubError::InvalidPayload)
        }
        Err(HostGrantError::UnknownRoom) => {
            return build_pubsub_error(iq, PubSubError::ItemNotFound)
        }
        Err(HostGrantError::UnknownScope | HostGrantError::SpaceNotBindable) => {
            return build_pubsub_error(iq, PubSubError::NodeNotFound)
        }
        Err(error) => {
            tracing::warn!(
                kind = error.kind(),
                "Space bookmark write could not be confirmed"
            );
            return build_pubsub_error(iq, PubSubError::InternalServerError);
        }
    };

    if let Ok(Some(record)) = state
        .deps
        .protocol
        .pubsub_storage
        .get_node(service, &node)
        .await
    {
        let sender = Jid::from(service.clone());
        match &outcome {
            SpaceBookmarkOutcome::Published { evicted_item_ids } => {
                if let Some(item) = published {
                    let item = PubSubItem {
                        publisher: None,
                        payload: record
                            .config
                            .deliver_payloads
                            .then_some(item.payload)
                            .flatten(),
                        ..item
                    };
                    let event = build_pubsub_event(
                        &sender,
                        &sender,
                        &PubSubEvent::new(node.clone(), vec![item]),
                    );
                    queue_remote_notification(state, service, &record, &event);
                }
                if record.config.notify_retract {
                    for evicted in evicted_item_ids {
                        let event = build_pubsub_retract_event(&sender, &sender, &node, evicted);
                        queue_remote_notification(state, service, &record, &event);
                    }
                }
            }
            SpaceBookmarkOutcome::Retracted { removed: true }
                if notify || record.config.notify_retract =>
            {
                let event = build_pubsub_retract_event(&sender, &sender, &node, &id);
                queue_remote_notification(state, service, &record, &event);
            }
            SpaceBookmarkOutcome::Retracted { .. } => {}
        }
    }
    match outcome {
        SpaceBookmarkOutcome::Published { .. } => build_pubsub_publish_result(iq, &node, &id),
        SpaceBookmarkOutcome::Retracted { removed: true } => build_pubsub_success(iq),
        SpaceBookmarkOutcome::Retracted { removed: false } => {
            build_pubsub_error(iq, PubSubError::ItemNotFound)
        }
    }
}
