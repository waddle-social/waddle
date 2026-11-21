//! The served owner's XEP-0060 membership changes for a bound Space.

use std::time::Duration;

use jid::{BareJid, FullJid};
use minidom::Element;
use waddle_xmpp::pubsub::{
    parse_pubsub_iq, Affiliation, PubSubRequest, NS_PUBSUB_ERRORS, NS_PUBSUB_OWNER,
};
use xmpp_parsers::iq::Iq;

use crate::permissions::host_grant::{
    AdmissionState, AuthorityIncarnation, ConfigureSpace, DeleteSpace, GrantGeneration,
    GrantOperationId, GrantSubject, HostGrantAuthority, HostGrantError, HostGrantGate,
    OperationRecord, Reconciliation, ScopeKey, ScopeLifecycleId, SetSpaceAffiliations,
    FENCE_ACKNOWLEDGEMENT_DEADLINE,
};

use super::errors::{
    feature_not_implemented_iq_error, resource_constraint_iq_error, service_unavailable_iq_error,
};
use super::{
    build_pubsub_error, build_pubsub_success, iq_to_xml, PubSubError, Session, WebSocketState,
};
use crate::server::routes::websocket::frame::ResponseFrame;

const RECONCILE_DEADLINE: Duration = Duration::from_secs(5);

pub(super) async fn read_space_owner_state(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&jid::FullJid>,
    session: Option<&Session>,
) -> Option<ResponseFrame> {
    use waddle_xmpp::pubsub::{
        build_pubsub_affiliations_result, build_pubsub_configure_form_result,
    };

    let Iq::Get { payload, to, .. } = iq else {
        return None;
    };
    let service = state.deps.service_domains.spaces.as_str();
    if !payload.is("pubsub", NS_PUBSUB_OWNER) || to.as_ref()?.as_str() != service {
        return None;
    }
    let operation = payload.children().find(|child| {
        child.ns() == NS_PUBSUB_OWNER && matches!(child.name(), "affiliations" | "configure")
    })?;
    let node = operation.attr("node")?;
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
    let (grant, mut reply) = if operation.name() == "configure" {
        let Some(grant) = gate.space_reply_local(&key, requester, iq).await else {
            return error(PubSubError::Forbidden);
        };
        let config = match state
            .deps
            .protocol
            .pubsub_storage
            .get_node(key.service(), node)
            .await
        {
            Ok(Some(record)) => record.config,
            Ok(None) => return error(PubSubError::NodeNotFound),
            Err(_) => return error(PubSubError::InternalServerError),
        };
        (grant, build_pubsub_configure_form_result(iq, node, &config))
    } else {
        let Some((grant, listing)) = gate
            .local_space_affiliations_reply(&key, requester, iq)
            .await
        else {
            return error(PubSubError::Forbidden);
        };
        (
            grant,
            build_pubsub_affiliations_result(iq, node, &listing.rows),
        )
    };
    if let Iq::Result { to, .. } = &mut reply {
        *to = Some(requester.clone().into());
    }
    match ResponseFrame::protected_space_frame(super::iq_to_xml(reply), grant) {
        Ok(frame) => Some(frame),
        Err(_) => error(PubSubError::InternalServerError),
    }
}

struct EnrolledSpaceOwner {
    key: ScopeKey,
    initiator: GrantSubject,
    lifecycle: ScopeLifecycleId,
    incarnation: AuthorityIncarnation,
    generation: GrantGeneration,
    authority: HostGrantAuthority,
}

async fn enrolled_space_owner(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&FullJid>,
    session: Option<&Session>,
    node: &str,
) -> Result<Option<EnrolledSpaceOwner>, Box<Iq>> {
    let failure = |condition| Box::new(build_pubsub_error(iq, condition));
    let service = state.deps.service_domains.spaces.as_str();
    let gate = &state.deps.app_state.host_grants;
    match gate.space_enrolled(node).await {
        Ok(false) => return Ok(None),
        Ok(true) => {}
        Err(_) => return Err(failure(PubSubError::InternalServerError)),
    }
    let Some(requester) = requester else {
        return Err(failure(PubSubError::Forbidden));
    };
    let bare = requester.to_bare();
    if session.is_none_or(|session| session.user_jid != bare.as_str())
        || iq
            .from()
            .is_some_and(|from| from.as_str() != requester.as_str())
    {
        return Err(failure(PubSubError::Forbidden));
    }
    let (Ok(service), Ok(initiator)) = (service.parse(), GrantSubject::new(bare)) else {
        return Err(failure(PubSubError::BadRequest));
    };
    let Ok(key) = ScopeKey::new(service, node) else {
        return Err(failure(PubSubError::BadRequest));
    };
    let Some(decision) = gate.space_read_local(&key, &initiator).await else {
        return Err(failure(PubSubError::Forbidden));
    };
    if decision.admit(|affiliation| affiliation == Affiliation::Owner) != Some(true) {
        return Err(failure(PubSubError::Forbidden));
    }
    let (Some(store), Some(attachment)) = (gate.store(), gate.attachment()) else {
        return Err(failure(PubSubError::InternalServerError));
    };
    let Some(policy) = state
        .deps
        .service_domains
        .host_grant_issuance_policy(attachment.principals())
    else {
        return Err(failure(PubSubError::InternalServerError));
    };
    let binding = decision.subscription_grant();
    let scope = match store.scope(binding.lifecycle).await {
        Ok(Some(scope)) if scope.authority_incarnation == binding.incarnation => scope,
        Ok(_) => return Err(failure(PubSubError::NodeNotFound)),
        Err(error) => return Err(Box::new(membership_mutation_error(iq, error))),
    };
    Ok(Some(EnrolledSpaceOwner {
        key,
        initiator,
        lifecycle: binding.lifecycle,
        incarnation: binding.incarnation,
        generation: scope.generation,
        authority: HostGrantAuthority::new(
            store.clone(),
            state.deps.app_state.permission_actor.clone(),
            policy,
        ),
    }))
}

