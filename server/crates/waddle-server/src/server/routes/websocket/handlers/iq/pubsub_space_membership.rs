//! The served owner's XEP-0060 membership changes for a bound Space.

use std::time::Duration;

use jid::{BareJid, FullJid};
use minidom::Element;
use waddle_xmpp::pubsub::{
    parse_pubsub_iq, Affiliation, PubSubRequest, NS_PUBSUB_ERRORS, NS_PUBSUB_OWNER,
};
use xmpp_parsers::iq::Iq;

use crate::permissions::host_grant::{
    AdmissionState, AuthorityIncarnation, GrantGeneration, GrantOperationId, GrantSubject,
    HostGrantAuthority, HostGrantError, Reconciliation, ScopeKey, ScopeLifecycleId,
    SetSpaceAffiliations, FENCE_ACKNOWLEDGEMENT_DEADLINE,
};

use super::errors::{
    feature_not_implemented_iq_error, resource_constraint_iq_error, service_unavailable_iq_error,
};
use super::{
    build_pubsub_error, build_pubsub_success, iq_to_xml, PubSubError, Session, WebSocketState,
};
use crate::server::routes::websocket::frame::ResponseFrame;

const RECONCILE_DEADLINE: Duration = Duration::from_secs(5);

pub(super) async fn list_space_membership(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&jid::FullJid>,
    session: Option<&Session>,
) -> Option<ResponseFrame> {
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

pub(super) async fn set_space_membership_reply(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&FullJid>,
    session: Option<&Session>,
) -> Option<ResponseFrame> {
    use waddle_xmpp::pubsub::build_pubsub_affiliations_result;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let Iq::Set { payload, to, .. } = iq else {
        return None;
    };
    let service = state.deps.service_domains.spaces.as_str();
    if !payload.is("pubsub", NS_PUBSUB_OWNER) || to.as_ref()?.as_str() != service {
        return None;
    }
    let PubSubRequest::AffiliationsSet { node, changes } = parse_pubsub_iq(iq).ok()? else {
        return None;
    };
    let ordinary = |reply| Some(ResponseFrame::from(iq_to_xml(reply)));
    let pubsub_error = |condition| ordinary(build_pubsub_error(iq, condition));
    let gate = &state.deps.app_state.host_grants;
    match gate.space_enrolled(&node).await {
        Ok(false) => return None,
        Ok(true) => {}
        Err(_) => return pubsub_error(PubSubError::InternalServerError),
    }
    let Some(requester) = requester else {
        return pubsub_error(PubSubError::Forbidden);
    };
    let bare = requester.to_bare();
    if session.is_none_or(|session| session.user_jid != bare.as_str())
        || iq
            .from()
            .is_some_and(|from| from.as_str() != requester.as_str())
    {
        return pubsub_error(PubSubError::Forbidden);
    }
    if !valid_membership_delta(payload, &node, &changes) {
        return pubsub_error(PubSubError::BadRequest);
    }
    let (Ok(service), Ok(initiator)) = (service.parse::<BareJid>(), GrantSubject::new(bare)) else {
        return pubsub_error(PubSubError::BadRequest);
    };
    let Ok(key) = ScopeKey::new(service, &node) else {
        return pubsub_error(PubSubError::BadRequest);
    };
    let settled = match set_space_membership(iq, state, &key, initiator, &changes).await {
        Ok(settled) => settled,
        Err(reply) => return ordinary(*reply),
    };
    if settled.refused.is_empty() {
        return ordinary(build_pubsub_success(iq));
    }
    let Some(grant) = gate
        .local_space_affiliation_refusals_reply(
            &key,
            requester,
            iq,
            settled.lifecycle,
            settled.incarnation,
            settled.generation,
        )
        .await
    else {
        return ordinary(membership_mutation_error(iq, HostGrantError::Busy));
    };
    let Iq::Result {
        payload: Some(payload),
        ..
    } = build_pubsub_affiliations_result(iq, &node, &settled.refused)
    else {
        return pubsub_error(PubSubError::InternalServerError);
    };
    let reply = Element::builder("iq", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            key.service().as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            requester.as_str(),
        )
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq.id())
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error")
        .append(payload)
        .append(Element::from(StanzaError::new(
            ErrorType::Modify,
            DefinedCondition::NotAcceptable,
            "en",
            "",
        )))
        .build();
    let wire = crate::server::routes::websocket::transport_xml::element_to_xml(reply);
    match ResponseFrame::protected_space_frame(wire, grant) {
        Ok(frame) => Some(frame),
        Err(_) => pubsub_error(PubSubError::InternalServerError),
    }
}

