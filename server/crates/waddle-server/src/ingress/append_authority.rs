//! Receiver-side authority for an ingress obligation a peer node relayed.
//!
//! Shared by every place a relayed obligation is about to key an XEP-0198 replay
//! append: the ordered-relay receivers (#1778) and the registered-socket detach
//! drain (#1789). An archive-ordered relay must retain valid authority to enter
//! the destination queue. Already accepted transport frames keep their drain
//! behavior when optional deduplication authority cannot be recovered.

use std::time::Duration;

use waddle_xmpp::ingress::{IngressEffectKind, MessageKey};
use waddle_xmpp::telemetry::attributes::IngressAppendAuthorizationFailure;
use waddle_xmpp::Stanza;

#[path = "append_authority_carbons.rs"]
mod carbons;

#[cfg(test)]
#[path = "append_authority_carbon_tests.rs"]
mod carbon_tests;

#[cfg(test)]
#[path = "append_authority_room_tests.rs"]
mod room_tests;

const AUTHORIZATION_READ_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) enum AppendAuthorityRejection {
    IneligibleKind,
    NotMessage,
    /// Only the clustering receivers hold a validated sender claim to compare.
    #[cfg(feature = "clustering")]
    SenderClaimMismatch,
    StanzaSenderMismatch,
    ServicesUnavailable,
    CanonicalReadFailed,
    CanonicalReadTimedOut,
    CanonicalSenderMissing,
    CanonicalSenderMismatch,
    CarbonObligationMismatch,
    ArchivePositionMismatch,
}

impl AppendAuthorityRejection {
    /// Whether the identity was provably unusable, or merely undecidable here.
    ///
    /// The two degrade identically — delivery never depends on this check — but
    /// they mean different things operationally: `Unauthorized` is a statement
    /// about the peer, `Indeterminate` is a statement about this node's own
    /// ability to read canonical state, and only the latter silently widens the
    /// duplicate window while it persists.
    pub(crate) fn failure_class(&self) -> IngressAppendAuthorizationFailure {
        match self {
            Self::IneligibleKind
            | Self::NotMessage
            | Self::StanzaSenderMismatch
            | Self::CanonicalSenderMissing
            | Self::CanonicalSenderMismatch
            | Self::CarbonObligationMismatch => IngressAppendAuthorizationFailure::Unauthorized,
            Self::ArchivePositionMismatch => IngressAppendAuthorizationFailure::Unauthorized,
            #[cfg(feature = "clustering")]
            Self::SenderClaimMismatch => IngressAppendAuthorizationFailure::Unauthorized,
            Self::ServicesUnavailable | Self::CanonicalReadFailed | Self::CanonicalReadTimedOut => {
                IngressAppendAuthorizationFailure::Indeterminate
            }
        }
    }
}

/// Only recorded message routes and carbon copies allocate keyed SM appends.
pub(crate) fn receipt_kind_is_append_eligible(storage_tag: i32) -> bool {
    [
        IngressEffectKind::RouteDirect,
        IngressEffectKind::RouteMucGroupchat,
        IngressEffectKind::Carbons,
        IngressEffectKind::RelayCarbons,
    ]
    .into_iter()
    .any(|kind| kind.storage_tag() == storage_tag)
}

/// The stanza must be a message from the claimed sender, on a route that allocates
/// keyed appends. Needs no I/O, so it can run where the stanza is still typed.
pub(crate) fn check_stanza_binding(
    stanza: &Stanza,
    sender_bare: &jid::BareJid,
    receipt_kind_storage_tag: i32,
) -> Result<(), AppendAuthorityRejection> {
    if !receipt_kind_is_append_eligible(receipt_kind_storage_tag) {
        return Err(AppendAuthorityRejection::IneligibleKind);
    }
    if &stanza_sender(stanza, receipt_kind_storage_tag)? != sender_bare {
        return Err(AppendAuthorityRejection::StanzaSenderMismatch);
    }
    Ok(())
}

pub(crate) fn stanza_sender(
    stanza: &Stanza,
    receipt_kind_storage_tag: i32,
) -> Result<jid::BareJid, AppendAuthorityRejection> {
    let Stanza::Message(message) = stanza else {
        return Err(AppendAuthorityRejection::NotMessage);
    };
    let sender = if carbons::is_carbon_kind(receipt_kind_storage_tag) {
        carbons::parse(message)?.inner.from
    } else {
        message.from.clone()
    };
    sender
        .map(|jid| jid.to_bare())
        .ok_or(AppendAuthorityRejection::StanzaSenderMismatch)
}

pub(crate) fn check_resource_binding(
    stanza: &Stanza,
    receipt_kind_storage_tag: i32,
    resource: &jid::FullJid,
) -> Result<(), AppendAuthorityRejection> {
    if carbons::is_carbon_kind(receipt_kind_storage_tag) {
        let Stanza::Message(message) = stanza else {
            return Err(AppendAuthorityRejection::NotMessage);
        };
        if message.to.as_ref() != Some(&resource.clone().into()) {
            return Err(AppendAuthorityRejection::CarbonObligationMismatch);
        }
    }
    Ok(())
}

