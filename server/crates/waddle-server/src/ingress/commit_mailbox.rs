//! Remote room acceptance through the durable mailbox, as part of the ingress
//! commit.
//!
//! A groupchat message for a room owned by another node is committed twice:
//! by the origin, which records the sender's submission, and by the room
//! owner, which decides it. With the mailbox selected, the two commits meet
//! on one mailbox entry keyed by the canonical message:
//!
//! - the origin's transaction binds the message to its authenticated source,
//!   opens the owner decision and appends the entry `Pending`;
//! - the owner's transaction seals the room manifest (or records the
//!   rejection), latches `owner_decision` and moves the entry to
//!   `OwnerAccepted` or `OwnerRejected`, all before it commits.
//!
//! Neither transaction waits on the network. The lane row is locked before
//! the canonical message in both; the origin takes it after its stream row,
//! which no owner or collector transaction ever locks.

use jid::BareJid;
use waddle_xmpp::ingress::mailbox::{
    MailboxAdmissionLimits, MailboxAdmissionRefusal, MailboxDispositionChange,
    MailboxEntryDisposition, MailboxLaneFence, MailboxLaneIdentity, MailboxLaneOrigin,
    MailboxLaneRecipient, MailboxOperationKey, MailboxOwnerDecision, MailboxRoomLane,
};
use waddle_xmpp::ingress::{MessageKey, NormalizedTarget};

use super::commit_stream::StreamAttempt;
use super::identity::IngressStreamIdentity;
use super::principal::IngressPrincipal;
use super::submission::IngressSubmission;
use super::RoomExecutionPath;
use crate::ingress_substrate::{CanonicalSourceBinding, MessageEnvelope};
use crate::ingress_uow::{
    CanonicalMessageRepository, IngressUnitOfWork, IngressUowError, IngressUowTransaction,
    LockedMailboxLane, MailboxError, MailboxRepository, RoomOwnerDecision, StoredMailboxEntry,
};

/// Pending-entry caps for remote room acceptance. A lane is one sender
/// session's traffic to one room, so its backlog is small unless the owner
/// has stalled; the node cap bounds what one stalled owner can accumulate.
pub(crate) const ROOM_ACCEPTANCE_LIMITS: MailboxAdmissionLimits = MailboxAdmissionLimits {
    lane_pending: 64,
    node_pending: 16_384,
};

/// The origin's side of one remote room acceptance, between its lane lock and
/// its entry append.
pub(super) struct OriginAcceptance {
    lane: LockedMailboxLane,
    room: BareJid,
    source: CanonicalSourceBinding,
}

impl OriginAcceptance {
    /// Source binding the canonical message is created with.
    pub(super) fn source(&self) -> &CanonicalSourceBinding {
        &self.source
    }
}

/// Result of the origin's admission check, taken under the lane lock.
pub(super) enum OriginAdmission {
    Admitted(Box<OriginAcceptance>),
    /// The lane or the owner's node is at its pending cap. Nothing was
    /// appended; the caller refuses the stanza.
    Full(MailboxAdmissionRefusal),
}

/// Whether this submission is a groupchat message an origin node dispatches
/// to a room owned elsewhere, and so takes part in mailbox acceptance.
fn origin_room(submission: &IngressSubmission) -> Option<&BareJid> {
    let RoomExecutionPath::Remote { room, .. } = &submission.plan.room_execution else {
        return None;
    };
    let groupchat =
        submission.plan.sanitized_message.type_ == xmpp_parsers::message::MessageType::Groupchat;
    let addressed = submission.target == NormalizedTarget::Bare(room.clone());
    let from_client = matches!(
        submission.identity,
        IngressStreamIdentity::Resumable { .. } | IngressStreamIdentity::Ephemeral { .. }
    );
    (groupchat && addressed && from_client).then_some(room)
}

fn lane_identity(submission: &IngressSubmission, room: &BareJid) -> MailboxLaneIdentity {
    let origin = match &submission.identity {
        IngressStreamIdentity::Resumable { stream_id, .. } => {
            MailboxLaneOrigin::SmSession(stream_id.clone())
        }
        // A stream without stream management has no session that outlives
        // it; its lane is the bound resource.
        _ => MailboxLaneOrigin::Entity(waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            submission.sender.to_string(),
        )),
    };
    MailboxLaneIdentity {
        origin,
        recipient: MailboxLaneRecipient::Room {
            room: room.clone(),
            lane: MailboxRoomLane::MucStanza,
        },
    }
}

/// The fence the origin presents: its own stream claim epoch and the room
/// claim as the claims table records it in this transaction.
#[cfg(feature = "clustering")]
async fn origin_fence(
    tx: &mut IngressUowTransaction<'_>,
    submission: &IngressSubmission,
    room: &BareJid,
) -> Result<MailboxLaneFence, IngressUowError> {
    use crate::ingress_uow::ClaimRepository;
    use crate::server::routes::interpret::effects::PlanFailure;
    let origin_epoch = match &submission.identity {
        IngressStreamIdentity::Resumable { claim_epoch, .. } => *claim_epoch,
        _ => waddle_xmpp::ownership::ClaimEpoch(0),
    };
    let (target_owner, target_epoch) = ClaimRepository::room_claim(tx, room)
        .await?
        .ok_or(IngressUowError::Plan(PlanFailure::RoomClaimStale))?;
    Ok(MailboxLaneFence {
        origin_epoch,
        target_epoch,
        target_owner,
    })
}

