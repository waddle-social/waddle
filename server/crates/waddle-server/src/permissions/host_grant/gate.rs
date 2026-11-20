//! The host-grant decision every surface outside room-manifest acceptance
//! consults for a room an enrolled scope manages.
//!
//! [`HostGrantGate`] pairs the canonical store with the replica's enrolled
//! set ([`EnrolledScopes`]) and the [`HostGrantAttachment`] its
//! [`HostGrantAttachmentGate`] holds while it is attached: the attachment
//! the restore fence verified, which the attachment gate replaces when it
//! verifies a changed authority again and drops while the fence refuses.
//! These surfaces do not compare the attachment's binding with the
//! authority they read, as acceptance does, so they read the attachment
//! only while the attachment gate's last verification stands
//! ([`HostGrantAttachmentGate::verified_attachment`]): while it verifies a
//! changed authority again, or after that verification failed, every
//! managed room is unavailable even though the gate still holds the
//! earlier attachment for acceptance to compare with. Whether a room
//! is managed is answered from the enrolled set, in memory: a room outside
//! it answers [`RoomGate::Unmanaged`] without touching PostgreSQL, and the
//! surface keeps its existing checks. A managed room is answered from its
//! canonical grants alone, read from PostgreSQL in one bounded snapshot,
//! with the policy room-manifest admission applies (`policy::grant_allows`)
//! and the replica's [`PrincipalPolicy`], which decides whose grants are
//! honoured (ADR-0020 D1 and D8): legacy tuples, durable room affiliations,
//! PubSub affiliations, Space membership and the room actor's occupancy
//! admit nothing. It answers
//! [`RoomGate::Unavailable`] when the enrolled set has not loaded, when the
//! address reaches a managed room in a form that is not canonical, when the
//! grants cannot be read within the bounds, on a replica whose
//! attachment gate is not attached (its runtime role's privileges or the
//! restore fence not verified, or verified again and refused), and on a
//! replica that holds no lease of the revocation fence: a surface fails
//! closed on it.
//!
//! Every decision for a managed room carries the replica's revocation-fence
//! ticket ([`FenceTicket`]): the registration and the scope generation it
//! was read under. [`ManagedRoomGrants::allows_as`] and
//! [`ManagedRoomGrants::standing_as`], which every decision goes through
//! with the evidence its surface names, answer nothing once the ticket is
//! no longer current, so a decision the replica has acknowledged a newer
//! generation over, or that outlived the replica's lease, admits nothing
//! however long its caller held it (see `fence`).
//!
//! SQLite holds no canonical store, so every room is unmanaged there.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jid::BareJid;
use waddle_xmpp::pubsub::Affiliation;

use super::enrolled::{EnrolledScopes, Enrollment};
use super::fence::{FenceTicket, ReplicaFence};
use super::issuance::IssuanceBasis;
use super::model::{GrantSubject, ScopeKey};
use super::policy::HostGrantAction;
use super::principal::{
    named_evidence, NoSeats, PrincipalEvidence, PrincipalPolicy, PrincipalWitness, SeatEvidence,
    StreamEvidence,
};
use super::rate_limited::RateLimitedWarning;
use super::runtime_attach::{HostGrantAttachment, HostGrantAttachmentGate};
use super::standing::{RoomBinding, RoomGrants, RoomLookup};
use super::store::HostGrantStore;
use crate::db::Database;
use crate::s2s::route::RemotePrincipal;

/// At most one warning a minute that a managed room was refused on a replica
/// without an attachment; only the kind of the refusal is logged.
static UNATTACHED: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that the store could not be read for a
/// decision; only the kind of the failure is logged.
static UNREADABLE: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that an object was refused because the
/// enrolled set has not loaded or the address is not canonical.
static UNDECIDED: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that a managed room was refused on a replica
/// without a lease of the revocation fence.
static UNLEASED: RateLimitedWarning = RateLimitedWarning::new();

