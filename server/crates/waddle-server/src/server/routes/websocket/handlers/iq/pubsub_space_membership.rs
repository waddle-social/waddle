//! The served owner's XEP-0060 membership changes for a bound Space.

use std::time::Duration;

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::pubsub::{Affiliation, NS_PUBSUB_ERRORS, NS_PUBSUB_OWNER};
use xmpp_parsers::iq::Iq;

use crate::permissions::host_grant::{
    AdmissionState, GrantOperationId, GrantSubject, HostGrantAuthority, HostGrantError,
    Reconciliation, ScopeKey, SetSpaceAffiliations, SpaceAffiliationChange, SpaceMembership,
    FENCE_ACKNOWLEDGEMENT_DEADLINE,
};

use super::errors::{
    feature_not_implemented_iq_error, resource_constraint_iq_error, service_unavailable_iq_error,
};
use super::{build_pubsub_error, build_pubsub_success, PubSubError, Session, WebSocketState};

const RECONCILE_DEADLINE: Duration = Duration::from_secs(5);

pub(super) async fn list_space_membership(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&jid::FullJid>,
    session: Option<&Session>,
) -> Option<crate::server::routes::websocket::frame::ResponseFrame> {
    use crate::server::routes::websocket::frame::ResponseFrame;
    use waddle_xmpp::pubsub::build_pubsub_affiliations_result;

    let Iq::Get { payload, to, .. } = iq else {
        return None;
    };
    let service = state.deps.service_domains.spaces.as_str();
    if !payload.is("pubsub", NS_PUBSUB_OWNER) || to.as_ref()?.as_str() != service {
        return None;
    }
    let node = payload
        .get_child("affiliations", NS_PUBSUB_OWNER)?
        .attr("node")?;
    let error = |error| {
        Some(ResponseFrame::from(super::iq_to_xml(build_pubsub_error(
            iq, error,
        ))))
    };
    let gate = &state.deps.app_state.host_grants;
    match gate.space_enrolled(node).await {
        Ok(false) => return None,
        Ok(true) => {}
        Err(_) => return error(PubSubError::InternalServerError),
    }
    let Some(requester) = requester else {
        return error(PubSubError::Forbidden);
    };
    if session.is_none_or(|session| session.user_jid != requester.to_bare().as_str()) {
        return error(PubSubError::Forbidden);
    }
    let Some(key) = service
        .parse()
        .ok()
        .and_then(|service| ScopeKey::new(service, node).ok())
    else {
        return error(PubSubError::BadRequest);
    };
    let Some((grant, listing)) = gate
        .local_space_affiliations_reply(&key, requester, iq)
        .await
    else {
        return error(PubSubError::Forbidden);
    };
    let mut reply = build_pubsub_affiliations_result(iq, node, &listing.rows);
    if let Iq::Result { to, .. } = &mut reply {
        *to = Some(requester.clone().into());
    }
    match ResponseFrame::protected_space_frame(super::iq_to_xml(reply), grant) {
        Ok(frame) => Some(frame),
        Err(_) => error(PubSubError::InternalServerError),
    }
}

