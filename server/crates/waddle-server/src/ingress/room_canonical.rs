//! Frozen room payloads and their provenance at the replay boundary.
use crate::ingress_substrate::MessageEnvelope;
use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};
use xmpp_parsers::message::{Message, MessageType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(super) enum CanonicalSourceError {
    #[error("missing_canonical_provenance")]
    MissingCanonicalProvenance,
    #[error("missing_payload")]
    MissingPayload,
}

pub(super) fn has_groupchat_provenance(
    message: &Message,
    room: &BareJid,
    identity: &EffectMessageIdentity,
) -> bool {
    message.type_ == MessageType::Groupchat
        && message
            .from
            .as_ref()
            .is_some_and(|from| from.is_full() && from.to_bare() == *room)
        && matches!(identity, EffectMessageIdentity::StanzaId(id)
            if id.by == *room && waddle_xmpp::xep::extract_stanza_ids(message).contains(id))
        && waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(message).is_some()
}

pub(super) fn source<'a>(
    envelope: &'a MessageEnvelope,
    intent: &'a IngressEffectIntent,
) -> Result<&'a Message, CanonicalSourceError> {
    match intent {
        IngressEffectIntent::RouteMucGroupchat {
            room,
            route_identity,
            ..
        } if has_groupchat_provenance(envelope.message(), room, route_identity) => {
            Ok(envelope.message())
        }
        IngressEffectIntent::RouteMucSystemBroadcast { system_message, .. } => system_message
            .as_ref()
            .map(|payload| payload.message())
            .ok_or(CanonicalSourceError::MissingPayload),
        _ => Err(CanonicalSourceError::MissingCanonicalProvenance),
    }
}

/// Personalize only the destination; the frozen source owns sender and content.
pub(super) fn occupant_copy_message(
    source: &Message,
    occupant: &FullJid,
    intents: &[IngressEffectIntent],
) -> Message {
    let mut message = source.clone();
    message.to = Some(occupant.clone().into());
    let stamps = waddle_xmpp::xep::extract_stanza_ids(source);
    for intent in intents {
        let (IngressEffectIntent::ArchiveAuthoritative { stanza_id, .. }
        | IngressEffectIntent::SystemMessageArchive { stanza_id, .. }) = intent
        else {
            continue;
        };
        // Several system broadcasts can share one row. Never borrow the
        // stanza-id of another payload merely because its authority is the room.
        if stamps.contains(stanza_id) {
            waddle_xmpp_core::xep0359::add_stanza_id(&mut message, stanza_id);
        }
    }
    message
}
