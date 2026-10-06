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

#[cfg(all(test, feature = "clustering"))]
#[path = "append_authority_deferred_tests.rs"]
mod deferred_tests;

const AUTHORIZATION_READ_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
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

/// Whether a keyed append context's claim is proven against the canonical row.
///
/// Never serialized: relays forward only the obligation's data fields, and every
/// receiver establishes its own authority (#1790).
#[derive(Clone)]
pub(crate) enum AppendAuthority {
    /// Minted by this node's own ingress commit, or already verified against the
    /// canonical row.
    Verified,
    /// A relayed claim that passed the synchronous checks only (sender claim and
    /// stanza binding). The canonical read runs on the first
    /// [`AppendAuthority::ensure_verified`], once per context clone-tree that
    /// completes it: a caller cancelled inside the read leaves the cell
    /// uninitialized, so a later caller reads again. The failure counter cannot
    /// double-count, because recording and caching a result share no await.
    #[cfg(feature = "clustering")]
    Deferred(std::sync::Arc<DeferredAppendAuthority>),
}

impl std::fmt::Debug for AppendAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Verified => f.write_str("Verified"),
            #[cfg(feature = "clustering")]
            Self::Deferred(deferred) => f
                .debug_struct("Deferred")
                .field("resolved", &deferred.result.get())
                .finish(),
        }
    }
}

/// The relayed claim and the database it is verified against. The cached
/// result is shared by every clone of the context; see
/// [`AppendAuthority::Deferred`] for why a cancelled read can repeat.
#[cfg(feature = "clustering")]
pub(crate) struct DeferredAppendAuthority {
    db: crate::db::Database,
    obligation: super::identity::IngressAppendObligationRef,
    result: tokio::sync::OnceCell<Result<(), AppendAuthorityRejection>>,
}

#[cfg(feature = "clustering")]
impl DeferredAppendAuthority {
    pub(crate) fn new(
        db: crate::db::Database,
        obligation: super::identity::IngressAppendObligationRef,
    ) -> Self {
        Self {
            db,
            obligation,
            result: tokio::sync::OnceCell::new(),
        }
    }
}

impl AppendAuthority {
    /// Resolve the canonical authority before the first action that trusts it.
    /// A deferred claim's completed read is cached, and its failure recorded
    /// once (see [`AppendAuthority::Deferred`] for cancellation).
    pub(crate) async fn ensure_verified(
        &self,
        stanza: &Stanza,
    ) -> Result<(), AppendAuthorityRejection> {
        match self {
            Self::Verified => {
                // Only a deferred claim inspects the stanza it is bound to.
                let _ = stanza;
                Ok(())
            }
            #[cfg(feature = "clustering")]
            Self::Deferred(deferred) => deferred
                .result
                .get_or_init(|| {
                    // Keep canonical decoding state off the caller's future:
                    // unoptimized interpreter futures must fit the default stack.
                    Box::pin(async {
                        let result =
                            check_canonical_obligation(&deferred.db, stanza, &deferred.obligation)
                                .await;
                        if let Err(reason) = &result {
                            record_authorization_failure(reason, &deferred.obligation.sender_bare);
                        }
                        result
                    })
                })
                .await
                .clone(),
        }
    }
}