/// How many times a managed room's grants are read for one decision: a read
/// the replica installed a newer generation over is made again once.
const ROOM_READS: usize = 2;

/// Longest a managed room's decision waits for its grants, connection
/// checkout included. Each statement is bounded by the interactive bounds
/// (500 ms for a lock, 1 s for a statement); this bounds the whole read.
const ROOM_READ_DEADLINE: Duration = Duration::from_millis(1_500);

/// The canonical store of a replica, its enrolled set and the gate that
/// holds its verified attachment to the store.
#[derive(Clone, Default)]
pub struct HostGrantGate {
    store: Option<HostGrantStore>,
    enrolled: EnrolledScopes,
    attachment: Arc<OnceLock<Arc<HostGrantAttachmentGate>>>,
    /// The replica's part of the revocation fence, on PostgreSQL.
    fence: Option<ReplicaFence>,
    /// Another attachment gate than the bound one was offered: which of
    /// them speaks for the replica cannot be told, so no managed room is
    /// decided.
    conflicting: Arc<AtomicBool>,
}

/// Whether a room or Space is managed by an enrolled scope cannot be told:
/// the enrolled set has not loaded. A caller fails closed on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the host-grant enrolled set is not known")]
pub struct HostGrantUnavailable;

/// A protected Space decision tied to the replica's lease and scope generation.
#[derive(Debug)]
pub struct ManagedSpaceGrant {
    affiliation: Affiliation,
    fence: FenceTicket,
}

impl ManagedSpaceGrant {
    /// Transfer a protected effect synchronously while the decision is current.
    pub fn admit<R>(&self, effect: impl FnOnce(Affiliation) -> R) -> Option<R> {
        self.fence.admit(|| effect(self.affiliation))
    }
}

/// What the gate decided for a room.
#[derive(Clone, Debug)]
pub enum RoomGate<T> {
    /// No enrolled scope manages the room; its existing checks apply.
    Unmanaged,
    /// An enrolled scope manages the room and this is its decision.
    Managed(T),
    /// The room may be managed and nothing can be decided for it: fail
    /// closed.
    Unavailable,
}

/// A managed room's canonical grants together with the replica's
/// [`PrincipalPolicy`] at the decision (the account domains it serves and
/// the S2S snapshot it runs, which decide whose grants are honoured) and the
/// revocation-fence ticket the grants were read under.
#[derive(Clone, Debug)]
pub struct ManagedRoomGrants {
    pub grants: RoomGrants,
    pub principals: PrincipalPolicy,
    /// What the grants were read under. A decision is acted on only while
    /// it is current.
    pub fence: FenceTicket,
}

impl ManagedRoomGrants {
    /// Whether `subject` may perform `action`, decided without stream
    /// evidence, for suites. A surface names the evidence it decides with
    /// ([`Self::allows_as`]), so that a request an S2S stream carried is
    /// never decided as one no stream did.
    #[cfg(feature = "test-support")]
    pub fn allows(&self, subject: &BareJid, action: HostGrantAction) -> bool {
        self.allows_as(subject, &PrincipalEvidence::NoStream, action)
    }

    /// Whether `subject`, whose request reached the surface with
    /// `evidence`, may perform `action`, and the decision may still be acted
    /// on: a decision whose ticket is no longer current allows nothing,
    /// whatever the evidence.
    pub fn allows_as(
        &self,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
        action: HostGrantAction,
    ) -> bool {
        self.fence.is_current()
            && self
                .grants
                .allows(subject, evidence, action, &self.principals)
    }

    /// The standing `subject` is honoured with, decided without stream
    /// evidence, for suites; a surface names its evidence
    /// ([`Self::standing_as`]).
    #[cfg(feature = "test-support")]
    pub fn standing(&self, subject: &BareJid) -> super::model::RoomAffiliation {
        self.standing_as(subject, &PrincipalEvidence::NoStream)
    }

