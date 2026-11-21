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

use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::pubsub::Affiliation;
use waddle_xmpp::stream_management::persistence::SmReplayPayload;
use waddle_xmpp::stream_management::{
    ProtectedSpaceReplyDescriptor, ProtectedSpaceReplyKind, ProtectedSpaceRoomDependency,
};
use xmpp_parsers::iq::Iq;

use super::enrolled::{EnrolledScopes, Enrollment};
use super::fence::{FenceTicket, ReplicaFence};
use super::issuance::IssuanceBasis;
use super::model::{
    GrantOperationId, GrantSubject, OperationRecord, ScopeKey, SpaceBookmarkVersion,
};
use super::policy::HostGrantAction;
use super::principal::{
    named_evidence, NoSeats, PrincipalEvidence, PrincipalPolicy, PrincipalWitness, SeatEvidence,
    StreamEvidence,
};
use super::rate_limited::RateLimitedWarning;
use super::runtime_attach::{HostGrantAttachment, HostGrantAttachmentGate};
use super::standing::{RoomBinding, RoomGrants, RoomLookup};
use super::store::HostGrantStore;
use crate::db::local_space_subscriptions::{LocalSpaceSubscription, LocalSpaceSubscriptions};
use crate::db::s2s_spaces::{SpaceSubscription, SpaceSubscriptionGrant, SpaceSubscriptions};
use crate::db::Database;
use crate::s2s::conn::AuthenticationStrength;
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
    remote_effect: Option<RemoteSpaceEffect>,
    room_dependencies: Vec<FenceTicket>,
    subscription_grant: SpaceSubscriptionGrant,
}

/// An exact protected reply and its current delivery authority.
#[derive(Debug)]
pub(crate) struct LocalSpaceReplyGrant {
    authority: LocalSpaceReplyAuthority,
    replay: ProtectedSpaceReplyDescriptor,
    preference: Option<LocalReplyPreference>,
}

#[derive(Debug)]
enum LocalSpaceReplyAuthority {
    Active(ManagedSpaceGrant),
    Deleted(LocalDeletionDelivery),
}

struct LocalDeletionDelivery {
    fence: FenceTicket,
    store: HostGrantStore,
}

impl std::fmt::Debug for LocalDeletionDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalDeletionDelivery")
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

struct LocalReplyPreference {
    store: LocalSpaceSubscriptions,
    key: ScopeKey,
    subscription: LocalSpaceSubscription,
}

impl std::fmt::Debug for LocalReplyPreference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalReplyPreference")
            .finish_non_exhaustive()
    }
}

impl LocalSpaceReplyGrant {
    pub(crate) fn admit<R>(&self, effect: impl FnOnce() -> R) -> Option<R> {
        match &self.authority {
            LocalSpaceReplyAuthority::Active(grant) => grant.admit(|_| effect()),
            LocalSpaceReplyAuthority::Deleted(deletion) => deletion.fence.admit(effect),
        }
    }

    pub(crate) fn replay_identity(&self) -> &ProtectedSpaceReplyDescriptor {
        &self.replay
    }

    pub(crate) fn bookmark_has_payload(&self, payload: &SmReplayPayload) -> bool {
        self.replay.bookmark_event().is_some()
            && bookmark_item_has_payload(&payload.as_ref().to_element())
    }

    pub(crate) fn needs_delivery_hold(&self) -> bool {
        self.preference.is_some() || matches!(&self.authority, LocalSpaceReplyAuthority::Deleted(_))
    }

