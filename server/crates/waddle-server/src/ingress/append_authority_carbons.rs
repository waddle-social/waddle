//! A carbon wrapper cannot borrow another message's durable append identity.
use super::AppendAuthorityRejection;
use crate::{ingress::identity::IngressAppendObligationRef, ingress_uow::CarbonReceiptRepository};
use jid::{BareJid, FullJid};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, IngressEffectKind},
    protocol::CarbonKind,
    Stanza,
};
use xmpp_parsers::{
    carbons::{Received, Sent},
    message::Message,
};

pub(super) fn is_carbon_kind(tag: i32) -> bool {
    [IngressEffectKind::Carbons, IngressEffectKind::RelayCarbons]
        .iter()
        .any(|kind| kind.storage_tag() == tag)
}

pub(super) struct CarbonEnvelope {
    owner: BareJid,
    resource: FullJid,
    kind: CarbonKind,
    pub(super) inner: Message,
}

pub(super) fn parse(message: &Message) -> Result<CarbonEnvelope, AppendAuthorityRejection> {
    let invalid = || AppendAuthorityRejection::CarbonObligationMismatch;
    let [payload] = message.payloads.as_slice() else {
        return Err(invalid());
    };
    let (kind, mut inner) = if let Ok(sent) = Sent::try_from(payload.clone()) {
        (CarbonKind::Sent, sent.forwarded.message)
    } else if let Ok(received) = Received::try_from(payload.clone()) {
        (CarbonKind::Received, received.forwarded.message)
    } else {
        return Err(invalid());
    };
    // The forwarded parser uses the typed thread field; canonical envelopes
    // retain parent-bearing threads as payloads at the ingress parse boundary.
    if let Some(parent) = inner
        .thread
        .as_ref()
        .and_then(|thread| thread.parent.clone())
    {
        waddle_xmpp_core::parser_utils::reattach_thread_parent(
            &mut inner,
            parent,
            waddle_xmpp_core::xep0201::CLIENT_STANZA_NS,
        );
    }
    let resource = message
        .to
        .clone()
        .ok_or_else(invalid)?
        .try_into_full()
        .map_err(|_| invalid())?;
    let owner = resource.to_bare();
    if message.from.as_ref() != Some(&owner.clone().into())
        || message.type_ != inner.type_
        || !message.bodies.is_empty()
        || !message.subjects.is_empty()
        || message.thread.is_some()
    {
        return Err(invalid());
    }
    Ok(CarbonEnvelope {
        owner,
        resource,
        kind,
        inner,
    })
}

pub(super) async fn authorize(
    db: &crate::db::Database,
    stanza: &Stanza,
    obligation: &IngressAppendObligationRef,
) -> Result<(), AppendAuthorityRejection> {
    let Stanza::Message(message) = stanza else {
        return Err(AppendAuthorityRejection::NotMessage);
    };
    let carbon = parse(message)?;
    let (envelope, intents) = CarbonReceiptRepository::load_authority(db, obligation.message_key)
        .await
        .map_err(|_| AppendAuthorityRejection::CanonicalReadFailed)?;
    let sender = envelope
        .message()
        .from
        .as_ref()
        .map(jid::Jid::to_bare)
        .ok_or(AppendAuthorityRejection::CanonicalSenderMissing)?;
    if sender != obligation.sender_bare {
        return Err(AppendAuthorityRejection::CanonicalSenderMismatch);
    }
    let authorized = intents.iter().any(|intent| {
        if super::super::durable::receipt_key(intent).ok().as_ref() != Some(&obligation.receipt) {
            return false;
        }
        match intent {
            IngressEffectIntent::Carbons {
                excluded_source,
                carbon_recipients,
                kind,
            } => {
                excluded_source.to_bare() == carbon.owner
                    && *kind == carbon.kind
                    && carbon_recipients.contains(&carbon.resource)
                    && excluded_source != &carbon.resource
            }
            IngressEffectIntent::RelayCarbons {
                owner,
                exclude,
                kind,
            } => {
                owner == &carbon.owner
                    && *kind == carbon.kind
                    && !exclude.contains(&carbon.resource)
            }
            _ => false,
        }
    });
    if !authorized {
        return Err(AppendAuthorityRejection::CarbonObligationMismatch);
    }
    let mut expected = envelope.message().clone();
    let mut archived_owner = false;
    for intent in &intents {
        if let IngressEffectIntent::ArchiveAuthoritative {
            archive, stanza_id, ..
        } = intent
        {
            if archive == &carbon.owner {
                archived_owner = true;
                waddle_xmpp_core::xep0359::add_stanza_id(&mut expected, stanza_id);
            }
        }
    }
    let matches = match carbon.kind {
        CarbonKind::Sent => super::same_message_content(&expected, &carbon.inner),
        CarbonKind::Received => {
            super::recipient_copy_matches(&expected, &carbon.inner, &carbon.owner, archived_owner)
        }
    };
    if !matches {
        return Err(AppendAuthorityRejection::CarbonObligationMismatch);
    }
    Ok(())
}