    /// The standing `subject`, whose request reached the surface with
    /// `evidence`, is honoured with; `None` once the decision's ticket is no
    /// longer current, whatever the evidence.
    pub fn standing_as(
        &self,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
    ) -> super::model::RoomAffiliation {
        if !self.fence.is_current() {
            return super::model::RoomAffiliation::None;
        }
        self.grants.standing(subject, evidence, &self.principals)
    }

    /// The subjects whose grants allow `action`, each decided without
    /// stream evidence.
    pub fn admitted(&self, action: HostGrantAction) -> Vec<BareJid> {
        self.grants.admitted(action, &self.principals)
    }

    /// What `subject`, reaching the surface with `evidence`, is decided as.
    pub fn witness(&self, subject: &BareJid, evidence: &PrincipalEvidence) -> PrincipalWitness {
        self.principals.witness(subject, evidence)
    }
}

/// A managed room's canonical grants as the store holds them, read without
/// the replica's lease: what [`HostGrantGate::room_as_stored`] returns.
///
/// It carries no ticket of the revocation fence and is not a decision. It
/// answers what the grants say, for the caller that puts derived state back
/// in line with them (the revocation sweep removes an occupant, lowers an
/// entry). It offers no `allows_as` and is no [`ManagedRoomGrants`], so no
/// surface can admit a request from it.
#[derive(Clone, Debug)]
pub struct StoredRoomGrants {
    grants: RoomGrants,
    principals: PrincipalPolicy,
}

impl StoredRoomGrants {
    /// Whether the scope admits anybody: it is active under the current
    /// authority incarnation and policy version.
    pub fn scope_admits(&self) -> bool {
        self.grants.scope_admits
    }

    /// The standing the grants honour `subject` with on `evidence`.
    pub fn standing(
        &self,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
    ) -> super::model::RoomAffiliation {
        self.grants.standing(subject, evidence, &self.principals)
    }

    /// Whether the grants would let `subject` read the room on `evidence`.
    pub fn reads(&self, subject: &BareJid, evidence: &PrincipalEvidence) -> bool {
        self.grants
            .allows(subject, evidence, HostGrantAction::Read, &self.principals)
    }

    /// The affiliation recorded for `subject`, effective or not yet.
    pub fn recorded_affiliation(&self, subject: &BareJid) -> super::model::RoomAffiliation {
        self.grants.recorded_affiliation(subject)
    }

    /// The affiliation in effect for `subject`: what admission honours.
    pub fn effective_affiliation(&self, subject: &BareJid) -> super::model::RoomAffiliation {
        self.grants.effective_affiliation(subject)
    }

    /// The subjects whose recorded affiliation is `affiliation`.
    pub fn recorded(&self, affiliation: super::model::RoomAffiliation) -> Vec<BareJid> {
        self.grants.recorded(affiliation)
    }

    /// What `subject` is decided as on `evidence`, under the snapshot the
    /// replica runs.
    pub fn witness(&self, subject: &BareJid, evidence: &PrincipalEvidence) -> PrincipalWitness {
        self.principals.witness(subject, evidence)
    }
}

impl HostGrantGate {
    /// The gate of a replica whose global database is `db`, with an enrolled
    /// set of its own. A database that is not PostgreSQL has no canonical
    /// store, and the gate then answers [`RoomGate::Unmanaged`] for every
    /// room.
    pub fn new(db: Database) -> Self {
        match HostGrantStore::open(db) {
            Ok(store) => Self::with_enrolled_scopes(store.clone(), EnrolledScopes::watch(store)),
            Err(_) => Self::default(),
        }
    }

    /// The gate of a replica whose canonical store is `store`, sharing the
    /// enrolled set `enrolled` with the permission actor.
    pub fn with_enrolled_scopes(store: HostGrantStore, enrolled: EnrolledScopes) -> Self {
        let attachment = Arc::default();
        let conflicting = Arc::default();
        let fence = ReplicaFence::new(
            store.clone(),
            enrolled.clone(),
            Arc::clone(&attachment),
            Arc::clone(&conflicting),
        );
        Self {
            store: Some(store),
            enrolled,
            attachment,
            fence: Some(fence),
            conflicting,
        }
    }

