//! XEP-0045 administration of a room an enrolled host-grant scope manages.
//!
//! The canonical grants decide every part of it. The initiator's authority
//! is the standing those grants honour, not the room actor's affiliation
//! list or occupant role; an affiliation change is recorded in the
//! host-grant store first, where the initiator is revalidated under the
//! scope's guard, and the room actor applies it afterwards as a mirror. No
//! authorization relationship is written by the handler: the store's
//! projection writes the channel's affiliation family, conditioned on its
//! marker. A change the store refuses is never applied to the room actor.
//!
//! Request identity: each affiliation change is recorded under a fresh
//! operation identity, decided at the generation the handler read. An IQ
//! identifier is not a durable idempotency key, so a retransmitted request
//! is a new operation; setting an affiliation a subject already holds
//! records a new generation and changes nothing it admits. The reply
//! reports the room actor's application after the change was recorded: a
//! change that was recorded stays recorded even when the actor then fails,
//! and it is what every admission reads.

use jid::{BareJid, FullJid};
use waddle_xmpp::identity::CanonicalDomain;
use xmpp_parsers::stanza_error::StanzaError;

use super::{
    conflict_iq_error, forbidden_iq_error, internal_server_error_iq_error, jid_malformed_iq_error,
    not_allowed_iq_error, service_unavailable_iq_error, warn, Affiliation, GetSnapshot,
    WebSocketState,
};
use crate::permissions::host_grant::{
    GrantOperationId, GrantSubject, HostGrantAction, HostGrantAuthority, HostGrantError,
    IssuancePolicy, ManagedRoomGrants, OperationOutcome, RoomAffiliation, RoomBinding, RoomGate,
    SetAffiliation,
};
use crate::server::routes::websocket::handlers::iq::errors::resource_constraint_iq_error;

/// Most times a change is decided again after another operation of the
/// scope was recorded between the handler's read and its write.
const STALE_GENERATION_ATTEMPTS: usize = 3;

pub(super) fn room_affiliation(affiliation: Affiliation) -> RoomAffiliation {
    match affiliation {
        Affiliation::Owner => RoomAffiliation::Owner,
        Affiliation::Admin => RoomAffiliation::Admin,
        Affiliation::Member => RoomAffiliation::Member,
        Affiliation::None => RoomAffiliation::None,
        Affiliation::Outcast => RoomAffiliation::Outcast,
    }
}

pub(super) fn muc_affiliation(affiliation: RoomAffiliation) -> Affiliation {
    match affiliation {
        RoomAffiliation::Owner => Affiliation::Owner,
        RoomAffiliation::Admin => Affiliation::Admin,
        RoomAffiliation::Member => Affiliation::Member,
        RoomAffiliation::None => Affiliation::None,
        RoomAffiliation::Outcast => Affiliation::Outcast,
    }
}

/// The standing the canonical grants honour for `sender`, as the MUC
/// affiliation the room actor applies an administration request under.
pub(super) fn sender_affiliation(room: &ManagedRoomGrants, sender: &FullJid) -> Affiliation {
    muc_affiliation(room.standing(&sender.to_bare()))
}

/// Whether `sender` may moderate the room: change roles, read the role
/// lists and see the room's administration lists. Only an owner or admin
/// standing that admission honours confers it.
pub(super) fn sender_moderates(room: &ManagedRoomGrants, sender: &FullJid) -> bool {
    room.allows(&sender.to_bare(), HostGrantAction::Moderate)
}

/// The affiliation list of a room an enrolled scope manages, from the
/// canonical grants as recorded: an increase that waits for its projection
/// is listed, since that is the list as administered. The member list is
/// answered to anyone who may read the room (XEP-0045 §9.5); every other
/// list to a moderator.
pub(super) fn host_grant_affiliation_list(
    room: &ManagedRoomGrants,
    sender: &FullJid,
    filter: Option<Affiliation>,
) -> Result<Vec<(BareJid, Affiliation)>, Box<StanzaError>> {
    let sender_bare = sender.to_bare();
    let member_list =
        filter == Some(Affiliation::Member) && room.allows(&sender_bare, HostGrantAction::Read);
    if !member_list && !sender_moderates(room, sender) {
        return Err(Box::new(forbidden_iq_error("Operation not permitted.")));
    }
    let affiliations = match filter {
        Some(Affiliation::None) => Vec::new(),
        Some(affiliation) => vec![room_affiliation(affiliation)],
        None => RoomAffiliation::PROJECTED.to_vec(),
    };
    Ok(affiliations
        .into_iter()
        .flat_map(|affiliation| {
            room.grants
                .recorded(affiliation)
                .into_iter()
                .map(move |jid| (jid, muc_affiliation(affiliation)))
        })
        .collect())
}

