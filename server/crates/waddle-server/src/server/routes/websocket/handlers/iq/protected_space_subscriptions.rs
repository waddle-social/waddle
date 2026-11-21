use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::pubsub::{
    build_pubsub_error, build_pubsub_subscribe_result, build_pubsub_success, parse_pubsub_iq,
    PubSubError, PubSubRequest, SubId, NS_PUBSUB,
};
use xmpp_parsers::iq::Iq;

use super::{iq_to_xml, Session, WebSocketState};
use crate::db::local_space_subscriptions::{LocalSpaceSubscriptions, LocalSpaceUnsubscribe};
use crate::permissions::host_grant::{GrantSubject, ScopeKey};
use crate::server::routes::websocket::frame::ResponseFrame;

pub(super) async fn reply(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&FullJid>,
    session: Option<&Session>,
) -> Option<ResponseFrame> {
    let Iq::Set { payload, .. } = iq else {
        return None;
    };
    let service = state.deps.service_domains.spaces.as_str();
    if iq.to()?.as_str() != service || !payload.is("pubsub", NS_PUBSUB) {
        return None;
    }
    let (node, jid, operation) = match parse_pubsub_iq(iq).ok()? {
        PubSubRequest::Subscribe { node, jid } => (node, jid, None),
        PubSubRequest::Unsubscribe { node, jid, subid } => (node, jid, Some(subid)),
        _ => return None,
    };
    let gate = &state.deps.app_state.host_grants;
    let error = |error| {
        Some(ResponseFrame::from(iq_to_xml(build_pubsub_error(
            iq, error,
        ))))
    };
    match gate.space_enrolled(&node).await {
        Ok(false) => return None,
        Ok(true) => {}
        Err(_) => return error(PubSubError::InternalServerError),
    }
    let Some(requester) = requester else {
        return error(PubSubError::Forbidden);
    };
    let bare = requester.to_bare();
    if session.is_none_or(|session| session.user_jid != bare.as_str())
        || iq
            .from()
            .is_some_and(|from| from.as_str() != requester.as_str())
        || jid.as_str() != bare.as_str()
    {
        return error(PubSubError::Forbidden);
    }
    if payload.children().count() != 1
        || payload
            .children()
            .next()
            .is_some_and(|child| child.children().next().is_some())
    {
        return error(PubSubError::BadRequest);
    }
    let (Ok(service), Ok(account)) = (service.parse::<BareJid>(), GrantSubject::new(bare)) else {
        return error(PubSubError::BadRequest);
    };
    let Ok(key) = ScopeKey::new(service, &node) else {
        return error(PubSubError::BadRequest);
    };
    let Some(attachment) = gate.attachment() else {
        return error(PubSubError::NodeNotFound);
    };
    let subscriptions = LocalSpaceSubscriptions::new(state.deps.app_state.db_pool.global().clone());
    if let Some(subid) = operation {
        let subid = subid.map(SubId::from_raw);
        return match subscriptions
            .remove_own(&key, &account, attachment.served(), subid.as_ref())
            .await
        {
            Ok(LocalSpaceUnsubscribe::Removed) => {
                Some(ResponseFrame::from(iq_to_xml(build_pubsub_success(iq))))
            }
            Ok(LocalSpaceUnsubscribe::Absent) => error(PubSubError::NotSubscribed),
            Ok(LocalSpaceUnsubscribe::Invalid) => error(PubSubError::InvalidSubscriptionId),
            Ok(LocalSpaceUnsubscribe::Required) => error(PubSubError::SubscriptionIdRequired),
            Err(_) => error(PubSubError::InternalServerError),
        };
    }
    let Some(decision) = gate.space_read_local(&key, &account).await else {
        return error(PubSubError::NodeNotFound);
    };
    let subscription = match subscriptions
        .record(
            &key,
            &account,
            attachment.served(),
            decision.subscription_grant(),
        )
        .await
    {
        Ok(Some(subscription)) => subscription,
        Ok(None) => return error(PubSubError::NodeNotFound),
        Err(_) => return error(PubSubError::InternalServerError),
    };
    let mut response = build_pubsub_subscribe_result(
        iq,
        &node,
        &Jid::from(account.jid().clone()),
        &subscription.subid,
    );
    if let Iq::Result { to, .. } = &mut response {
        *to = Some(requester.clone().into());
    }
    let Some(grant) = gate
        .local_space_subscribe_reply(&key, requester, iq, subscription)
        .await
    else {
        return error(PubSubError::NodeNotFound);
    };
    match ResponseFrame::protected_space_frame(iq_to_xml(response), grant) {
        Ok(frame) => Some(frame),
        Err(_) => error(PubSubError::InternalServerError),
    }
}