    pub(crate) async fn hold_delivery(
        &self,
        has_bookmark_payload: bool,
    ) -> Option<crate::db::Transaction<'_>> {
        tokio::time::timeout(ROOM_READ_DEADLINE, async {
            if let LocalSpaceReplyAuthority::Deleted(deletion) = &self.authority {
                let key = ScopeKey::new(self.replay.service().clone(), self.replay.node()).ok()?;
                let subject = GrantSubject::new(self.replay.recipient().to_bare()).ok()?;
                let operation = GrantOperationId::from_uuid(self.replay.deletion_operation()?);
                return deletion
                    .store
                    .hold_local_space_deletion_recipient(
                        &key,
                        operation,
                        &subject,
                        deletion.fence.generation(),
                    )
                    .await
                    .ok()?;
            }
            let preference = self.preference.as_ref()?;
            if let Some(event) = self.replay.bookmark_event() {
                let room = self.replay.room_dependencies().first()?.room().clone();
                let version = SpaceBookmarkVersion {
                    lifecycle: preference.subscription.grant.lifecycle,
                    incarnation: preference.subscription.grant.incarnation,
                    room,
                    operation: GrantOperationId::from_uuid(event.operation()),
                    present: event.present(),
                };
                preference
                    .store
                    .hold_delivery(
                        &preference.key,
                        &preference.subscription,
                        &version,
                        has_bookmark_payload,
                    )
                    .await
                    .ok()?
            } else {
                preference
                    .store
                    .hold_preference(&preference.key, &preference.subscription)
                    .await
                    .ok()?
            }
        })
        .await
        .ok()?
    }

    pub(crate) fn with_room_dependencies(
        mut self,
        rooms: Vec<(ProtectedSpaceRoomDependency, ManagedRoomGrants)>,
    ) -> Option<Self> {
        let (dependencies, decisions): (Vec<_>, Vec<_>) = rooms.into_iter().unzip();
        self.replay = self.replay.with_room_dependencies(dependencies).ok()?;
        let LocalSpaceReplyAuthority::Active(grant) = &mut self.authority else {
            return None;
        };
        for decision in decisions {
            grant.require_room(decision);
        }
        Some(self)
    }
}

/// An exact legacy Space disco reply whose handoff still depends on the
/// replica classifying the node as unmanaged.
#[derive(Clone, Debug)]
pub(crate) struct UnmanagedSpaceReplyGuard {
    enrolled: EnrolledScopes,
    service: BareJid,
    nodes: Vec<String>,
    recipient: FullJid,
    id: String,
    exact_payload: Option<minidom::Element>,
}

impl UnmanagedSpaceReplyGuard {
    pub(crate) fn permits_reply(&self, stanza: &minidom::Element, recipient: &FullJid) -> bool {
        recipient == &self.recipient
            && stanza.is("iq", "jabber:client")
            && stanza.attr("type") == Some("result")
            && stanza.attr("from") == Some(self.service.as_str())
            // Older replies addressed only the authenticated socket when a
            // client omitted `from`. Explicit addressing must match it.
            && stanza.attr("to").is_none_or(|to| to == recipient.as_str())
            && stanza.attr("id") == Some(self.id.as_str())
            && stanza.children().count() == 1
            && match &self.exact_payload {
                Some(payload) => stanza.children().next() == Some(payload),
                None => stanza
                    .get_child("query", "http://jabber.org/protocol/disco#info")
                    .is_some_and(|query| query.attr("node") == self.nodes.first().map(String::as_str)),
            }
    }

    pub(crate) fn admit<R>(&self, effect: impl FnOnce() -> R) -> Option<R> {
        self.enrolled
            .admit_unmanaged_spaces(self.nodes.iter().map(String::as_str), effect)
    }
}

fn is_local_space_disco_request(
    service: &BareJid,
    node: &str,
    requester: &FullJid,
    request: &Iq,
) -> bool {
    let Iq::Get { payload, .. } = request else {
        return false;
    };
    let query = xmpp_parsers::disco::DiscoInfoQuery::try_from(payload.clone());
    query.is_ok_and(|query| query.node.as_deref() == Some(node))
        && request.to().map(Jid::as_str) == Some(service.as_str())
        && request
            .from()
            .is_none_or(|from| from.as_str() == requester.as_str())
}

fn bookmark_item_has_payload(stanza: &minidom::Element) -> bool {
    use waddle_xmpp::pubsub::NS_PUBSUB_EVENT;
    stanza
        .get_child("event", NS_PUBSUB_EVENT)
        .and_then(|event| event.get_child("items", NS_PUBSUB_EVENT))
        .and_then(|items| items.get_child("item", NS_PUBSUB_EVENT))
        .is_some_and(|item| item.children().next().is_some())
}