/// Record one affiliation change in the host-grant store before the room
/// actor applies it. `Ok` means the change is recorded: a reduction is
/// effective at once, and an increase once its projection has been verified
/// and activated by the reconciliation this starts.
///
/// The refusal says whether the change may have been recorded anyway: its
/// commit was not acknowledged and could not be read back.
pub(crate) async fn record_host_grant_affiliation(
    state: &WebSocketState,
    room: &ManagedRoomGrants,
    initiator: &BareJid,
    target: &BareJid,
    affiliation: Affiliation,
) -> Result<(), RecordRefusal> {
    let gate = &state.deps.app_state.host_grants;
    let (Some(store), Some(attachment)) = (gate.store(), gate.attachment()) else {
        return Err(RecordRefusal::refused(service_unavailable_iq_error(
            "The room is not available.",
        )));
    };
    let issuance = issuance_policy(state, attachment.served()).ok_or_else(|| {
        RecordRefusal::refused(service_unavailable_iq_error("The room is not available."))
    })?;
    let initiator = GrantSubject::new(initiator.clone())
        .map_err(|_| RecordRefusal::refused(forbidden_iq_error("Operation not permitted.")))?;
    let subject = GrantSubject::new(target.clone()).map_err(|_| {
        RecordRefusal::refused(jid_malformed_iq_error(
            "Room affiliations require a bare account JID.",
        ))
    })?;
    let authority = HostGrantAuthority::new(
        store.clone(),
        state.deps.app_state.permission_actor.clone(),
        issuance,
    );
    let mut generation = room.grants.generation;
    for _ in 0..STALE_GENERATION_ATTEMPTS {
        let command = SetAffiliation {
            operation: GrantOperationId::generate(),
            lifecycle: room.grants.lifecycle,
            expected_generation: generation,
            initiator: initiator.clone(),
            room: room.grants.room.clone(),
            subject: subject.clone(),
            affiliation: room_affiliation(affiliation),
        };
        match authority.set_affiliation(&command).await {
            Ok(_) => {
                spawn_reconcile(authority, command);
                return Ok(());
            }
            Err(HostGrantError::StaleGeneration { current }) => generation = current,
            Err(HostGrantError::CommitOutcomeUnknown) => {
                return match store.operation_outcome(command.operation).await {
                    Ok(OperationOutcome::Committed(_)) => {
                        spawn_reconcile(authority, command);
                        Ok(())
                    }
                    Ok(OperationOutcome::Absent) => Err(RecordRefusal::refused(
                        internal_server_error_iq_error("Internal server error."),
                    )),
                    Err(error) => {
                        warn!(
                            kind = error.kind(),
                            "A room affiliation change has an unknown outcome"
                        );
                        Err(RecordRefusal {
                            error: Box::new(internal_server_error_iq_error(
                                "Internal server error.",
                            )),
                            maybe_recorded: true,
                        })
                    }
                };
            }
            Err(error) => return Err(RecordRefusal::refused(refusal(&error))),
        }
    }
    Err(RecordRefusal::refused(resource_constraint_iq_error(
        "The room's grants are changing; try again.",
    )))
}

/// Why an affiliation change was not recorded, as the request's reply.
pub(crate) struct RecordRefusal {
    pub(crate) error: Box<StanzaError>,
    /// The commit was not acknowledged and its outcome could not be read
    /// back: the store may hold the change.
    pub(crate) maybe_recorded: bool,
}

impl RecordRefusal {
    fn refused(error: StanzaError) -> Self {
        Self {
            error: Box::new(error),
            maybe_recorded: false,
        }
    }
}

/// The issuance policy of this replica: its served account domains, the
/// domains of its own services, and no trusted home. Until the operator's
/// trusted-home list is configured, an owner or admin grant to a remote
/// account is refused (ADR-0020 D8).
fn issuance_policy(
    state: &WebSocketState,
    served: &crate::permissions::host_grant::ServedAccountDomains,
) -> Option<IssuancePolicy> {
    let domains = &state.deps.service_domains;
    let services = [
        &domains.muc,
        &domains.spaces,
        &domains.upload,
        &domains.extensions,
        &domains.push,
        &domains.community,
    ]
    .into_iter()
    .map(|domain| CanonicalDomain::parse_exact(domain))
    .collect::<Result<Vec<_>, _>>()
    .ok()?;
    IssuancePolicy::for_served(served, services, []).ok()
}

/// Delays before each further reconciliation pass of a recorded change whose
/// pass did not finish: about a minute in all.
const RECONCILE_RETRY_DELAYS: [std::time::Duration; 6] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(4),
    std::time::Duration::from_secs(8),
    std::time::Duration::from_secs(16),
    std::time::Duration::from_secs(32),
];