    /// Decide managed rooms with the attachment `gate` holds: the replica's
    /// attachment gate, shared with room-manifest acceptance. Only the first
    /// gate bound is kept; the answer is whether this call bound it. A
    /// different gate offered after one is bound leaves this gate refusing
    /// every managed room, since it cannot tell which of them speaks for
    /// the replica; offering the bound gate again changes nothing.
    pub fn bind_attachment(&self, gate: Arc<HostGrantAttachmentGate>) -> bool {
        match self.attachment.set(gate) {
            Ok(()) => true,
            Err(offered) => {
                let bound = self.attachment.get();
                if !bound.is_some_and(|bound| Arc::ptr_eq(bound, &offered)) {
                    self.conflicting.store(true, Ordering::Release);
                }
                false
            }
        }
    }

    /// Whether a gate other than the bound one was offered to
    /// [`Self::bind_attachment`], which leaves this gate refusing every
    /// managed room.
    pub fn attachment_conflicts(&self) -> bool {
        self.conflicting.load(Ordering::Acquire)
    }

    /// Hold `attachment` in the bound attachment gate, binding a gate of
    /// this gate's own when none is bound. The first attachment stays; the
    /// answer is whether this call set it. For suites that decide rooms
    /// without a verifier; a replica's gate is attached only by its
    /// verification.
    #[cfg(feature = "test-support")]
    pub fn attach(&self, attachment: HostGrantAttachment) -> bool {
        self.attachment.get_or_init(Arc::default).attach(attachment)
    }

    /// The canonical store, on PostgreSQL.
    pub fn store(&self) -> Option<&HostGrantStore> {
        self.store.as_ref()
    }

    /// The replica's enrolled set.
    pub fn enrolled(&self) -> &EnrolledScopes {
        &self.enrolled
    }

    /// The replica's part of the revocation fence, on PostgreSQL.
    pub fn fence(&self) -> Option<&ReplicaFence> {
        self.fence.as_ref()
    }

    /// The verified attachment the bound attachment gate holds now, only
    /// while that gate is attached
    /// ([`HostGrantAttachmentGate::verified_attachment`]). `None` while it
    /// verifies a changed authority again, after that verification failed,
    /// before it first attaches, when no gate is bound and when another
    /// gate was offered than the bound one.
    pub fn attachment(&self) -> Option<HostGrantAttachment> {
        if self.conflicting.load(Ordering::Acquire) {
            return None;
        }
        self.attachment
            .get()
            .and_then(|gate| gate.verified_attachment())
    }

    /// The evidence a decision that names `account` in `room` without its
    /// request decides it on: that of each seat this replica holds for it
    /// there, or no stream when it holds none
    /// ([`super::principal::named_evidence`]).
    pub fn named_evidence(&self, room: &BareJid, account: &BareJid) -> Vec<PrincipalEvidence> {
        match self.attachment.get() {
            Some(gate) => named_evidence(gate.seat_evidence(), room, account),
            None => named_evidence(&NoSeats, room, account),
        }
    }

    /// The stream evidence of every seat this replica holds for `account`
    /// in `room`; empty when it seated none over an S2S stream.
    pub fn seat_evidence(&self, room: &BareJid, account: &BareJid) -> Vec<StreamEvidence> {
        self.attachment
            .get()
            .map(|gate| gate.seat_evidence().of_account(room, account))
            .unwrap_or_default()
    }

    /// Hold the stream evidence of the remote occupants this replica seats
    /// in the bound attachment gate. `false` when no gate is bound or one
    /// source is already held.
    pub fn attach_seat_evidence(&self, seats: Arc<dyn SeatEvidence>) -> bool {
        self.attachment
            .get()
            .is_some_and(|gate| gate.attach_seat_evidence(seats))
    }