#[derive(Debug)]
enum RemoteSpaceEffect {
    Reply {
        service: BareJid,
        recipient: Jid,
        id: String,
    },
    Event {
        key: ScopeKey,
        recipient: BareJid,
        subscribed_with: AuthenticationStrength,
        subscriptions: Box<SpaceSubscriptions>,
        bookmark_version: Option<super::SpaceBookmarkVersion>,
    },
}

impl ManagedSpaceGrant {
    /// Check the authorized addresses, stanza shape, and stream assurance.
    pub(crate) fn permits_remote_effect(
        &self,
        stanza: &minidom::Element,
        strength: AuthenticationStrength,
    ) -> bool {
        if self.affiliation != Affiliation::Member && strength != AuthenticationStrength::Pkix {
            return false;
        }
        match self.remote_effect.as_ref() {
            Some(RemoteSpaceEffect::Reply {
                service,
                recipient,
                id,
            }) => {
                stanza.name() == "iq"
                    && stanza.attr("type") == Some("result")
                    && stanza.attr("from") == Some(service.as_str())
                    && stanza.attr("to") == Some(recipient.as_str())
                    && stanza.attr("id") == Some(id.as_str())
            }
            Some(RemoteSpaceEffect::Event {
                key,
                recipient,
                subscribed_with,
                bookmark_version,
                ..
            }) => {
                (*subscribed_with != AuthenticationStrength::Pkix
                    || strength == AuthenticationStrength::Pkix)
                    && stanza.name() == "message"
                    && stanza.attr("from") == Some(key.service().as_str())
                    && stanza.attr("to") == Some(recipient.as_str())
                    && stanza
                        .get_child("event", waddle_xmpp::pubsub::NS_PUBSUB_EVENT)
                        .and_then(|event| {
                            event.get_child("items", waddle_xmpp::pubsub::NS_PUBSUB_EVENT)
                        })
                        .is_some_and(|items| {
                            items.attr("node") == Some(key.node())
                                && bookmark_version.as_ref().is_some_and(|version| {
                                    items.children().count() == 1
                                        && items.children().next().is_some_and(|entry| {
                                            entry.is(
                                                if version.present { "item" } else { "retract" },
                                                waddle_xmpp::pubsub::NS_PUBSUB_EVENT,
                                            ) && entry.attr("id") == Some(version.room.as_str())
                                        })
                                })
                        })
            }
            None => false,
        }
    }

    pub(crate) fn subscription_grant(&self) -> SpaceSubscriptionGrant {
        self.subscription_grant
    }

    pub(crate) fn bind_bookmark_version(&mut self, version: &super::SpaceBookmarkVersion) -> bool {
        let Some(RemoteSpaceEffect::Event {
            bookmark_version, ..
        }) = &mut self.remote_effect
        else {
            return false;
        };
        if version.lifecycle != self.subscription_grant.lifecycle
            || version.incarnation != self.subscription_grant.incarnation
        {
            return false;
        }
        *bookmark_version = Some(version.clone());
        true
    }

    /// Transfer a protected effect synchronously while the decision is current.
    pub fn admit<R>(&self, effect: impl FnOnce(Affiliation) -> R) -> Option<R> {
        self.fence
            .admit_with(&self.room_dependencies, || effect(self.affiliation))
    }

    /// Stage a remote effect. For an event, a self-unsubscribe must wait
    /// until the TLS session owns the bytes or staging has been refused.
    pub(crate) fn remote_bookmark_has_payload(&self, stanza: &minidom::Element) -> bool {
        matches!(self.remote_effect, Some(RemoteSpaceEffect::Event { .. }))
            && bookmark_item_has_payload(stanza)
    }

    pub(crate) async fn admit_remote<R>(
        &self,
        has_bookmark_payload: bool,
        effect: impl FnOnce() -> R,
    ) -> Option<R> {
        let Some(RemoteSpaceEffect::Event {
            key,
            recipient,
            subscriptions,
            bookmark_version,
            ..
        }) = &self.remote_effect
        else {
            return self.admit(|_| effect());
        };
        let hold = tokio::time::timeout(
            ROOM_READ_DEADLINE,
            subscriptions.hold_protected_delivery(
                key.service(),
                key.node(),
                recipient,
                self.subscription_grant,
                bookmark_version.as_ref()?,
                has_bookmark_payload,
            ),
        )
        .await
        .ok()?
        .ok()??;
        let staged = self.admit(|_| effect());
        let _ = hold.rollback().await;
        staged
    }