/// Project the recorded change and activate it if it is an increase. The
/// reply does not wait. A pass that does not finish (the projection did not
/// answer, another operation got ahead, the store was busy) is repeated in
/// the background after each of [`RECONCILE_RETRY_DELAYS`]; after the last,
/// the change stays recorded and not effective until the next change of
/// the scope or the recovery pass a replica runs when it attaches to the
/// store. A contradiction or a scope that no longer admits changes is not
/// retried.
fn spawn_reconcile(authority: HostGrantAuthority, command: SetAffiliation) {
    use crate::permissions::host_grant::Reconciliation;
    tokio::spawn(async move {
        let mut delays = RECONCILE_RETRY_DELAYS.iter();
        loop {
            let retry = match authority.reconcile(command.lifecycle).await {
                Ok(Reconciliation::Activated(_) | Reconciliation::AlreadyActive(_)) => {
                    tracing::debug!("Reconciled a room affiliation change's projection");
                    return;
                }
                Ok(Reconciliation::Contradiction(_)) => {
                    warn!(
                        kind = "contradiction",
                        "A room affiliation change's projection contradicts the store"
                    );
                    return;
                }
                Ok(Reconciliation::Superseded | Reconciliation::ProjectionUnknown) => true,
                Err(error) => reconcile_error_is_transient(&error),
            };
            let Some(delay) = delays.next().filter(|_| retry) else {
                warn!(
                    kind = "unfinished",
                    "A room affiliation change's projection was not reconciled; it stays \
                     recorded and not effective until the scope's next reconciliation"
                );
                return;
            };
            tokio::time::sleep(*delay).await;
        }
    });
}

/// Whether a failed reconciliation pass may finish if repeated. A scope
/// that was retired, exhausted or bound to another incarnation or policy
/// version, or whose rows are corrupt, will not.
fn reconcile_error_is_transient(error: &HostGrantError) -> bool {
    !matches!(
        error,
        HostGrantError::ScopeRetired
            | HostGrantError::ScopeExhausted
            | HostGrantError::IncarnationChanged
            | HostGrantError::PolicyVersionUnsupported
            | HostGrantError::UnknownScope
            | HostGrantError::CorruptRow { .. }
    )
}

/// The stanza error a refused change replies with. None names a JID, a room
/// or a Space.
fn refusal(error: &HostGrantError) -> StanzaError {
    match error {
        HostGrantError::InitiatorNotAuthorized => forbidden_iq_error("Operation not permitted."),
        HostGrantError::LastOwner => conflict_iq_error("Cannot remove the last owner from a room."),
        HostGrantError::Issuance(_) => {
            not_allowed_iq_error("That affiliation cannot be granted to this account.")
        }
        HostGrantError::Subject(_) => {
            jid_malformed_iq_error("Room affiliations require a bare account JID.")
        }
        HostGrantError::GrantLimit => {
            resource_constraint_iq_error("The room holds as many grants as it can.")
        }
        HostGrantError::Busy => resource_constraint_iq_error("The room is busy; try again."),
        HostGrantError::ScopeRetired
        | HostGrantError::ScopeExhausted
        | HostGrantError::IncarnationChanged
        | HostGrantError::PolicyVersionUnsupported
        | HostGrantError::UnknownScope
        | HostGrantError::UnknownRoom => service_unavailable_iq_error("The room is not available."),
        other => {
            warn!(kind = other.kind(), "A room affiliation change failed");
            internal_server_error_iq_error("Internal server error.")
        }
    }
}

/// Delays before each attempt to put a managed room actor's affiliation
/// list back in line with the canonical grants.
const MIRROR_REPAIR_DELAYS: [std::time::Duration; 5] = [
    std::time::Duration::from_millis(250),
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(4),
    std::time::Duration::from_secs(16),
    std::time::Duration::from_secs(60),
];

/// A repair of the room actor's affiliation list for the subjects of an
/// administration request's changes, armed before the store records the
/// first of them and disarmed once the room actor applied them all, or when
/// the request was refused before the store could hold any of them.
///
/// The actor's list of a managed room is a mirror: no admission reads it,
/// but the role it seats an occupant with and the lists a stale actor shows
/// follow it. When the actor did not apply a recorded change (it refused,
/// failed, timed out or answered ambiguously), or when the request stopped
/// after the store may have recorded some of them, dropping the armed repair
/// sets each subject's entry to the affiliation the canonical grants
/// record, in the background, after each of [`MIRROR_REPAIR_DELAYS`] until
/// one attempt succeeds.
pub(super) struct MirrorRepair {
    pending: Option<MirrorRepairWork>,
}