pub(super) async fn configure_enrolled_space(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&FullJid>,
    session: Option<&Session>,
) -> Option<ResponseFrame> {
    let Iq::Set { payload, to, .. } = iq else {
        return None;
    };
    let service = state.deps.service_domains.spaces.as_str();
    if !payload.is("pubsub", NS_PUBSUB_OWNER) || to.as_ref()?.as_str() != service {
        return None;
    }
    let configure = payload.get_child("configure", NS_PUBSUB_OWNER)?;
    let node = configure.attr("node")?;
    let ordinary = |reply| Some(ResponseFrame::from(iq_to_xml(reply)));
    let failure = |condition| ordinary(build_pubsub_error(iq, condition));
    let owner = match enrolled_space_owner(iq, state, requester, session, node).await {
        Ok(Some(owner)) => owner,
        Ok(None) => return None,
        Err(error) => return ordinary(*error),
    };
    let current = match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(owner.key.service(), node)
        .await
    {
        Ok(Some(record)) => record.config,
        Ok(None) => return failure(PubSubError::NodeNotFound),
        Err(_) => return failure(PubSubError::InternalServerError),
    };
    let deliver_payloads = match configuration_payload_setting(payload, configure, &current) {
        Some(PayloadSetting::Change(value)) => value,
        Some(PayloadSetting::Cancel) => return ordinary(build_pubsub_success(iq)),
        None => {
            use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
            return ordinary(error(
                iq,
                StanzaError::new(
                    ErrorType::Modify,
                    DefinedCondition::NotAcceptable,
                    "en",
                    "The form contains an unsupported Space setting or an invalid value.",
                ),
            ));
        }
    };
    let command = ConfigureSpace {
        operation: GrantOperationId::generate(),
        key: owner.key,
        lifecycle: owner.lifecycle,
        expected_incarnation: owner.incarnation,
        expected_generation: owner.generation,
        initiator: owner.initiator,
        deliver_payloads,
    };
    let mut result = owner.authority.configure_space(&command).await;
    if matches!(result, Err(HostGrantError::CommitOutcomeUnknown)) {
        result = owner.authority.configure_space(&command).await;
    }
    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => return ordinary(membership_mutation_error(iq, error)),
    };
    if settle_owner_mutation(
        &state.deps.app_state.host_grants,
        &owner.authority,
        receipt.record(),
    )
    .await
    {
        ordinary(build_pubsub_success(iq))
    } else {
        ordinary(error(
            iq,
            resource_constraint_iq_error(
                "The configuration change was recorded and is awaiting confirmation.",
            ),
        ))
    }
}

pub(super) async fn delete_enrolled_space(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&FullJid>,
    session: Option<&Session>,
) -> Option<ResponseFrame> {
    let Iq::Set { payload, to, .. } = iq else {
        return None;
    };
    if !payload.is("pubsub", NS_PUBSUB_OWNER)
        || to.as_ref()?.as_str() != state.deps.service_domains.spaces.as_str()
    {
        return None;
    }
    let delete = payload.get_child("delete", NS_PUBSUB_OWNER)?;
    let node = delete.attr("node")?;
    let ordinary = |reply| Some(ResponseFrame::from(iq_to_xml(reply)));
    let owner = match enrolled_space_owner(iq, state, requester, session, node).await {
        Ok(Some(owner)) => owner,
        Ok(None) => return None,
        Err(error) => return ordinary(*error),
    };
    if !payload.attrs().is_empty()
        || payload.children().count() != 1
        || !payload.text().trim().is_empty()
        || delete.attrs().len() != 1
        || delete.children().next().is_some()
        || !delete.text().trim().is_empty()
    {
        return ordinary(build_pubsub_error(iq, PubSubError::BadRequest));
    }
    let command = DeleteSpace {
        operation: GrantOperationId::generate(),
        key: owner.key,
        lifecycle: owner.lifecycle,
        expected_incarnation: owner.incarnation,
        expected_generation: owner.generation,
        initiator: owner.initiator,
    };
    let mut result = owner.authority.delete_space(&command).await;
    if matches!(result, Err(HostGrantError::CommitOutcomeUnknown)) {
        result = owner.authority.delete_space(&command).await;
    }
    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => return ordinary(membership_mutation_error(iq, error)),
    };
    if settle_owner_mutation(
        &state.deps.app_state.host_grants,
        &owner.authority,
        receipt.record(),
    )
    .await
    {
        ordinary(build_pubsub_success(iq))
    } else {
        ordinary(error(
            iq,
            resource_constraint_iq_error(
                "The Space deletion was recorded and is awaiting confirmation.",
            ),
        ))
    }
}