    /// Whether an enrolled scope manages the room `binding` names, from the
    /// enrolled set alone.
    pub async fn room_enrollment(&self, binding: RoomBinding<'_>) -> Enrollment {
        if self.store.is_none() {
            return Enrollment::Unmanaged;
        }
        let enrollment = match binding {
            RoomBinding::Room(room) => self.enrolled.settled_room(room).await,
            RoomBinding::Channel(channel) => self.enrolled.settled_channel(channel).await,
        };
        if enrollment == Enrollment::Undecided && UNDECIDED.due() {
            tracing::warn!(
                kind = "enrollment_undecided",
                "A room was refused: the host-grant enrolled set has not loaded, or the address \
                 is not canonical"
            );
        }
        enrollment
    }

    /// The canonical grants of the room `binding` names.
    pub async fn room(&self, binding: RoomBinding<'_>) -> RoomGate<ManagedRoomGrants> {
        let Some(store) = &self.store else {
            return RoomGate::Unmanaged;
        };
        match self.room_enrollment(binding).await {
            Enrollment::Unmanaged => return RoomGate::Unmanaged,
            Enrollment::Undecided => return RoomGate::Unavailable,
            Enrollment::Enrolled => {}
        }
        let Some(attachment) = self.attachment() else {
            if UNATTACHED.due() {
                tracing::warn!(
                    kind = "authority_unavailable",
                    "A managed room was refused: this replica holds no verified host-grant \
                     attachment (runtime role privileges or restore fence, or the authority is \
                     being verified again)"
                );
            }
            return RoomGate::Unavailable;
        };
        let Some(fence) = &self.fence else {
            return RoomGate::Unavailable;
        };
        for _ in 0..ROOM_READS {
            // The registration comes first: the grants are read by a
            // replica that holds its lease.
            let Some(registration) = fence.begin_decision().await else {
                if UNLEASED.due() {
                    tracing::warn!(
                        kind = "fence_unleased",
                        "A managed room was refused: this replica holds no lease of the \
                         host-grant revocation fence"
                    );
                }
                return RoomGate::Unavailable;
            };
            match tokio::time::timeout(ROOM_READ_DEADLINE, store.room_grants(binding)).await {
                Ok(Ok(RoomLookup::Managed(grants))) => {
                    let ticket = fence.ticket(registration, grants.lifecycle, grants.generation);
                    // A read the replica installed a newer generation over
                    // is not a decision; it is made again.
                    if ticket.is_current() {
                        return RoomGate::Managed(ManagedRoomGrants {
                            grants,
                            principals: attachment.principals().clone(),
                            fence: ticket,
                        });
                    }
                }
                // Room bindings are immutable, so a binding the enrolled set
                // holds and the store does not is not a decision either.
                Ok(Ok(RoomLookup::Unmanaged | RoomLookup::NotCanonical)) => {
                    if UNREADABLE.due() {
                        tracing::warn!(
                            kind = "binding_missing",
                            "A managed room was refused: its binding could not be read"
                        );
                    }
                    return RoomGate::Unavailable;
                }
                Ok(Err(error)) => {
                    if UNREADABLE.due() {
                        tracing::warn!(
                            kind = error.kind(),
                            "The host-grant store could not be read for a room decision"
                        );
                    }
                    return RoomGate::Unavailable;
                }
                Err(_) => {
                    if UNREADABLE.due() {
                        tracing::warn!(
                            kind = "deadline",
                            "The host-grant store could not be read for a room decision"
                        );
                    }
                    return RoomGate::Unavailable;
                }
            }
        }
        if UNREADABLE.due() {
            tracing::warn!(
                kind = "overtaken",
                "A managed room was refused: its grants kept changing while they were read"
            );
        }
        RoomGate::Unavailable
    }

