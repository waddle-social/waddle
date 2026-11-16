//! The host-grant decision every surface outside room-manifest acceptance
//! consults for a room an enrolled scope manages.
//!
//! [`HostGrantGate`] pairs the canonical store with the replica's enrolled
//! set ([`EnrolledScopes`]) and the [`HostGrantAttachment`] its
//! [`HostGrantAttachmentGate`] holds: the attachment the restore fence
//! verified, which the attachment gate replaces when it verifies a changed
//! authority again and drops while the fence refuses. Whether a room
//! is managed is answered from the enrolled set, in memory: a room outside
//! it answers [`RoomGate::Unmanaged`] without touching PostgreSQL, and the
//! surface keeps its existing checks. A managed room is answered from its
//! canonical grants alone, read from PostgreSQL in one bounded snapshot,
//! with the policy room-manifest admission applies (`policy::grant_allows`):
//! legacy tuples, durable room affiliations, PubSub affiliations, Space
//! membership and the room actor's occupancy admit nothing. It answers
//! [`RoomGate::Unavailable`] when the enrolled set has not loaded, when the
//! address reaches a managed room in a form that is not canonical, when the
//! grants cannot be read within the bounds, on a replica whose
//! attachment gate holds no attachment (its runtime role's privileges or
//! the restore fence not verified), and on a replica that holds no lease of
//! the revocation fence: a surface fails closed on it.
//!
//! Every decision for a managed room carries the replica's revocation-fence
//! ticket ([`FenceTicket`]): the registration and the scope generation it
//! was read under. [`ManagedRoomGrants::allows`] and
//! [`ManagedRoomGrants::standing`] answer nothing once the ticket is no
//! longer current, so a decision the replica has acknowledged a newer
//! generation over, or that outlived the replica's lease, admits nothing
//! however long its caller held it (see `fence`).
//!
//! SQLite holds no canonical store, so every room is unmanaged there.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jid::BareJid;

use super::enrolled::{EnrolledScopes, Enrollment};
use super::fence::{FenceTicket, ReplicaFence};
use super::issuance::ServedAccountDomains;
use super::policy::HostGrantAction;
use super::rate_limited::RateLimitedWarning;
use super::runtime_attach::{HostGrantAttachment, HostGrantAttachmentGate};
use super::standing::{RoomBinding, RoomGrants, RoomLookup};
use super::store::HostGrantStore;
use crate::db::Database;

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
}

/// Whether a room or Space is managed by an enrolled scope cannot be told:
/// the enrolled set has not loaded. A caller fails closed on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the host-grant enrolled set is not known")]
pub struct HostGrantUnavailable;

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

/// A managed room's canonical grants together with the account domains the
/// replica serves, which decide whose privileged grants are honoured, and
/// the revocation-fence ticket the grants were read under.
#[derive(Clone, Debug)]
pub struct ManagedRoomGrants {
    pub grants: RoomGrants,
    pub served: ServedAccountDomains,
    /// What the grants were read under. A decision is acted on only while
    /// it is current.
    pub fence: FenceTicket,
}

impl ManagedRoomGrants {
    /// Whether `subject` may perform `action`, and the decision may still
    /// be acted on: a decision whose ticket is no longer current allows
    /// nothing.
    pub fn allows(&self, subject: &BareJid, action: HostGrantAction) -> bool {
        self.fence.is_current() && self.grants.allows(subject, action, &self.served)
    }

    /// The standing `subject` is honoured with; `None` once the decision's
    /// ticket is no longer current.
    pub fn standing(&self, subject: &BareJid) -> super::model::RoomAffiliation {
        if !self.fence.is_current() {
            return super::model::RoomAffiliation::None;
        }
        self.grants.standing(subject, &self.served)
    }

    /// The standing the grants as read honour `subject` with, whether or
    /// not the decision may still be acted on. It is what the grants said,
    /// for a caller that puts derived state back in line with them and
    /// checks the ticket itself.
    pub fn standing_as_read(&self, subject: &BareJid) -> super::model::RoomAffiliation {
        self.grants.standing(subject, &self.served)
    }

    pub fn admitted(&self, action: HostGrantAction) -> Vec<BareJid> {
        self.grants.admitted(action, &self.served)
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
        let fence = ReplicaFence::new(store.clone(), enrolled.clone(), Arc::clone(&attachment));
        Self {
            store: Some(store),
            enrolled,
            attachment,
            fence: Some(fence),
        }
    }

    /// Decide managed rooms with the attachment `gate` holds: the replica's
    /// attachment gate, shared with room-manifest acceptance. Only the first
    /// gate bound is kept; the answer is whether this call bound it.
    pub fn bind_attachment(&self, gate: Arc<HostGrantAttachmentGate>) -> bool {
        self.attachment.set(gate).is_ok()
    }

    /// Hold `attachment` in the bound attachment gate, binding a gate of
    /// this gate's own when none is bound. The first attachment stays; the
    /// answer is whether this call set it.
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

    /// The verified attachment the bound attachment gate holds now, if any.
    pub fn attachment(&self) -> Option<HostGrantAttachment> {
        self.attachment.get().and_then(|gate| gate.current())
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
                     attachment (runtime role privileges or restore fence)"
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
                            served: attachment.served().clone(),
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
    /// Nothing may be admitted from it.
    pub async fn room_as_stored(&self, binding: RoomBinding<'_>) -> RoomGate<ManagedRoomGrants> {
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
            Ok(Ok(RoomLookup::Managed(grants))) => {
                let fence = FenceTicket::unfenced(grants.lifecycle, grants.generation);
                RoomGate::Managed(ManagedRoomGrants {
                    grants,
                    served: attachment.served().clone(),
                    fence,
                })
            }
            _ => RoomGate::Unavailable,
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
        }
    }

    /// Whether `subject` may perform `action` in the room `binding` names.
    pub async fn allows(
        &self,
        binding: RoomBinding<'_>,
        subject: &BareJid,
        action: HostGrantAction,
    ) -> RoomGate<bool> {
        match self.room(binding).await {
            RoomGate::Unmanaged => RoomGate::Unmanaged,
            RoomGate::Unavailable => RoomGate::Unavailable,
            RoomGate::Managed(room) => RoomGate::Managed(room.allows(subject, action)),
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
