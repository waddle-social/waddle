//! XEP-0357 publications to the Push Services of other servers: what the
//! notification outbox sends for a registration whose Push Service is a
//! third-party client's app server (§2, §5), and what its answers decide
//! (§5.1).

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp::pubsub::NS_PUBSUB;
use waddle_xmpp::xep::NS_DATA_FORMS;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

use super::WebSocketState;
use crate::destination::{RemoteOutbound, RemoteRouteOutcome, UnservedDestination};
use crate::notification_outbox::{
    RemotePushPublication, RemotePushPublishOutcome, RemotePushPublisher,
};
use crate::s2s::route::{
    publication_id, PublicationAnswer, PublicationAnswers, PublicationOutcome,
};

/// The `<pubsub/>` payload of a publication: the notification as the item
/// of the node, with the registration's publish options when it has any.
pub(crate) fn publish_payload(publication: &RemotePushPublication) -> Element {
    let mut item = Element::builder("item", NS_PUBSUB);
    if let Some(id) = &publication.item.id {
        item = item.attr(minidom::rxml::xml_ncname!("id").to_owned(), id.as_str());
    }
    if let Some(payload) = &publication.item.payload {
        item = item.append(payload.clone());
    }
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            publication.node.as_str(),
        )
        .append(item.build());
    let mut pubsub = Element::builder("pubsub", NS_PUBSUB).append(publish.build());
    if let Some(form) = &publication.publish_options {
        debug_assert!(form.is("x", NS_DATA_FORMS));
        pubsub = pubsub.append(
            Element::builder("publish-options", NS_PUBSUB)
                .append(form.clone())
                .build(),
        );
    }
    pubsub.build()
}

/// Let the state's notification outbox publish to remote Push Services
/// through `router`, and let the services' answers decide whether their
/// registrations stay enabled. The state's remote route must be `router`.
pub(crate) fn host_remote_push(state: &Arc<WebSocketState>, router: &crate::s2s::route::S2sRouter) {
    state
        .deps
        .protocol
        .notification_outbox
        .attach_remote_publisher(Arc::new(S2sPushPublisher::new(state)));
    router.attach_publications(Arc::new(S2sPushAnswers::new(state)));
}

/// [`RemotePushPublisher`] through the state's remote route. It holds the
/// state weakly: the state holds the outbox that holds this.
pub(crate) struct S2sPushPublisher {
    state: Weak<WebSocketState>,
}

impl S2sPushPublisher {
    pub(crate) fn new(state: &Arc<WebSocketState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }
}

#[async_trait]
impl RemotePushPublisher for S2sPushPublisher {
    async fn publish(&self, publication: RemotePushPublication) -> RemotePushPublishOutcome {
        let Some(state) = self.state.upgrade() else {
            return RemotePushPublishOutcome::Refused;
        };
        let home = state.deps.auth_state.xmpp_domain.as_str();
        let service = Jid::from(publication.service.clone());
        let Some(UnservedDestination::Remote(destination)) = state
            .deps
            .service_domains
            .served_domains(home)
            .and_then(|served| served.classify(Some(&service)).unserved())
        else {
            return RemotePushPublishOutcome::Refused;
        };
        let Ok(from) = home.parse::<BareJid>() else {
            return RemotePushPublishOutcome::Refused;
        };
        // From the bare server domain, as in XEP-0357 §5's example.
        let iq = Iq::Set {
            from: Some(Jid::from(from)),
            to: Some(service),
            id: publication_id(publication.job, publication.attempt),
            payload: publish_payload(&publication),
        };
        let Ok(outbound) = RemoteOutbound::new(destination, Stanza::Iq(Box::new(iq))) else {
            return RemotePushPublishOutcome::Refused;
        };
        let routed = tokio::time::timeout(
            crate::destination::REMOTE_ROUTE_OFFER_TIMEOUT,
            state.deps.remote_route.route(outbound),
        )
        .await;
        match routed {
            Ok(RemoteRouteOutcome::Accepted) => RemotePushPublishOutcome::Accepted,
            Ok(RemoteRouteOutcome::Unroutable { reason, .. }) => {
                tracing::debug!(?reason, "remote push publication was not routed");
                RemotePushPublishOutcome::Refused
            }
            Err(_) => RemotePushPublishOutcome::Refused,
        }
    }
}

/// [`PublicationAnswers`] that disables the registration a push service
/// refused (XEP-0357 §5.1: "If a publish request is returned with an
/// IQ-error, then the server SHOULD consider the particular JID and node
/// combination to be disabled").
pub(crate) struct S2sPushAnswers {
    state: Weak<WebSocketState>,
}

impl S2sPushAnswers {
    pub(crate) fn new(state: &Arc<WebSocketState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }
}

#[async_trait]
impl PublicationAnswers for S2sPushAnswers {
    async fn answered(&self, answer: PublicationAnswer) {
        if answer.outcome != PublicationOutcome::Refused {
            return;
        }
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let protocol = &state.deps.protocol;
        let registration = match protocol
            .notification_outbox
            .job_registration(answer.job)
            .await
        {
            Ok(Some(registration)) => registration,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "a push service refused a publication; its registration could not be found");
                return;
            }
        };
        let (recipient, service, node) = registration;
        if service != answer.service {
            return;
        }
        if let Err(error) = protocol
            .push_store
            .remove(
                &recipient.to_string(),
                &service.to_string(),
                Some(node.as_str()),
            )
            .await
        {
            tracing::warn!(%error, "disabling a push registration the service refused failed");
        }
    }
}