    /// The canonical grants of the room `binding` names as the store holds
    /// them now, read without the replica's lease and carrying no ticket of
    /// its fence. It is for the caller that puts derived state back in line
    /// with the grants: what such a caller does on a read that shows no
    /// standing (it removes an occupant, lowers a role) needs no admission,
    /// and it must keep working while the replica's lease is lapsed.
    /// Nothing can be admitted from it: [`StoredRoomGrants`] is not a
    /// decision and carries no fence ticket.
    pub async fn room_as_stored(&self, binding: RoomBinding<'_>) -> RoomGate<StoredRoomGrants> {
        let Some(store) = &self.store else {
            return RoomGate::Unmanaged;
        };
        match self.room_enrollment(binding).await {
            Enrollment::Unmanaged => return RoomGate::Unmanaged,
            Enrollment::Undecided => return RoomGate::Unavailable,
            Enrollment::Enrolled => {}
        }
        let Some(attachment) = self.attachment() else {
            return RoomGate::Unavailable;
        };
        match tokio::time::timeout(ROOM_READ_DEADLINE, store.room_grants(binding)).await {
            Ok(Ok(RoomLookup::Managed(grants))) => RoomGate::Managed(StoredRoomGrants {
                grants,
                principals: attachment.principals().clone(),
            }),
            _ => RoomGate::Unavailable,
        }
    }

    /// Ask the replica's revocation sweep for the scope that manages
    /// `room`, if one does: a caller that changed what a room actor holds
    /// from a decision it made earlier (seats restored after a claim move)
    /// has the result put in line with the grants as they stand now.
    pub fn request_sweep_of(&self, room: &BareJid) {
        let (Some(fence), Some(enrolled)) = (&self.fence, self.enrolled.snapshot()) else {
            return;
        };
        if let Some(lifecycle) = enrolled.room(room.as_str()) {
            fence.request_sweep(lifecycle);
        }
    }

    /// This gate without its part of the revocation fence: the same store,
    /// enrolled set and attachment. Its [`Self::room`] decides nothing; a
    /// task the fence itself drives holds it, so that the task does not keep
    /// the fence alive.
    pub fn detached(&self) -> Self {
        Self {
            store: self.store.clone(),
            enrolled: self.enrolled.clone(),
            attachment: Arc::clone(&self.attachment),
            fence: None,
            conflicting: Arc::clone(&self.conflicting),
        }
    }

    /// Whether `subject` may perform `action` in the room `binding` names,
    /// decided without stream evidence, for suites; a surface names its
    /// evidence ([`Self::allows_as`]).
    #[cfg(feature = "test-support")]
    pub async fn allows(
        &self,
        binding: RoomBinding<'_>,
        subject: &BareJid,
        action: HostGrantAction,
    ) -> RoomGate<bool> {
        self.allows_as(binding, subject, &PrincipalEvidence::NoStream, action)
            .await
    }

    /// Whether `subject`, whose request reached the surface with
    /// `evidence`, may perform `action` in the room `binding` names.
    pub async fn allows_as(
        &self,
        binding: RoomBinding<'_>,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
        action: HostGrantAction,
    ) -> RoomGate<bool> {
        match self.room(binding).await {
            RoomGate::Unmanaged => RoomGate::Unmanaged,
            RoomGate::Unavailable => RoomGate::Unavailable,
            RoomGate::Managed(room) => RoomGate::Managed(room.allows_as(subject, evidence, action)),
        }
    }

    /// Whether `subject`, whose request reached the surface with
    /// `evidence`, may still perform `action` in the room `decided` was read
    /// for, asked once an await separated that decision from the effect it
    /// guards: the decision itself while its ticket is current, otherwise a
    /// decision read now. A room that cannot be decided now allows nothing.
    pub async fn still_allows(
        &self,
        decided: &ManagedRoomGrants,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
        action: HostGrantAction,
    ) -> bool {
        if decided.fence.is_current() {
            return decided.allows_as(subject, evidence, action);
        }
        match self.room(RoomBinding::Room(&decided.grants.room)).await {
            RoomGate::Managed(now) => now.allows_as(subject, evidence, action),
            RoomGate::Unmanaged | RoomGate::Unavailable => false,
        }
    }