fn valid_membership_delta(
    payload: &Element,
    node: &str,
    changes: &[(BareJid, Affiliation)],
) -> bool {
    let Some(list) = payload.get_child("affiliations", NS_PUBSUB_OWNER) else {
        return false;
    };
    payload.attrs().is_empty()
        && payload.text().trim().is_empty()
        && payload.children().count() == 1
        && list.attrs().len() == 1
        && list.attr("node") == Some(node)
        && list.text().trim().is_empty()
        && list.children().count() == changes.len()
        && list.children().zip(changes).all(|(entry, (jid, _))| {
            entry.is("affiliation", NS_PUBSUB_OWNER)
                && entry.attrs().len() == 2
                && entry.attr("jid") == Some(jid.as_str())
                && entry.attr("affiliation").is_some()
                && entry.children().next().is_none()
                && entry.text().trim().is_empty()
        })
}

struct SettledAffiliations {
    lifecycle: ScopeLifecycleId,
    incarnation: AuthorityIncarnation,
    generation: GrantGeneration,
    refused: Vec<(BareJid, Affiliation)>,
}

async fn set_space_membership(
    iq: &Iq,
    state: &WebSocketState,
    key: &ScopeKey,
    initiator: GrantSubject,
    changes: &[(BareJid, Affiliation)],
) -> Result<SettledAffiliations, Box<Iq>> {
    let pubsub_error = |condition| Box::new(build_pubsub_error(iq, condition));
    let gate = &state.deps.app_state.host_grants;
    let (Some(store), Some(attachment)) = (gate.store(), gate.attachment()) else {
        return Err(pubsub_error(PubSubError::InternalServerError));
    };
    let Some(policy) = state
        .deps
        .service_domains
        .host_grant_issuance_policy(attachment.principals())
    else {
        return Err(pubsub_error(PubSubError::InternalServerError));
    };
    let plan = match store
        .plan_space_affiliation_changes(key, &initiator, attachment.binding(), &policy, changes)
        .await
    {
        Ok(Some(plan)) => plan,
        Ok(None) => return Err(pubsub_error(PubSubError::Forbidden)),
        Err(error) => return Err(Box::new(membership_mutation_error(iq, error))),
    };
    if let [(_, affiliation)] = changes {
        if !matches!(affiliation, Affiliation::Member | Affiliation::None) {
            return Err(Box::new(unsupported_affiliation(iq, *affiliation)));
        }
        if plan.changes.is_empty() && !plan.refused.is_empty() {
            return Err(pubsub_error(PubSubError::Forbidden));
        }
    }
    let lifecycle = plan.lifecycle;
    let mut settled = SettledAffiliations {
        lifecycle,
        incarnation: attachment.binding().incarnation,
        generation: plan.generation,
        refused: plan.refused,
    };
    if plan.changes.is_empty() {
        return Ok(settled);
    }
    let authority = HostGrantAuthority::new(
        store.clone(),
        state.deps.app_state.permission_actor.clone(),
        policy,
    );
    let command = SetSpaceAffiliations {
        operation: GrantOperationId::generate(),
        lifecycle,
        expected_generation: plan.generation,
        initiator,
        changes: plan.changes,
    };
    let mut recorded = authority.set_space_affiliations(&command).await;
    if matches!(recorded, Err(HostGrantError::CommitOutcomeUnknown)) {
        // Retry the same operation and its already classified subset.
        recorded = authority.set_space_affiliations(&command).await;
    }
    let receipt = recorded.map_err(|error| Box::new(membership_mutation_error(iq, error)))?;
    let record = receipt.record();
    if record.authority_incarnation != settled.incarnation {
        return Err(Box::new(error(
            iq,
            service_unavailable_iq_error("The Space is unavailable."),
        )));
    }
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
        settled.generation = record.generation;
        Ok(settled)
    } else {
        Err(Box::new(error(
            iq,
            resource_constraint_iq_error(
                "The membership change was recorded and is awaiting confirmation; retry.",
            ),
        )))
    }
}

fn membership_mutation_error(iq: &Iq, failure: HostGrantError) -> Iq {
    match failure {
        HostGrantError::InitiatorNotAuthorized | HostGrantError::Issuance(_) => {
            build_pubsub_error(iq, PubSubError::Forbidden)
        }
        HostGrantError::InvalidSpaceMembershipBatch => {
            build_pubsub_error(iq, PubSubError::BadRequest)
        }
        HostGrantError::StaleGeneration { .. }
        | HostGrantError::GrantLimit
        | HostGrantError::Busy => error(
            iq,
            resource_constraint_iq_error("Space membership could not be changed; retry."),
        ),
        HostGrantError::UnknownScope | HostGrantError::SpaceNotBindable => error(
            iq,
            service_unavailable_iq_error("The Space is unavailable."),
        ),
        other => {
            tracing::warn!(
                kind = other.kind(),
                "Space membership outcome could not be verified"
            );
            build_pubsub_error(iq, PubSubError::InternalServerError)
        }
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
