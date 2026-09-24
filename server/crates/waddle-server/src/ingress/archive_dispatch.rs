//! Tie dispatch to the archive position committed with its frozen obligations.

use std::time::Duration;

use jid::{BareJid, FullJid};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent, MessageKey, PendingDeliveryMutation},
    mam::{ArchiveOrdinal, ArchivedMessage},
};

use crate::{
    ingress_uow::{
        ArchiveDispatchObligation, ArchiveDispatchRepository, DispatchReadiness, DispatchTarget,
        IngressUnitOfWork, IngressUowError, IngressUowTransaction,
    },
    server::routes::interpret::effects::{delivery::ExternalDeliveryEffect, ExternalEffect},
};

use super::{EffectReceiptKey, IngressDecision};

impl super::IngressAuthority {
    /// Recheck at the socket after network or actor delay. The caller retains
    /// the exact connection owner across this read and the queue acceptance.
    pub(crate) async fn socket_delivery_readiness(
        &self,
        context: &crate::server::routes::interpret::SmIngressAppendContext,
        resource: &FullJid,
        stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
    ) -> Result<DispatchReadiness, IngressUowError> {
        resource_ready(
            &self.uow,
            context.message_key,
            &context.receipt,
            Some(resource),
            stream,
        )
        .await
    }
}

pub(super) async fn record(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    archive: &BareJid,
    message: &ArchivedMessage,
    ordinal: ArchiveOrdinal,
    intents: &[IngressEffectIntent],
) -> Result<(), IngressUowError> {
    let mut obligations = Vec::new();
    for intent in intents {
        let targets = targets(intent, archive, message);
        if targets.is_empty() {
            continue;
        }
        let receipt = super::receipt_key(intent)?;
        obligations.extend(targets.into_iter().map(|target| ArchiveDispatchObligation {
            receipt: receipt.clone(),
            target,
        }));
    }
    ArchiveDispatchRepository::record(tx, key, archive, ordinal, &obligations).await
}

fn targets(
    intent: &IngressEffectIntent,
    archive: &BareJid,
    message: &ArchivedMessage,
) -> Vec<DispatchTarget> {
    match intent {
        IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            route_identity,
        } if (recipient == archive
            || matches!(route_identity,
            EffectMessageIdentity::StanzaId(id) if id.by == *archive))
            && identity_matches(route_identity, archive, message) =>
        {
            if fanout.is_empty() {
                vec![DispatchTarget::ArchiveWide]
            } else {
                resources(fanout)
            }
        }
        IngressEffectIntent::Carbons {
            carbon_recipients,
            excluded_source,
            ..
        } if excluded_source.to_bare() == *archive => resources(carbon_recipients),
        IngressEffectIntent::RelayCarbons { owner, .. } if owner == archive => {
            vec![DispatchTarget::ArchiveWide]
        }
        IngressEffectIntent::RouteMucGroupchat {
            room,
            occupants,
            reflection,
            route_identity,
            ..
        } if room == archive && identity_matches(route_identity, archive, message) => {
            // The original sender's reflection is written by its own connection.
            // The recorded route receipt owns the remaining occupant copies.
            occupants
                .iter()
                .filter(|target| *target != reflection)
                .cloned()
                .map(DispatchTarget::Resource)
                .collect()
        }
        IngressEffectIntent::RouteMucSystemBroadcast {
            room,
            occupants,
            route_identity,
            ..
        } if room == archive && identity_matches(route_identity, archive, message) => {
            resources(occupants)
        }
        IngressEffectIntent::PendingDelivery {
            mutation:
                PendingDeliveryMutation::Archived {
                    recipient,
                    row_id,
                    archive_stanza_id,
                },
        } if recipient == archive
            && archive_stanza_id.id == message.id
            && archive_stanza_id.by == *archive =>
        {
            vec![DispatchTarget::Pending(row_id.clone())]
        }
        _ => Vec::new(),
    }
}

fn resources(targets: &[FullJid]) -> Vec<DispatchTarget> {
    targets
        .iter()
        .cloned()
        .map(DispatchTarget::Resource)
        .collect()
}