    /// Whether the room whose authorization object is `channel_id` is
    /// managed by an enrolled scope. `Err` while that cannot be told.
    pub async fn channel_enrolled(&self, channel_id: &str) -> Result<bool, HostGrantUnavailable> {
        enrolled(self.room_enrollment(RoomBinding::Channel(channel_id)).await)
    }

    /// Which of `channel_ids` belong to rooms enrolled scopes manage.
    pub async fn enrolled_channels(
        &self,
        channel_ids: &[&str],
    ) -> Result<BTreeSet<String>, HostGrantUnavailable> {
        if self.store.is_none() || channel_ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let Some(set) = self.enrolled.settled_snapshot().await else {
            return Err(HostGrantUnavailable);
        };
        Ok(channel_ids
            .iter()
            .filter(|channel| set.channel(channel).is_some())
            .map(|channel| (*channel).to_owned())
            .collect())
    }

    /// Authorize a read for an authenticated local account.
    pub async fn space_read_local(
        &self,
        key: &ScopeKey,
        subject: &GrantSubject,
    ) -> Option<ManagedSpaceGrant> {
        self.space_read(key, subject, &PrincipalEvidence::NoStream)
            .await
    }

    /// Authorize a read for the account carried by the authenticated S2S stream.
    pub async fn space_read_remote(
        &self,
        key: &ScopeKey,
        principal: &RemotePrincipal,
    ) -> Option<ManagedSpaceGrant> {
        let subject = GrantSubject::new(principal.account().as_bare_jid().clone()).ok()?;
        self.space_read(key, &subject, &PrincipalEvidence::of(principal))
            .await
    }

    async fn space_read(
        &self,
        key: &ScopeKey,
        subject: &GrantSubject,
        evidence: &PrincipalEvidence,
    ) -> Option<ManagedSpaceGrant> {
        let store = self.store.as_ref()?;
        let attachment = self.attachment()?;
        let fence = self.fence.as_ref()?;
        let registration = fence.begin_decision().await?;
        let standing = tokio::time::timeout(
            ROOM_READ_DEADLINE,
            store.space_standing(key, subject, attachment.binding()),
        )
        .await
        .ok()?
        .ok()??;
        let witness = attachment.principals().witness(subject.jid(), evidence);
        let valid_origin = matches!(
            (&witness, evidence),
            (PrincipalWitness::Served, PrincipalEvidence::NoStream)
                | (PrincipalWitness::Remote(_), PrincipalEvidence::Stream(_))
        );
        if !valid_origin
            || standing.recorded == Affiliation::Outcast
            || (matches!(&witness, PrincipalWitness::Remote(_)) && !standing.federated)
        {
            return None;
        }
        let affiliation = match standing.effective {
            Affiliation::Member if witness.grants_honoured() => Affiliation::Member,
            affiliation @ (Affiliation::Owner | Affiliation::Publisher)
                if witness.privilege_honoured()
                    && matches!(
                        (&witness, standing.issuance_basis),
                        (PrincipalWitness::Served, IssuanceBasis::Local)
                            | (PrincipalWitness::Remote(_), IssuanceBasis::TrustedHome)
                    ) =>
            {
                affiliation
            }
            _ => return None,
        };
        let ticket = fence.ticket(registration, standing.lifecycle, standing.generation);
        ticket.is_current().then_some(ManagedSpaceGrant {
            affiliation,
            fence: ticket,
        })
    }

    /// Whether `node` is the Space node of an enrolled scope. `Err` while
    /// that cannot be told.
    pub async fn space_enrolled(&self, node: &str) -> Result<bool, HostGrantUnavailable> {
        if self.store.is_none() {
            return Ok(false);
        }
        enrolled(self.enrolled.settled_space_node(node).await)
    }
}

fn enrolled(enrollment: Enrollment) -> Result<bool, HostGrantUnavailable> {
    match enrollment {
        Enrollment::Unmanaged => Ok(false),
        Enrollment::Enrolled => Ok(true),
        Enrollment::Undecided => Err(HostGrantUnavailable),
    }
}