/// Without clustering no room is owned by another node, and
/// [`IngressUnitOfWork::mailbox_acceptance_enabled`] is never true.
#[cfg(not(feature = "clustering"))]
async fn origin_fence(
    _tx: &mut IngressUowTransaction<'_>,
    _submission: &IngressSubmission,
    _room: &BareJid,
) -> Result<MailboxLaneFence, IngressUowError> {
    Err(MailboxError::PostgresRequired.into())
}

/// Lock the origin's lane and check admission, before any canonical row is
/// locked. `None` when the submission does not take part in mailbox
/// acceptance.
pub(super) async fn begin_origin(
    uow: &IngressUnitOfWork,
    tx: &mut IngressUowTransaction<'_>,
    submission: &IngressSubmission,
    stream: Option<&StreamAttempt>,
) -> Result<Option<OriginAdmission>, IngressUowError> {
    if !uow.mailbox_acceptance_enabled() || submission.plan.rejection.is_some() {
        return Ok(None);
    }
    let Some(room) = origin_room(submission) else {
        return Ok(None);
    };
    let IngressPrincipal::Authenticated(principal) = &submission.principal else {
        return Ok(None);
    };
    let fence = origin_fence(tx, submission, room).await?;
    let lane = MailboxRepository::lock_lane(tx, &lane_identity(submission, room), &fence).await?;
    // A stanza bound to this wire position by an earlier attempt already has
    // its entry, so a retransmission is never refused for a backlog it is
    // itself part of.
    let rebound = stream.is_some_and(|stream| stream.bound.is_some());
    if !rebound {
        let load = MailboxRepository::pending_load(tx, &lane).await?;
        if let Err(refusal) = ROOM_ACCEPTANCE_LIMITS.admit(load) {
            return Ok(Some(OriginAdmission::Full(refusal)));
        }
    }
    Ok(Some(OriginAdmission::Admitted(Box::new(
        OriginAcceptance {
            lane,
            room: room.clone(),
            source: CanonicalSourceBinding::new(
                principal.clone(),
                stream.map(|stream| (stream.id, stream.ordinal)),
            ),
        },
    ))))
}

/// Open the owner decision and append the pending entry for `key`, after the
/// origin's dispatch intent is recorded and under the canonical row lock.
///
/// A message that carries no source binding predates mailbox acceptance: it
/// keeps the relay's behaviour and gets no entry, so nothing is recorded as
/// awaiting an owner decision that the earlier path never promised.
pub(super) async fn record_origin(
    tx: &mut IngressUowTransaction<'_>,
    acceptance: &mut OriginAcceptance,
    key: MessageKey,
) -> Result<Option<StoredMailboxEntry>, IngressUowError> {
    let state = CanonicalMessageRepository::room_owner_state(tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    if !state.source_bound {
        return Ok(None);
    }
    let dispatched = crate::ingress_uow::EffectIntentRepository::load(tx, key)
        .await?
        .iter()
        .any(|intent| {
            matches!(intent,
                waddle_xmpp::ingress::IngressEffectIntent::DispatchToRoomRemote { room, .. }
                    if room == &acceptance.room)
        });
    // A replay that records no dispatch to the owner asks nothing of it.
    if !dispatched {
        return Ok(None);
    }
    if state.decision.is_none() {
        CanonicalMessageRepository::initialize_room_owner_decision(tx, key, &acceptance.room)
            .await?;
    }
    let outcome = MailboxRepository::append(
        tx,
        &mut acceptance.lane,
        MailboxOperationKey::RoomAcceptance(key),
        &ROOM_ACCEPTANCE_LIMITS,
    )
    .await?;
    Ok(Some(*outcome.entry()))
}

/// The owner's side of one remote room acceptance: the entry's lane, locked
/// before the canonical message.
pub(super) struct OwnerAcceptance {
    lane: LockedMailboxLane,
    operation: MailboxOperationKey,
}

/// What the owner's commit concluded for the relayed message.
pub(super) enum OwnerOutcome<'a> {
    Accepted,
    /// The owner refused the message. `reply` is the frozen error stanza it
    /// answers the sender with; a replay of a recorded refusal may carry
    /// none because the evidence already exists.
    Rejected {
        reply: Option<&'a xmpp_parsers::message::Message>,
        sender: &'a jid::FullJid,
    },
}

