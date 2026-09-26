//! Saved extension output is admitted as a room-authored XMPP message.
use super::effects::{EffectSink, PlanSink};
use super::*;
use crate::{
    ingress::{IngressPrincipal, IngressStreamIdentity, IngressSubmission},
    ingress_uow::RoomPublication,
};
use minidom::rxml::{xml_ncname, Namespace};
use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget, TransportGeneration};
use waddle_xmpp_core::xep0359::{add_origin_id, StanzaId};

pub(crate) async fn plan_room_result(
    deps: &Deps<'_>,
    publication: RoomPublication,
) -> Result<IngressSubmission, crate::room_observation::ObservationRuntimeError> {
    use crate::room_observation::ObservationRuntimeError;
    let room = publication.source.room.clone();
    let sender = room
        .clone()
        .with_resource_str("__extensions__")
        .map_err(|_| ObservationRuntimeError::InvalidResult)?;
    let origin = publication
        .source
        .origin_id
        .as_ref()
        .ok_or(ObservationRuntimeError::InvalidResult)?;
    let mut payload = publication.payload.to_minidom();
    // These reserved attributes are always assigned by the host. A guest
    // cannot redirect a result to another room, source, or revision.
    payload.set_attr(
        Namespace::NONE,
        xml_ncname!("target-stanza-id").to_owned(),
        publication.source.stanza_id.as_str(),
    );
    payload.set_attr(
        Namespace::NONE,
        xml_ncname!("target-stanza-by").to_owned(),
        room.to_string(),
    );
    payload.set_attr(
        Namespace::NONE,
        xml_ncname!("source-revision-id").to_owned(),
        publication.source.revision_stanza_id.as_str(),
    );
    let fastening = Element::builder(
        "apply-to",
        waddle_xmpp::xep::xep_waddle_call_thread::NS_FASTEN,
    )
    .attr(xml_ncname!("id").to_owned(), origin.as_str())
    .append(payload)
    .build();
    let mut message = Message::new(Some(room.clone().into()));
    message.type_ = XmppMessageType::Groupchat;
    message.from = Some(room.clone().into());
    message.id = Some(xmpp_parsers::message::Id(publication.id.to_string()));
    add_origin_id(&mut message, &publication.id.to_string());
    message.payloads.push(fastening);
    message
        .payloads
        .push(waddle_xmpp::xep::xep0334::build_hint_element(
            waddle_xmpp::xep::xep0334::Hint::Store,
        ));
    let digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: NormalizedTarget::Bare(room.clone()),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .map_err(|_| ObservationRuntimeError::InvalidResult)?;
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    sink.observe_sender(&sender);
    sink.observe_message(&message);
    let mut planned = super::message_plan::build_plan_deps(deps, &sink)
        .with_ingress_effect_capture(Some(capture.clone()));
    planned.host_sender = Some(HostOwnedResources::Sender(sender.clone()));
    #[cfg(feature = "clustering")]
    {
        planned.ordered_relay_origin = Some(OrderedRelayRouteOrigin::room(&room));
    }
    super::room_system_message::broadcast_room_system_message_with_identity(
        &planned,
        room.clone(),
        Box::new(message.clone()),
        0,
        Some(StanzaId::new(
            publication.id.to_string(),
            room.clone().into(),
        )),
    )
    .await;
    let plan = super::message_plan::finish_plan(&sink, &capture, message, Some(sender.clone()));
    if !plan.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::SystemMessageArchive { archive, .. } if archive == &room)) {
        return Err(ObservationRuntimeError::RoomUnavailable);
    }
    Ok(IngressSubmission {
        identity: IngressStreamIdentity::RoomResult {
            id: publication.id,
            room: room.clone(),
        },
        principal: IngressPrincipal::RoomResult(Box::new(publication)),
        sender,
        target: NormalizedTarget::Bare(room),
        plan,
        digest_input,
        connection_generation: TransportGeneration::Host,
    })
}