enum PayloadSetting {
    Cancel,
    Change(bool),
}

fn configuration_payload_setting(
    payload: &Element,
    configure: &Element,
    current: &waddle_xmpp::pubsub::NodeConfig,
) -> Option<PayloadSetting> {
    const DATA_FORMS: &str = "jabber:x:data";
    let form = configure.get_child("x", DATA_FORMS)?;
    if !payload.attrs().is_empty()
        || !payload.text().trim().is_empty()
        || payload.children().count() != 1
        || configure.attrs().len() != 1
        || !configure.text().trim().is_empty()
        || configure.children().count() != 1
        || form.attrs().len() != 1
        || !form.text().trim().is_empty()
    {
        return None;
    }
    if form.attr("type") == Some("cancel") && form.children().next().is_none() {
        return Some(PayloadSetting::Cancel);
    }
    if form.attr("type") != Some("submit") || !(2..=10).contains(&form.children().count()) {
        return None;
    }
    let mut form_type = false;
    let mut deliver_payloads = None;
    let mut seen = std::collections::BTreeSet::new();
    for field in form.children() {
        let value = field.get_child("value", DATA_FORMS)?;
        if !field.is("field", DATA_FORMS)
            || field.children().count() != 1
            || !field.text().trim().is_empty()
            || !value.attrs().is_empty()
            || value.children().next().is_some()
        {
            return None;
        }
        let field_type = field.attr("type");
        if field.attrs().len() != 1 + usize::from(field_type.is_some()) {
            return None;
        }
        let name = field.attr("var")?;
        if !seen.insert(name) {
            return None;
        }
        match (name, field_type, value.text().as_str()) {
            (
                "FORM_TYPE",
                None | Some("hidden"),
                "http://jabber.org/protocol/pubsub#node_config",
            ) if !form_type => form_type = true,
            ("pubsub#deliver_payloads", None | Some("boolean"), "1" | "true")
                if deliver_payloads.is_none() =>
            {
                deliver_payloads = Some(true)
            }
            ("pubsub#deliver_payloads", None | Some("boolean"), "0" | "false")
                if deliver_payloads.is_none() =>
            {
                deliver_payloads = Some(false)
            }
            ("pubsub#access_model", None, value) if value == current.access_model.to_string() => {}
            ("pubsub#publish_model", None, value) if value == current.publish_model.to_string() => {
            }
            ("pubsub#type", None, value)
                if current.node_type.is_some_and(|kind| kind.as_str() == value) => {}
            ("pubsub#max_items", None, value)
                if value.parse::<u32>().ok() == Some(current.max_items) => {}
            ("pubsub#persist_items", None | Some("boolean"), value)
                if setting_matches(value, current.persist_items) => {}
            ("pubsub#notify_retract", None | Some("boolean"), value)
                if setting_matches(value, current.notify_retract) => {}
            ("pubsub#notify_delete", None | Some("boolean"), value)
                if setting_matches(value, current.notify_delete) => {}
            ("pubsub#send_last_published_item", None, value)
                if value == current.send_last_published_item.to_string() => {}
            _ => return None,
        }
    }
    deliver_payloads
        .filter(|_| form_type)
        .map(PayloadSetting::Change)
}

fn setting_matches(value: &str, enabled: bool) -> bool {
    matches!(
        (value, enabled),
        ("1" | "true", true) | ("0" | "false", false)
    )
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
    if settle_owner_mutation(gate, &authority, record).await {
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

async fn settle_owner_mutation(
    gate: &HostGrantGate,
    authority: &HostGrantAuthority,
    record: &OperationRecord,
) -> bool {
    let Some(store) = gate.store() else {
        return false;
    };
    if let Some(fence) = gate.fence() {
        fence.wake();
    }
    let lifecycle = record.lifecycle;
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
    matches!(activated, Ok(true)) && acknowledged.is_ok_and(|reading| reading.acknowledged())
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
