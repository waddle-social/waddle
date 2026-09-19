//! Receiver-side authority for an ingress obligation a peer node relayed.
//!
//! Shared by every place a relayed obligation is about to key an XEP-0198 replay
//! append: the ordered-relay receivers (#1778) and the registered-socket detach
//! drain (#1789). Failure removes the optional deduplication key, never delivery.

use std::time::Duration;

use waddle_xmpp::ingress::{IngressEffectKind, MessageKey};
use waddle_xmpp::telemetry::attributes::IngressAppendAuthorizationFailure;
use waddle_xmpp::Stanza;

const AUTHORIZATION_READ_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) enum AppendAuthorityRejection {
    IneligibleKind,
    NotMessage,
    SenderClaimMismatch,
    StanzaSenderMismatch,
    ServicesUnavailable,
    CanonicalReadFailed,
    CanonicalReadTimedOut,
    CanonicalSenderMissing,
    CanonicalSenderMismatch,
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
            | Self::SenderClaimMismatch
            | Self::StanzaSenderMismatch
            | Self::CanonicalSenderMissing
            | Self::CanonicalSenderMismatch => IngressAppendAuthorizationFailure::Unauthorized,
            Self::ServicesUnavailable | Self::CanonicalReadFailed | Self::CanonicalReadTimedOut => {
                IngressAppendAuthorizationFailure::Indeterminate
            }
        }
    }
}

/// Only recorded direct and MUC groupchat routes allocate keyed SM appends.
pub(crate) fn receipt_kind_is_append_eligible(storage_tag: i32) -> bool {
    [
        IngressEffectKind::RouteDirect,
        IngressEffectKind::RouteMucGroupchat,
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
    let Stanza::Message(message) = stanza else {
        return Err(AppendAuthorityRejection::NotMessage);
    };
    if message.from.as_ref().map(jid::Jid::to_bare).as_ref() != Some(sender_bare) {
        return Err(AppendAuthorityRejection::StanzaSenderMismatch);
    }
    Ok(())
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

/// The canonical ingress row must exist and name the sender the stanza carries.
pub(crate) async fn check_canonical_authority(
    db: &crate::db::Database,
    stanza: &Stanza,
    message_key: MessageKey,
    sender_bare: &jid::BareJid,
    receipt_kind_storage_tag: i32,
) -> Result<(), AppendAuthorityRejection> {
    check_stanza_binding(stanza, sender_bare, receipt_kind_storage_tag)?;
    check_canonical_sender(db, message_key, sender_bare).await
}

/// Record that a relayed obligation degraded to unkeyed delivery.
pub(crate) fn record_degraded_to_unkeyed(
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
            "relay append identity unauthorized; continuing with unkeyed delivery"
        ),
        IngressAppendAuthorizationFailure::Indeterminate => tracing::debug!(
            ?reason,
            sender = %sender_bare,
            "relay append identity could not be authorized; continuing with \
             unkeyed delivery"
        ),
    }
    waddle_xmpp::counter_add!(
        "waddle.clustering.ingress_append.authorization_failed",
        "{obligation}",
        "Relayed ingress append identities degraded to unkeyed delivery -- \
         `indeterminate` means this node could not read canonical state and the \
         cross-node duplicate window is open for as long as it persists.",
        1,
        reason.failure_class(),
    );
}