/// Carbon envelopes additionally prove the frozen receipt, target and inner
/// message; their outer sender is the carbon owner rather than the originator.
pub(crate) async fn check_canonical_obligation(
    db: &crate::db::Database,
    stanza: &Stanza,
    obligation: &super::identity::IngressAppendObligationRef,
) -> Result<(), AppendAuthorityRejection> {
    if obligation.receipt.kind.to_storage() == IngressEffectKind::RouteMucGroupchat.storage_tag()
        || (obligation.receipt.kind.to_storage() == IngressEffectKind::RouteDirect.storage_tag()
            && matches!(stanza, Stanza::Message(message) if message.type_ == xmpp_parsers::message::MessageType::Groupchat))
    {
        tokio::time::timeout(
            AUTHORIZATION_READ_TIMEOUT,
            authorize_room_route(db, stanza, obligation),
        )
        .await
        .map_err(|_| AppendAuthorityRejection::CanonicalReadTimedOut)??;
    } else if carbons::is_carbon_kind(obligation.receipt.kind.to_storage()) {
        tokio::time::timeout(
            AUTHORIZATION_READ_TIMEOUT,
            carbons::authorize(db, stanza, obligation),
        )
        .await
        .map_err(|_| AppendAuthorityRejection::CanonicalReadTimedOut)??;
    } else {
        check_canonical_sender(db, obligation.message_key, &obligation.sender_bare).await?;
    }
    let positions = tokio::time::timeout(
        AUTHORIZATION_READ_TIMEOUT,
        crate::ingress_uow::ArchiveDispatchRepository::positions_pooled(
            db,
            obligation.message_key,
            &obligation.receipt,
        ),
    )
    .await
    .map_err(|_| AppendAuthorityRejection::CanonicalReadTimedOut)?
    .map_err(|_| AppendAuthorityRejection::CanonicalReadFailed)?;
    if positions != obligation.archive_positions {
        return Err(AppendAuthorityRejection::ArchivePositionMismatch);
    }
    Ok(())
}

async fn authorize_room_route(
    db: &crate::db::Database,
    stanza: &Stanza,
    obligation: &super::identity::IngressAppendObligationRef,
) -> Result<(), AppendAuthorityRejection> {
    let Stanza::Message(message) = stanza else {
        return Err(AppendAuthorityRejection::NotMessage);
    };
    let (envelope, intents) =
        crate::ingress_uow::CarbonReceiptRepository::load_authority(db, obligation.message_key)
            .await
            .map_err(|_| AppendAuthorityRejection::CanonicalReadFailed)?;
    let target = message
        .to
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .ok_or(AppendAuthorityRejection::StanzaSenderMismatch)?;
    for intent in &intents {
        if super::receipt_key(intent).ok().as_ref() != Some(&obligation.receipt) {
            continue;
        }
        let (room, occupants, source_intent) = match intent {
            waddle_xmpp::ingress::IngressEffectIntent::RouteMucGroupchat {
                room,
                occupants,
                ..
            }
            | waddle_xmpp::ingress::IngressEffectIntent::RouteMucSystemBroadcast {
                room,
                occupants,
                ..
            } => (room, occupants, intent),
            waddle_xmpp::ingress::IngressEffectIntent::RouteDirect { fanout, .. } => {
                let Some(source_intent) = intents.iter().find(|source| {
                    super::reflection_dispatch::original_intent(source).as_ref() == Some(intent)
                }) else {
                    continue;
                };
                let waddle_xmpp::ingress::IngressEffectIntent::RouteMucGroupchat { room, .. } =
                    source_intent
                else {
                    continue;
                };
                (room, fanout, source_intent)
            }
            _ => continue,
        };
        if let Ok(source) = super::room_canonical::source(&envelope, source_intent) {
            let expected = super::room_canonical::occupant_copy_message(source, target, &intents);
            if *room == obligation.sender_bare && occupants.contains(target) && expected == *message
            {
                return Ok(());
            }
        }
    }
    Err(AppendAuthorityRejection::StanzaSenderMismatch)
}

/// The canonical ingress row must exist and name the claimed sender.
pub(crate) async fn check_canonical_sender(
    db: &crate::db::Database,
    message_key: MessageKey,
    sender_bare: &jid::BareJid,
) -> Result<(), AppendAuthorityRejection> {
    let sender = tokio::time::timeout(
        AUTHORIZATION_READ_TIMEOUT,
        crate::ingress_substrate::canonical_sender_pooled(db, message_key),
    )
    .await
    .map_err(|_| AppendAuthorityRejection::CanonicalReadTimedOut)?
    .map_err(|_| AppendAuthorityRejection::CanonicalReadFailed)?
    .ok_or(AppendAuthorityRejection::CanonicalSenderMissing)?;
    if &sender != sender_bare {
        return Err(AppendAuthorityRejection::CanonicalSenderMismatch);
    }
    Ok(())
}

/// Record failed authority validation at a relay or accepted-frame drain boundary.
pub(crate) fn record_authorization_failure(
    reason: &AppendAuthorityRejection,
    sender_bare: &jid::BareJid,
) {
    // An `Indeterminate` rejection is correlated with a database problem
    // and fires once per relayed message, so warning on it would flood
    // the logs for the length of an outage. The counter below is the
    // alerting surface for that class; keep the log for the peer-fault
    // class, which should be rare and is worth a line each.
    match reason.failure_class() {
        IngressAppendAuthorizationFailure::Unauthorized => tracing::warn!(
            ?reason,
            sender = %sender_bare,
            "relay append identity unauthorized"
        ),
        IngressAppendAuthorizationFailure::Indeterminate => tracing::debug!(
            ?reason,
            sender = %sender_bare,
            "relay append identity could not be authorized"
        ),
    }
    waddle_xmpp::counter_add!(
        "waddle.clustering.ingress_append.authorization_failed",
        "{obligation}",
        "Relayed ingress append authority failures -- indeterminate means \
         this node could not read canonical state.",
        1,
        reason.failure_class(),
    );
}