/// Canonical-row authorization reads, counted per test process (nextest runs
/// one test per process) so tests can assert where the read happens (#1790).
#[cfg(all(test, feature = "clustering"))]
pub(crate) mod canonical_reads {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static READS: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn record() {
        READS.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn count() -> usize {
        READS.load(Ordering::SeqCst)
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
    #[cfg(all(test, feature = "clustering"))]
    canonical_reads::record();
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
    } else if obligation.receipt.kind.to_storage() == IngressEffectKind::RouteDirect.storage_tag() {
        tokio::time::timeout(
            AUTHORIZATION_READ_TIMEOUT,
            authorize_direct_sender(db, stanza, obligation),
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

async fn authorize_direct_sender(
    db: &crate::db::Database,
    stanza: &Stanza,
    obligation: &super::identity::IngressAppendObligationRef,
) -> Result<(), AppendAuthorityRejection> {
    let (envelope, intents) =
        crate::ingress_uow::CarbonReceiptRepository::load_authority(db, obligation.message_key)
            .await
            .map_err(|_| AppendAuthorityRejection::CanonicalReadFailed)?;
    if let Some(intent) = intents.iter().find(|intent| {
        matches!(
            intent,
            waddle_xmpp::ingress::IngressEffectIntent::RouteDirect {
                prepared: Some(_),
                ..
            }
        ) && super::receipt_key(intent).ok().as_ref() == Some(&obligation.receipt)
    }) {
        let expected = super::recorded::prepared_direct_message(&envelope, intent)
            .ok_or(AppendAuthorityRejection::StanzaSenderMismatch)?;
        let Stanza::Message(message) = stanza else {
            return Err(AppendAuthorityRejection::NotMessage);
        };
        return if expected.from.as_ref().map(jid::Jid::to_bare).as_ref()
            == Some(&obligation.sender_bare)
            && same_message_content(expected, message)
        {
            Ok(())
        } else {
            Err(AppendAuthorityRejection::StanzaSenderMismatch)
        };
    }
    let expected =
        super::invitation_authority::recorded_message(&envelope, &intents, &obligation.receipt)
            .map_err(|_| AppendAuthorityRejection::StanzaSenderMismatch)?;
    if let Some(expected) = expected {
        let Stanza::Message(message) = stanza else {
            return Err(AppendAuthorityRejection::NotMessage);
        };
        if expected.from.as_ref().map(jid::Jid::to_bare).as_ref() != Some(&obligation.sender_bare)
            || !same_message_content(&expected, message)
        {
            return Err(AppendAuthorityRejection::StanzaSenderMismatch);
        }
    } else if envelope
        .message()
        .from
        .as_ref()
        .map(jid::Jid::to_bare)
        .as_ref()
        != Some(&obligation.sender_bare)
    {
        return Err(AppendAuthorityRejection::CanonicalSenderMismatch);
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
            let expected = super::room_canonical::occupant_copy_message(source, target);
            if *room == obligation.sender_bare
                && occupants.contains(target)
                && same_message_content(&expected, message)
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

/// Compare the frozen message with its recipient-pass copy. Messages omitted
/// from archives still get an XEP-0359 recipient stamp, but have no intent to
/// persist that generated ID. Permit precisely that one typed stamp and compare
/// every other field, including sender-owned IDs and extension payloads.
pub(super) fn recipient_copy_matches(
    expected: &xmpp_parsers::message::Message,
    offered: &xmpp_parsers::message::Message,
    recipient: &jid::BareJid,
    archived_recipient: bool,
) -> bool {
    if same_message_content(expected, offered) {
        return true;
    }
    if archived_recipient
        || waddle_xmpp::protocol::handlers::archive::is_archivable(expected)
        || expected
            .from
            .as_ref()
            .is_none_or(|from| from.to_bare() == *recipient)
    {
        return false;
    }
    let stamps: Vec<_> = waddle_xmpp_core::xep0359::extract_stanza_ids(offered)
        .into_iter()
        .filter(|stamp| stamp.by == *recipient)
        .collect();
    let [stamp] = stamps.as_slice() else {
        return false;
    };
    let mut expected = expected.clone();
    waddle_xmpp_core::xep0359::add_stanza_id(&mut expected, stamp);
    same_message_content(&expected, offered)
}

/// Stored parent-bearing threads are parsed back into the last payload slot.
/// Room and recipient processing may have appended stamps after that slot in
/// the live copy. Normalize only this parser representation; every thread
/// attribute, payload, and the ordering of all other extensions remains exact.
pub(super) fn same_message_content(
    expected: &xmpp_parsers::message::Message,
    offered: &xmpp_parsers::message::Message,
) -> bool {
    if expected == offered {
        return true;
    }
    let normalize = |message: &xmpp_parsers::message::Message| {
        let mut normalized = message.clone();
        normalized.payloads.sort_by_key(|payload| {
            waddle_xmpp_core::xep0201::is_thread_element_for_stanza(
                payload,
                waddle_xmpp_core::xep0201::CLIENT_STANZA_NS,
            )
        });
        normalized
    };
    normalize(expected) == normalize(offered)
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