struct MirrorRepairWork {
    room_registry: kameo::actor::ActorRef<waddle_xmpp::muc::room_registry_actor::RoomRegistryActor>,
    host_grants: crate::permissions::host_grant::HostGrantGate,
    room: BareJid,
    subjects: Vec<BareJid>,
}

impl MirrorRepair {
    pub(super) fn arm(state: &WebSocketState, room: &BareJid, subjects: Vec<BareJid>) -> Self {
        Self {
            pending: Some(MirrorRepairWork {
                room_registry: state.deps.protocol.room_registry.clone(),
                host_grants: state.deps.app_state.host_grants.clone(),
                room: room.clone(),
                subjects,
            }),
        }
    }

    /// The actor applied the change; nothing is repaired.
    pub(super) fn disarm(mut self) {
        self.pending = None;
    }
}

impl Drop for MirrorRepair {
    fn drop(&mut self) {
        let Some(work) = self.pending.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            for delay in MIRROR_REPAIR_DELAYS {
                tokio::time::sleep(delay).await;
                if work.repair_once().await {
                    return;
                }
            }
            warn!(
                kind = "mirror_repair_unfinished",
                "A managed room actor's affiliation list could not be put back in line with the \
                 canonical grants"
            );
        });
    }
}

/// How many times one repair attempt re-reads the grants after applying
/// them, while other changes keep landing in between.
const MIRROR_REPAIR_READS: usize = 4;

impl MirrorRepairWork {
    /// One attempt; `true` once every subject's entry matches the recorded
    /// grant, or when there is no room actor left to repair.
    ///
    /// The values applied are those of one generation of the room's grants.
    /// A change recorded after that read may already have been applied to
    /// the actor by its own request, so after applying, the grants are read
    /// again: the attempt counts only when the generation has not moved,
    /// and otherwise the values of the newer generation are applied over
    /// the older ones.
    async fn repair_once(&self) -> bool {
        use waddle_xmpp::muc::room_registry_actor::GetRoom;
        let mut grants = match self.host_grants.room(RoomBinding::Room(&self.room)).await {
            RoomGate::Managed(grants) => grants,
            RoomGate::Unmanaged => return true,
            RoomGate::Unavailable => return false,
        };
        let actor = match self
            .room_registry
            .ask(GetRoom {
                room_jid: self.room.clone(),
            })
            .await
        {
            Ok(Some(actor)) => actor,
            // A room actor spawned later hydrates from durable state the
            // committed mutations wrote, not from this attempt.
            Ok(None) => return true,
            Err(_) => return false,
        };
        for _ in 0..MIRROR_REPAIR_READS {
            if !self.apply(&actor, &grants).await {
                return false;
            }
            let current = match self.host_grants.room(RoomBinding::Room(&self.room)).await {
                RoomGate::Managed(current) => current,
                RoomGate::Unmanaged => return true,
                RoomGate::Unavailable => return false,
            };
            if current.grants.generation == grants.grants.generation {
                return true;
            }
            grants = current;
        }
        false
    }

    /// Set each subject's entry in the actor's list to the affiliation
    /// `grants` record. `true` when every entry was set.
    async fn apply(
        &self,
        actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        grants: &ManagedRoomGrants,
    ) -> bool {
        use waddle_xmpp::muc::room_actor::ChangeAffiliation;
        let mut applied = true;
        for subject in &self.subjects {
            let affiliation = muc_affiliation(grants.grants.recorded_affiliation(subject));
            if actor
                .ask(ChangeAffiliation {
                    jid: subject.clone(),
                    affiliation,
                })
                .await
                .is_err()
            {
                applied = false;
            }
        }
        applied
    }
}

/// The canonical grants of the room an administration request names, when
/// an enrolled scope manages it.
pub(super) async fn managed_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> RoomGate<ManagedRoomGrants> {
    state
        .deps
        .app_state
        .host_grants
        .room(RoomBinding::Room(room_jid))
        .await
}

/// Whether the occupants named by `nicks` are all accounts of a domain this
/// deployment serves. The moderator role is room moderation, which a remote
/// principal holds only under ADR-0020 D8's conditions; this boundary does
/// not see them, so the role is granted to served accounts only.
pub(super) async fn occupants_are_served_accounts(
    room_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    room: &ManagedRoomGrants,
    nicks: &[&str],
) -> Result<bool, ()> {
    use crate::permissions::host_grant::SubjectHome;
    let snapshot = room_actor.ask(GetSnapshot).await.map_err(|_| ())?;
    Ok(nicks.iter().all(|nick| {
        snapshot
            .room
            .occupants
            .values()
            .find(|occupant| occupant.nick == *nick)
            .is_some_and(|occupant| {
                room.served.home_of(&occupant.real_jid.to_bare()) == SubjectHome::Served
            })
    }))
}