/// Lock the lane of the relayed message's entry as its target, after the
/// owner has asserted its room claim and before it locks the canonical
/// message. `None` when the origin appended no entry.
#[cfg(feature = "clustering")]
pub(super) async fn begin_owner(
    uow: &IngressUnitOfWork,
    tx: &mut IngressUowTransaction<'_>,
    submission: &IngressSubmission,
) -> Result<Option<OwnerAcceptance>, IngressUowError> {
    use crate::ingress_uow::MailboxTargetFence;
    if !uow.mailbox_acceptance_enabled() {
        return Ok(None);
    }
    let IngressStreamIdentity::Relayed {
        canonical,
        room_fence,
        ..
    } = &submission.identity
    else {
        return Ok(None);
    };
    let operation = MailboxOperationKey::RoomAcceptance(canonical.message_key);
    let Some(entry) = MailboxRepository::lookup(tx, operation).await? else {
        return Ok(None);
    };
    let lane = MailboxRepository::lock_lane_for_target(
        tx,
        entry.lane,
        &MailboxTargetFence {
            target_epoch: room_fence.epoch,
            target_owner: room_fence.owner.clone(),
        },
    )
    .await?;
    Ok(Some(OwnerAcceptance { lane, operation }))
}

#[cfg(not(feature = "clustering"))]
pub(super) async fn begin_owner(
    _uow: &IngressUnitOfWork,
    _tx: &mut IngressUowTransaction<'_>,
    _submission: &IngressSubmission,
) -> Result<Option<OwnerAcceptance>, IngressUowError> {
    Ok(None)
}

/// Whether the owner has yet to decide the relayed message `key`. The caller
/// holds the canonical row lock.
pub(super) async fn owner_undecided(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
) -> Result<bool, IngressUowError> {
    Ok(CanonicalMessageRepository::room_owner_state(tx, key)
        .await?
        .is_some_and(|state| state.decision == Some(RoomOwnerDecision::Undecided)))
}

/// Latch the owner's decision on the canonical message and on its mailbox
/// entry, in the transaction that sealed the manifest or recorded the
/// rejection. A replay of the same decision changes nothing; the opposite
/// decision fails the commit.
pub(super) async fn decide_owner(
    tx: &mut IngressUowTransaction<'_>,
    acceptance: &mut OwnerAcceptance,
    outcome: OwnerOutcome<'_>,
) -> Result<MailboxEntryDisposition, IngressUowError> {
    let key = acceptance.operation.message_key();
    let state = CanonicalMessageRepository::room_owner_state(tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    // A latched decision is the authority on a replay, whatever today's
    // plan concluded.
    let decision = match (state.decision, &outcome) {
        (Some(RoomOwnerDecision::Accepted), _) => MailboxOwnerDecision::Accepted,
        (Some(RoomOwnerDecision::Rejected), _) => MailboxOwnerDecision::Rejected,
        (Some(RoomOwnerDecision::Undecided), OwnerOutcome::Accepted) => {
            MailboxOwnerDecision::Accepted
        }
        (Some(RoomOwnerDecision::Undecided), OwnerOutcome::Rejected { .. }) => {
            MailboxOwnerDecision::Rejected
        }
        (None, _) => return Err(MailboxError::EntryMissing.into()),
    };
    if state.decision == Some(RoomOwnerDecision::Undecided) {
        match outcome {
            OwnerOutcome::Accepted => {
                CanonicalMessageRepository::decide_room_owner(tx, key, RoomOwnerDecision::Accepted)
                    .await?;
            }
            OwnerOutcome::Rejected { reply, sender } => {
                let reply = reply.ok_or(IngressUowError::EffectIntentConflict)?;
                let room = match acceptance.lane.key().encoding().decode() {
                    Ok(MailboxLaneIdentity {
                        recipient: MailboxLaneRecipient::Room { room, .. },
                        ..
                    }) => room,
                    _ => return Err(MailboxError::InvalidStored.into()),
                };
                let child = record_rejection_child(tx, reply, sender, &room).await?;
                CanonicalMessageRepository::record_owner_rejection(tx, key, child).await?;
                CanonicalMessageRepository::decide_room_owner(tx, key, RoomOwnerDecision::Rejected)
                    .await?;
            }
        }
    }
    let change =
        MailboxRepository::decide(tx, &mut acceptance.lane, acceptance.operation, decision).await?;
    Ok(match change {
        MailboxDispositionChange::Applied(disposition)
        | MailboxDispositionChange::Unchanged(disposition) => disposition,
    })
}

/// Store the owner's frozen error as its own canonical message. It carries
/// no obligation of its own (the reply is delivered from the source's
/// recorded error intent), so it is terminal from the start and is collected
/// together with its source.
async fn record_rejection_child(
    tx: &mut IngressUowTransaction<'_>,
    reply: &xmpp_parsers::message::Message,
    sender: &jid::FullJid,
    room: &BareJid,
) -> Result<MessageKey, IngressUowError> {
    use waddle_xmpp::ingress::DigestContext;
    let child = MessageKey::new();
    let digest_input = super::submission::digest_input(
        reply,
        &DigestContext {
            target: NormalizedTarget::Full(sender.clone()),
            server_authorities: vec![sender.to_bare(), room.clone()],
            stanza_lang: None,
        },
    )
    .map_err(|_| IngressUowError::EffectIntentConflict)?;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&digest_input);
    CanonicalMessageRepository::record_message(
        tx,
        child,
        &digest,
        Some(&MessageEnvelope::new(reply.clone())),
    )
    .await?;
    CanonicalMessageRepository::terminalize(tx, child, chrono::Utc::now()).await?;
    Ok(child)
}