    /// The reply includes data from a managed room as well as the Space.
    pub(crate) fn require_room(&mut self, room: ManagedRoomGrants) {
        self.room_dependencies.push(room.fence);
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

    /// Authorize the exact read reply for a bound local session.
    pub(crate) async fn space_reply_local(
        &self,
        key: &ScopeKey,
        requester: &FullJid,
        request: &Iq,
    ) -> Option<LocalSpaceReplyGrant> {
        let Iq::Get { payload, .. } = request else {
            return None;
        };
        if request.to().map(Jid::as_str) != Some(key.service().as_str())
            || request
                .from()
                .is_some_and(|from| from.as_str() != requester.as_str())
        {
            return None;
        }
        let kind = if xmpp_parsers::disco::DiscoInfoQuery::try_from(payload.clone())
            .is_ok_and(|query| query.node.as_deref() == Some(key.node()))
        {
            ProtectedSpaceReplyKind::DiscoInfo
        } else if xmpp_parsers::disco::DiscoItemsQuery::try_from(payload.clone())
            .is_ok_and(|query| query.node.as_deref() == Some(key.node()))
        {
            ProtectedSpaceReplyKind::DiscoItems
        } else if payload.is("pubsub", waddle_xmpp::pubsub::NS_PUBSUB)
            && matches!(
                waddle_xmpp::pubsub::parse_pubsub_iq(request),
                Ok(waddle_xmpp::pubsub::PubSubRequest::Items { node, .. }) if node == key.node()
            )
        {
            ProtectedSpaceReplyKind::PubSubItems
        } else if payload.is("pubsub", waddle_xmpp::pubsub::NS_PUBSUB_OWNER)
            && payload.attrs().is_empty()
            && payload.text().trim().is_empty()
            && payload.children().count() == 1
        {
            let operation = payload.children().next()?;
            if operation.ns() != waddle_xmpp::pubsub::NS_PUBSUB_OWNER
                || operation.attr("node") != Some(key.node())
                || operation.attrs().len() != 1
                || operation.children().next().is_some()
                || !operation.text().trim().is_empty()
            {
                return None;
            }
            match operation.name() {
                "affiliations" => ProtectedSpaceReplyKind::OwnerAffiliations,
                "configure" => ProtectedSpaceReplyKind::OwnerConfiguration,
                _ => return None,
            }
        } else {
            return None;
        };
        let subject = GrantSubject::new(requester.to_bare()).ok()?;
        let grant = self.space_read_local(key, &subject).await?;
        if matches!(
            kind,
            ProtectedSpaceReplyKind::OwnerAffiliations
                | ProtectedSpaceReplyKind::OwnerConfiguration
        ) && grant.admit(|affiliation| affiliation == Affiliation::Owner) != Some(true)
        {
            return None;
        }
        let replay = ProtectedSpaceReplyDescriptor::new(
            key.service().clone(),
            key.node().to_owned(),
            requester.clone(),
            request.id().to_owned(),
            grant.fence.lifecycle().as_uuid(),
            grant.subscription_grant.incarnation.as_uuid(),
            grant.fence.generation().value(),
        )
        .ok()?
        .with_kind(kind);
        Some(LocalSpaceReplyGrant {
            authority: LocalSpaceReplyAuthority::Active(grant),
            replay,
            preference: None,
        })
    }

    pub(crate) async fn local_space_affiliations_reply(
        &self,
        key: &ScopeKey,
        requester: &FullJid,
        request: &Iq,
    ) -> Option<(LocalSpaceReplyGrant, super::SpaceAffiliationListing)> {
        let reply = self.space_reply_local(key, requester, request).await?;
        if reply.replay.kind() != ProtectedSpaceReplyKind::OwnerAffiliations {
            return None;
        }
        let subject = GrantSubject::new(requester.to_bare()).ok()?;
        let attachment = self.attachment()?;
        let listing = tokio::time::timeout(
            ROOM_READ_DEADLINE,
            self.store
                .as_ref()?
                .space_affiliations(key, &subject, attachment.binding()),
        )
        .await
        .ok()?
        .ok()??;
        (listing.lifecycle.as_uuid() == reply.replay.lifecycle()
            && listing.generation.value() == reply.replay.generation()
            && attachment.binding().incarnation.as_uuid() == reply.replay.incarnation())
        .then_some((reply, listing))
    }

    pub(crate) async fn local_space_affiliation_refusals_reply(
        &self,
        key: &ScopeKey,
        requester: &FullJid,
        request: &Iq,
        lifecycle: super::ScopeLifecycleId,
        incarnation: super::AuthorityIncarnation,
        generation: super::GrantGeneration,
    ) -> Option<LocalSpaceReplyGrant> {
        if !matches!(request, Iq::Set { .. })
            || request.to().map(Jid::as_str) != Some(key.service().as_str())
            || request
                .from()
                .is_some_and(|from| from.as_str() != requester.as_str())
        {
            return None;
        }
        let subject = GrantSubject::new(requester.to_bare()).ok()?;
        let grant = self.space_read_local(key, &subject).await?;
        if grant.fence.lifecycle() != lifecycle
            || grant.subscription_grant.incarnation != incarnation
            || grant.fence.generation() != generation
            || grant.admit(|affiliation| affiliation == Affiliation::Owner) != Some(true)
        {
            return None;
        }
        let replay = ProtectedSpaceReplyDescriptor::new(
            key.service().clone(),
            key.node().to_owned(),
            requester.clone(),
            request.id().to_owned(),
            lifecycle.as_uuid(),
            incarnation.as_uuid(),
            generation.value(),
        )
        .ok()?
        .with_kind(ProtectedSpaceReplyKind::OwnerAffiliationRefusals);
        Some(LocalSpaceReplyGrant {
            authority: LocalSpaceReplyAuthority::Active(grant),
            replay,
            preference: None,
        })
    }

    pub(crate) async fn local_space_subscribe_reply(
        &self,
        key: &ScopeKey,
        requester: &FullJid,
        request: &Iq,
        subscription: LocalSpaceSubscription,
    ) -> Option<LocalSpaceReplyGrant> {
        if !matches!(request, Iq::Set { .. })
            || request.to().map(Jid::as_str) != Some(key.service().as_str())
            || subscription.account.jid() != &requester.to_bare()
            || !matches!(
                waddle_xmpp::pubsub::parse_pubsub_iq(request),
                Ok(waddle_xmpp::pubsub::PubSubRequest::Subscribe { node, jid })
                    if node == key.node() && jid.as_str() == subscription.account.as_str()
            )
        {
            return None;
        }
        let grant = self.space_read_local(key, &subscription.account).await?;
        if grant.subscription_grant() != subscription.grant {
            return None;
        }
        let replay = ProtectedSpaceReplyDescriptor::new(
            key.service().clone(),
            key.node().to_owned(),
            requester.clone(),
            request.id().to_owned(),
            grant.fence.lifecycle().as_uuid(),
            grant.subscription_grant.incarnation.as_uuid(),
            grant.fence.generation().value(),
        )
        .ok()?
        .with_subscription(subscription.subid.clone())
        .ok()?;
        let preference = LocalReplyPreference {
            store: LocalSpaceSubscriptions::new(self.store.as_ref()?.database().clone()),
            key: key.clone(),
            subscription,
        };
        Some(LocalSpaceReplyGrant {
            authority: LocalSpaceReplyAuthority::Active(grant),
            replay,
            preference: Some(preference),
        })
    }

    pub(crate) async fn local_space_bookmark_event(
        &self,
        key: &ScopeKey,
        requester: &FullJid,
        message_id: Option<&str>,
        subscription: &LocalSpaceSubscription,
        version: &SpaceBookmarkVersion,
    ) -> Option<ProtectedSpaceReplyDescriptor> {
        if subscription.account.jid() != &requester.to_bare()
            || subscription.grant.lifecycle != version.lifecycle
            || subscription.grant.incarnation != version.incarnation
        {
            return None;
        }
        let grant = self.space_read_local(key, &subscription.account).await?;
        if grant.subscription_grant() != subscription.grant {
            return None;
        }
        let incarnation = version.incarnation.as_uuid();
        let (room, _) = self
            .local_space_room_dependency(&version.room, subscription.account.jid(), incarnation)
            .await?;
        ProtectedSpaceReplyDescriptor::new(
            key.service().clone(),
            key.node().to_owned(),
            requester.clone(),
            message_id.unwrap_or_default().to_owned(),
            version.lifecycle.as_uuid(),
            incarnation,
            grant.fence.generation().value(),
        )
        .ok()?
        .with_bookmark_event(
            subscription.subid.clone(),
            version.operation.as_uuid(),
            room,
            version.present,
        )
        .ok()
    }

    /// Bind an already-authorized legacy reply to the exact local request.
    /// Legacy access decisions remain the handler's responsibility.
    pub(crate) fn unmanaged_space_disco_reply_local(
        &self,
        service: &BareJid,
        node: &str,
        requester: &FullJid,
        request: &Iq,
    ) -> Option<UnmanagedSpaceReplyGuard> {
        if !is_local_space_disco_request(service, node, requester, request)
            || self.enrolled.space_node(node) != Enrollment::Unmanaged
        {
            return None;
        }
        Some(UnmanagedSpaceReplyGuard {
            enrolled: self.enrolled.clone(),
            service: service.clone(),
            nodes: vec![node.to_owned()],
            recipient: requester.clone(),
            id: request.id().to_owned(),
            exact_payload: None,
        })
    }

    pub(crate) fn unmanaged_space_result_guard(
        &self,
        service: BareJid,
        requester: &FullJid,
        reply: &minidom::Element,
        nodes: Vec<String>,
    ) -> Option<UnmanagedSpaceReplyGuard> {
        if nodes.is_empty()
            || !self
                .enrolled
                .admit_unmanaged_spaces(nodes.iter().map(String::as_str), || ())
                .is_some()
        {
            return None;
        }
        let guard = UnmanagedSpaceReplyGuard {
            enrolled: self.enrolled.clone(),
            service,
            nodes,
            recipient: requester.clone(),
            id: reply.attr("id")?.to_owned(),
            exact_payload: Some(reply.children().next()?.clone()),
        };
        guard.permits_reply(reply, requester).then_some(guard)
    }

    pub(crate) fn local_space_deletion_event(
        key: &ScopeKey,
        recipient: &FullJid,
        record: &OperationRecord,
    ) -> Option<ProtectedSpaceReplyDescriptor> {
        Some(
            ProtectedSpaceReplyDescriptor::new(
                key.service().clone(),
                key.node().to_owned(),
                recipient.clone(),
                record.operation.as_uuid().simple().to_string(),
                record.lifecycle.as_uuid(),
                record.authority_incarnation.as_uuid(),
                record.generation.value(),
            )
            .ok()?
            .with_kind(ProtectedSpaceReplyKind::DeleteEvent),
        )
    }

    async fn refresh_local_space_deletion(
        &self,
        replay: &ProtectedSpaceReplyDescriptor,
        key: &ScopeKey,
        subject: &GrantSubject,
    ) -> Option<LocalSpaceReplyGrant> {
        let operation = GrantOperationId::from_uuid(replay.deletion_operation()?);
        let attachment = self.attachment()?;
        if attachment.binding().incarnation.as_uuid() != replay.incarnation()
            || !matches!(
                attachment
                    .principals()
                    .witness(subject.jid(), &PrincipalEvidence::NoStream),
                PrincipalWitness::Served
            )
        {
            return None;
        }
        let fence = self.fence.as_ref()?;
        let registration = fence.begin_decision().await?;
        let store = self.store.as_ref()?;
        let saved = tokio::time::timeout(
            ROOM_READ_DEADLINE,
            store.load_local_space_deletion_recipient(key, operation, subject),
        )
        .await
        .ok()?
        .ok()??;
        let record = saved.record;
        if Self::local_space_deletion_event(key, replay.recipient(), &record).as_ref()
            != Some(replay)
        {
            return None;
        }
        let ticket = fence.ticket(registration, record.lifecycle, saved.current_generation);
        ticket.is_current().then(|| LocalSpaceReplyGrant {
            authority: LocalSpaceReplyAuthority::Deleted(LocalDeletionDelivery {
                fence: ticket,
                store: store.clone(),
            }),
            replay: replay.clone(),
            preference: None,
        })
    }

    /// Refresh a retained reply's lease only for its original scope decision.
    pub(crate) async fn refresh_local_space_reply(
        &self,
        payload: &SmReplayPayload,
        requester: &FullJid,
    ) -> Option<LocalSpaceReplyGrant> {
        let replay = payload.protected_space_reply()?;
        if requester != replay.recipient() {
            return None;
        }
        let subject = GrantSubject::new(requester.to_bare()).ok()?;
        let key = ScopeKey::new(replay.service().clone(), replay.node()).ok()?;
        if replay.kind() == ProtectedSpaceReplyKind::DeleteEvent {
            return self
                .refresh_local_space_deletion(replay, &key, &subject)
                .await;
        }
        let mut grant = self.space_read_local(&key, &subject).await?;
        if grant.fence.lifecycle().as_uuid() != replay.lifecycle()
            || grant.subscription_grant.incarnation.as_uuid() != replay.incarnation()
            || grant.fence.generation().value() != replay.generation()
        {
            return None;
        }
        if matches!(
            replay.kind(),
            ProtectedSpaceReplyKind::OwnerAffiliations
                | ProtectedSpaceReplyKind::OwnerAffiliationRefusals
                | ProtectedSpaceReplyKind::OwnerConfiguration
        ) && grant.admit(|affiliation| affiliation == Affiliation::Owner) != Some(true)
        {
            return None;
        }
        for expected in replay.room_dependencies() {
            let (current, room) = self
                .local_space_room_dependency(expected.room(), subject.jid(), replay.incarnation())
                .await?;
            if &current != expected {
                return None;
            }
            grant.require_room(room);
        }
        let preference = if let Some(subid) = replay.subscription_id() {
            Some(LocalReplyPreference {
                store: LocalSpaceSubscriptions::new(self.store.as_ref()?.database().clone()),
                key,
                subscription: LocalSpaceSubscription {
                    account: subject,
                    grant: grant.subscription_grant(),
                    subid: subid.clone(),
                },
            })
        } else {
            None
        };
        let result = LocalSpaceReplyGrant {
            authority: LocalSpaceReplyAuthority::Active(grant),
            replay: replay.clone(),
            preference,
        };
        if replay.subscription_id().is_some() {
            let hold = result
                .hold_delivery(result.bookmark_has_payload(payload))
                .await?;
            let _ = hold.rollback().await;
        }
        Some(result)
    }

    pub(crate) async fn local_space_room_dependency(
        &self,
        room: &BareJid,
        requester: &BareJid,
        incarnation: uuid::Uuid,
    ) -> Option<(ProtectedSpaceRoomDependency, ManagedRoomGrants)> {
        if self.attachment()?.binding().incarnation.as_uuid() != incarnation {
            return None;
        }
        let RoomGate::Managed(decision) = self.room(RoomBinding::Room(room)).await else {
            return None;
        };
        if self.attachment()?.binding().incarnation.as_uuid() != incarnation
            || decision.grants.room != *room
            || !decision.allows_as(
                requester,
                &PrincipalEvidence::NoStream,
                HostGrantAction::View,
            )
        {
            return None;
        }
        let dependency = ProtectedSpaceRoomDependency::new(
            room.clone(),
            decision.fence.lifecycle().as_uuid(),
            incarnation,
            decision.fence.generation().value(),
        )
        .ok()?;
        Some((dependency, decision))
    }

    /// Authorize a read for the account carried by the authenticated S2S stream.
    pub async fn space_read_remote(
        &self,
        key: &ScopeKey,
        principal: &RemotePrincipal,
        request: &Iq,
    ) -> Option<ManagedSpaceGrant> {
        if !matches!(request, Iq::Get { .. }) {
            return None;
        }
        self.space_remote_request(key, principal, request).await
    }

    /// Authorize a subscription opened by the authenticated remote account.
    pub(crate) async fn space_subscribe_remote(
        &self,
        key: &ScopeKey,
        principal: &RemotePrincipal,
        request: &Iq,
    ) -> Option<ManagedSpaceGrant> {
        if !matches!(request, Iq::Set { .. }) {
            return None;
        }
        self.space_remote_request(key, principal, request).await
    }

    async fn space_remote_request(
        &self,
        key: &ScopeKey,
        principal: &RemotePrincipal,
        request: &Iq,
    ) -> Option<ManagedSpaceGrant> {
        if request.from() != Some(principal.address())
            || request.to().map(Jid::as_str) != Some(key.service().as_str())
        {
            return None;
        }
        let subject = GrantSubject::new(principal.account().as_bare_jid().clone()).ok()?;
        let mut grant = self
            .space_read(key, &subject, &PrincipalEvidence::of(principal))
            .await?;
        grant.remote_effect = Some(RemoteSpaceEffect::Reply {
            service: key.service().clone(),
            recipient: principal.address().clone(),
            id: request.id().to_owned(),
        });
        Some(grant)
    }

    /// Authorize an event only for a preference still backed by the same
    /// effective affiliation. Regranting access requires resubscription.
    pub async fn space_subscription_event(
        &self,
        key: &ScopeKey,
        subscription: &SpaceSubscription,
    ) -> Option<ManagedSpaceGrant> {
        let expected = subscription.grant?;
        let subject = GrantSubject::new(subscription.account.clone()).ok()?;
        let mut grant = self
            .space_read(
                key,
                &subject,
                &PrincipalEvidence::Stream(subscription.evidence),
            )
            .await?;
        if grant.subscription_grant != expected {
            return None;
        }
        grant.remote_effect = Some(RemoteSpaceEffect::Event {
            key: key.clone(),
            recipient: subscription.account.clone(),
            subscribed_with: subscription.evidence.strength(),
            subscriptions: Box::new(SpaceSubscriptions::new(
                self.store.as_ref()?.database().clone(),
            )),
            bookmark_version: None,
        });
        Some(grant)
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
            remote_effect: None,
            room_dependencies: Vec::new(),
            subscription_grant: SpaceSubscriptionGrant {
                lifecycle: standing.lifecycle,
                incarnation: attachment.binding().incarnation,
                generation: standing.affiliation_generation,
            },
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

#[cfg(test)]
mod unmanaged_space_reply_tests {
    use super::*;

    #[test]
    fn unmanaged_reply_guard_is_bound_to_its_request_and_recipient() {
        let gate = HostGrantGate::default();
        let service: BareJid = "spaces.example.test".parse().expect("service JID");
        let requester: FullJid = "alice@example.test/phone".parse().expect("client JID");
        let other: FullJid = "alice@example.test/desktop".parse().expect("other client");
        let query = || {
            minidom::Element::from(xmpp_parsers::disco::DiscoInfoQuery {
                node: Some("docs".to_owned()),
            })
        };
        let request = Iq::Get {
            from: None,
            to: Some(service.clone().into()),
            id: "space-1".to_owned(),
            payload: query(),
        };
        assert!(gate
            .unmanaged_space_disco_reply_local(&service, "other", &requester, &request)
            .is_none());
        let guard = gate
            .unmanaged_space_disco_reply_local(&service, "docs", &requester, &request)
            .expect("matching legacy decision");
        let reply = |to| {
            minidom::Element::from(Iq::Result {
                from: Some(service.clone().into()),
                to,
                id: "space-1".to_owned(),
                payload: Some(query()),
            })
        };
        assert!(guard.permits_reply(&reply(Some(requester.clone().into())), &requester));
        assert!(guard.permits_reply(&reply(None), &requester));
        assert!(!guard.permits_reply(&reply(None), &other));
        assert!(!guard.permits_reply(&reply(Some(other.into())), &requester));
    }
}