fn identity_matches(
    identity: &EffectMessageIdentity,
    archive: &BareJid,
    message: &ArchivedMessage,
) -> bool {
    match identity {
        EffectMessageIdentity::StanzaId(id) => id.id == message.id && id.by == *archive,
        EffectMessageIdentity::OriginId(id) => message.origin_id.as_ref() == Some(id),
        // A capture identity belongs to this canonical message and recipient.
        // Inbox refresh identities are explicitly a separate, best-effort family.
        EffectMessageIdentity::CaptureOrdinal(_) => true,
        EffectMessageIdentity::InboxPush(_) => false,
    }
}

/// Give in-flight predecessor receipts a brief chance to finish, without
/// retaining a transaction or executing predecessor work. Persistent barriers
/// still defer to maintenance; the enclosing execution deadline also applies.
pub(super) async fn resource_ready(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    receipt: &EffectReceiptKey,
    resource: Option<&FullJid>,
    stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
) -> Result<DispatchReadiness, IngressUowError> {
    let mut backoffs = [2, 4, 8, 16].into_iter();
    loop {
        let readiness = resource_ready_once(uow, key, receipt, resource, stream).await?;
        // An empty predecessor list denotes an independent pending-delivery
        // barrier, which can require client acknowledgement rather than an
        // in-flight canonical receipt. Do not delay that connection's loop.
        if matches!(&readiness, DispatchReadiness::Blocked(keys) if !keys.is_empty()) {
            if let Some(delay_ms) = backoffs.next() {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                continue;
            }
        }
        return Ok(readiness);
    }
}

async fn resource_ready_once(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    receipt: &EffectReceiptKey,
    resource: Option<&FullJid>,
    stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
) -> Result<DispatchReadiness, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    let readiness =
        ArchiveDispatchRepository::readiness(&mut tx, key, receipt, resource, stream).await?;
    tx.commit().await?;
    #[cfg(test)]
    if matches!(&readiness, DispatchReadiness::Blocked(_)) {
        super::execute::test_hooks::after_blocked_dispatch(key).await;
    }
    Ok(readiness)
}

pub(super) async fn positions(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    receipt: &EffectReceiptKey,
) -> Result<Vec<waddle_xmpp::stream_management::ArchiveDispatchPosition>, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    let positions = ArchiveDispatchRepository::positions(&mut tx, key, receipt).await?;
    tx.commit().await?;
    Ok(positions)
}

pub(super) async fn effect_ready(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalEffect,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> Result<
    (
        DispatchReadiness,
        Option<waddle_xmpp::pending_delivery::SmSessionId>,
    ),
    IngressUowError,
> {
    let Some(key) = decision.message_key else {
        return Ok((DispatchReadiness::Ready, None));
    };
    let target = match effect {
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. }) => {
            Some(target)
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::Carbons { recipient, .. }) => {
            Some(recipient)
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons { .. }) => None,
        ExternalEffect::Frame(stanza) => match stanza.as_ref() {
            waddle_xmpp::Stanza::Message(message) => {
                message.to.as_ref().and_then(|jid| jid.try_as_full().ok())
            }
            _ => return Ok((DispatchReadiness::Ready, None)),
        },
        // Progress-owned original copies are gated separately for each resource,
        // so an unavailable sibling cannot block an already-ready local copy.
        _ => return Ok((DispatchReadiness::Ready, None)),
    };
    let stream = target.and_then(|resource| deps.connection_registry.local_sm_stream(resource));
    let mut readiness = DispatchReadiness::Completed;
    let mut checked = false;
    for receipt in &decision.external_receipts[index] {
        // Error replies and transient reflections retain their replay semantics.
        if matches!(effect, ExternalEffect::Frame(_))
            && positions(uow, key, receipt).await?.is_empty()
        {
            continue;
        }
        checked = true;
        match resource_ready(uow, key, receipt, target, stream.as_ref()).await? {
            blocked @ DispatchReadiness::Blocked(_) => return Ok((blocked, None)),
            DispatchReadiness::Ready => readiness = DispatchReadiness::Ready,
            DispatchReadiness::Completed => {}
        }
    }
    if !checked {
        readiness = DispatchReadiness::Ready;
    }
    Ok((readiness, stream))
}