pub(super) async fn set_space_membership(
    iq: &Iq,
    state: &WebSocketState,
    service: &BareJid,
    user: &BareJid,
    session: Option<&Session>,
    node: &str,
    changes: Vec<(BareJid, Affiliation)>,
) -> Iq {
    let Iq::Set { payload, to, .. } = iq else {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    };
    if !payload.is("pubsub", NS_PUBSUB_OWNER)
        || to.as_ref().is_none_or(|to| to.as_str() != service.as_str())
    {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    }
    let mut operations = payload.children();
    let Some(affiliations) = operations.next() else {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    };
    if !affiliations.is("affiliations", NS_PUBSUB_OWNER)
        || operations.next().is_some()
        || affiliations.children().any(|item| {
            !item.is("affiliation", NS_PUBSUB_OWNER) || item.children().next().is_some()
        })
    {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    }
    if session.is_none_or(|session| session.user_jid != user.as_str()) {
        return build_pubsub_error(iq, PubSubError::Forbidden);
    }
    let (Ok(key), Ok(initiator)) = (
        ScopeKey::new(service.clone(), node),
        GrantSubject::new(user.clone()),
    ) else {
        return build_pubsub_error(iq, PubSubError::BadRequest);
    };
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
    let lifecycle = binding.lifecycle;
    let mut membership = Vec::with_capacity(changes.len());
    for (subject, affiliation) in changes {
        let Ok(subject) = GrantSubject::new(subject) else {
            return build_pubsub_error(iq, PubSubError::InvalidJid);
        };
        let affiliation = match affiliation {
            Affiliation::Member => SpaceMembership::Member,
            Affiliation::None => SpaceMembership::None,
            other => return unsupported_affiliation(iq, other),
        };
        membership.push(SpaceAffiliationChange {
            subject,
            affiliation,
        });
    }
    let Ok(Some(scope)) = store.scope(lifecycle).await else {
        return build_pubsub_error(iq, PubSubError::InternalServerError);
    };
    if scope.authority_incarnation != binding.incarnation {
        return error(
            iq,
            service_unavailable_iq_error("The Space is unavailable."),
        );
    }
    let authority = HostGrantAuthority::new(
        store.clone(),
        state.deps.app_state.permission_actor.clone(),
        policy,
    );
    let command = SetSpaceAffiliations {
        operation: GrantOperationId::generate(),
        lifecycle,
        expected_generation: scope.generation,
        initiator,
        changes: membership,
    };
    let mut recorded = authority.set_space_affiliations(&command).await;
    if matches!(recorded, Err(HostGrantError::CommitOutcomeUnknown)) {
        // An exact retry recovers the result without applying the batch twice.
        recorded = authority.set_space_affiliations(&command).await;
    }
    let receipt = match recorded {
        Ok(receipt) => receipt,
        Err(HostGrantError::InitiatorNotAuthorized | HostGrantError::Issuance(_)) => {
            return build_pubsub_error(iq, PubSubError::Forbidden);
        }
        Err(HostGrantError::InvalidSpaceMembershipBatch) => {
            return build_pubsub_error(iq, PubSubError::BadRequest);
        }
        Err(HostGrantError::StaleGeneration { .. } | HostGrantError::GrantLimit) => {
            return error(
                iq,
                resource_constraint_iq_error("Space membership could not be changed; retry."),
            );
        }
        Err(HostGrantError::UnknownScope | HostGrantError::SpaceNotBindable) => {
            return error(
                iq,
                service_unavailable_iq_error("The Space is unavailable."),
            );
        }
        Err(other) => {
            tracing::warn!(
                kind = other.kind(),
                "Space membership outcome could not be verified"
            );
            return build_pubsub_error(iq, PubSubError::InternalServerError);
        }
    };
    let record = receipt.record();
    if let Some(fence) = gate.fence() {
        fence.wake();
    }
    let reconcile = async {
        if gate.attachment().is_none() {
            return false;
        }
        if !matches!(
            authority.reconcile(lifecycle).await,
            Ok(Reconciliation::Activated(_)
                | Reconciliation::AlreadyActive(_)
                | Reconciliation::Superseded)
        ) {
            return false;
        }
        matches!(store.scope(lifecycle).await, Ok(Some(scope))
            if scope.authority_incarnation == record.authority_incarnation
                && scope.admission == AdmissionState::Active
                && scope.generation == record.generation
                && scope.projected_generation == Some(record.generation))
    };
    let acknowledged =
        store.await_revocation_fence(lifecycle, record.generation, FENCE_ACKNOWLEDGEMENT_DEADLINE);
    let (activated, acknowledged) = tokio::join!(
        tokio::time::timeout(RECONCILE_DEADLINE, reconcile),
        acknowledged,
    );
    if matches!(activated, Ok(true)) && acknowledged.is_ok_and(|reading| reading.acknowledged()) {
        build_pubsub_success(iq)
    } else {
        error(
            iq,
            resource_constraint_iq_error(
                "The membership change was recorded and is awaiting confirmation; retry.",
            ),
        )
    }
}

fn unsupported_affiliation(iq: &Iq, affiliation: Affiliation) -> Iq {
    let feature = match affiliation {
        Affiliation::Outcast => "outcast-affiliation",
        Affiliation::Publisher => "publisher-affiliation",
        Affiliation::PublishOnly => "publish-only-affiliation",
        _ => "modify-affiliations",
    };
    let mut stanza_error = feature_not_implemented_iq_error(
        "This Space supports member and none affiliation changes.",
    );
    stanza_error.other = Some(
        Element::builder("unsupported", NS_PUBSUB_ERRORS)
            .attr(minidom::rxml::xml_ncname!("feature").to_owned(), feature)
            .build(),
    );
    error(iq, stanza_error)
}

fn error(iq: &Iq, stanza_error: xmpp_parsers::stanza_error::StanzaError) -> Iq {
    Iq::Error {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        payload: None,
        error: stanza_error,
    }
}
