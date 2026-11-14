//! The host-grant decision every surface outside room-manifest acceptance
//! consults for a room an enrolled scope manages.
//!
//! [`HostGrantGate`] pairs the canonical store with the replica's
//! [`HostGrantAttachment`]. For a room no scope manages it answers
//! [`RoomGate::Unmanaged`] and the surface keeps its existing checks. For a
//! managed room it answers from the canonical grants alone, with the policy
//! room-manifest admission applies (`policy::grant_allows`): legacy tuples,
//! durable room affiliations, PubSub affiliations, Space membership and the
//! room actor's occupancy admit nothing. It answers
//! [`RoomGate::Unavailable`] when the store cannot be read, when the room
//! address is not canonical, and on a replica that has not verified its
//! runtime role's privileges on the store: a surface fails closed on it.
//!
//! SQLite holds no canonical store, so every room is unmanaged there.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use jid::BareJid;

use super::issuance::ServedAccountDomains;
use super::policy::HostGrantAction;
use super::runtime_attach::HostGrantAttachment;
use super::standing::{RoomBinding, RoomGrants, RoomLookup};
use super::store::HostGrantStore;
use crate::db::Database;

/// At most one warning a minute that a managed room was refused on a replica
/// without an attachment; only the kind of the refusal is logged.
static UNATTACHED: RateLimitedWarning = RateLimitedWarning::new();

/// A once-a-minute limiter for one warning.
struct RateLimitedWarning {
    /// Milliseconds after the process's first warning check at which the
    /// next warning may be logged.
    next_ms: std::sync::atomic::AtomicU64,
}

impl RateLimitedWarning {
    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

    const fn new() -> Self {
        Self {
            next_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn due(&self) -> bool {
        use std::sync::atomic::Ordering;
        static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
        let now = u64::try_from(
            ORIGIN
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let due = self.next_ms.load(Ordering::Relaxed);
        let next =
            now.saturating_add(u64::try_from(Self::INTERVAL.as_millis()).unwrap_or(u64::MAX));
        now >= due
            && self
                .next_ms
                .compare_exchange(due, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }
}

/// The canonical store of a replica and its verified attachment to it.
#[derive(Clone, Default)]
pub struct HostGrantGate {
    store: Option<HostGrantStore>,
    attachment: Arc<OnceLock<HostGrantAttachment>>,
}

/// The host-grant store could not be read, so whether a room or Space is
/// managed by an enrolled scope is unknown. A caller fails closed on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the host-grant store could not be read")]
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
/// replica serves, which decide whose privileged grants are honoured.
#[derive(Clone, Debug)]
pub struct ManagedRoomGrants {
    pub grants: RoomGrants,
    pub served: ServedAccountDomains,
}

impl ManagedRoomGrants {
    pub fn allows(&self, subject: &BareJid, action: HostGrantAction) -> bool {
        self.grants.allows(subject, action, &self.served)
    }

    pub fn standing(&self, subject: &BareJid) -> super::model::RoomAffiliation {
        self.grants.standing(subject, &self.served)
    }

    pub fn admitted(&self, action: HostGrantAction) -> Vec<BareJid> {
        self.grants.admitted(action, &self.served)
    }
}

impl HostGrantGate {
    /// The gate of a replica whose global database is `db`. A database that
    /// is not PostgreSQL has no canonical store, and the gate then answers
    /// [`RoomGate::Unmanaged`] for every room.
    pub fn new(db: Database) -> Self {
        Self {
            store: HostGrantStore::open(db).ok(),
            attachment: Arc::default(),
        }
    }

    /// Record the replica's verified attachment. Only the first one is kept.
    pub fn attach(&self, attachment: HostGrantAttachment) -> bool {
        self.attachment.set(attachment).is_ok()
    }

    /// The canonical store, on PostgreSQL.
    pub fn store(&self) -> Option<&HostGrantStore> {
        self.store.as_ref()
    }

    /// The replica's verified attachment, once its check passed.
    pub fn attachment(&self) -> Option<&HostGrantAttachment> {
        self.attachment.get()
    }

    /// The canonical grants of the room `binding` names.
    pub async fn room(&self, binding: RoomBinding<'_>) -> RoomGate<ManagedRoomGrants> {
        let Some(store) = &self.store else {
            return RoomGate::Unmanaged;
        };
        match store.room_grants(binding).await {
            Ok(RoomLookup::Unmanaged) => RoomGate::Unmanaged,
            Ok(RoomLookup::NotCanonical) => RoomGate::Unavailable,
            Ok(RoomLookup::Managed(grants)) => match self.attachment.get() {
                Some(attachment) => RoomGate::Managed(ManagedRoomGrants {
                    grants,
                    served: attachment.served().clone(),
                }),
                None => {
                    if UNATTACHED.due() {
                        tracing::warn!(
                            kind = "authority_unavailable",
                            "A managed room was refused: this replica has not verified its \
                             runtime role's privileges on the host-grant store"
                        );
                    }
                    RoomGate::Unavailable
                }
            },
            Err(error) => {
                tracing::warn!(
                    kind = error.kind(),
                    "The host-grant store could not be read for a room decision"
                );
                RoomGate::Unavailable
            }
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
    /// managed by an enrolled scope. `Err` when that cannot be read.
    pub async fn channel_enrolled(&self, channel_id: &str) -> Result<bool, HostGrantUnavailable> {
        let Some(store) = &self.store else {
            return Ok(false);
        };
        store
            .enrolled_channels(&[channel_id])
            .await
            .map(|enrolled| !enrolled.is_empty())
            .map_err(|error| {
                tracing::warn!(
                    kind = error.kind(),
                    "The host-grant store could not be read for an enrolled-room lookup"
                );
                HostGrantUnavailable
            })
    }

    /// Which of `channel_ids` belong to rooms enrolled scopes manage.
    pub async fn enrolled_channels(
        &self,
        channel_ids: &[&str],
    ) -> Result<BTreeSet<String>, HostGrantUnavailable> {
        let Some(store) = &self.store else {
            return Ok(BTreeSet::new());
        };
        store.enrolled_channels(channel_ids).await.map_err(|error| {
            tracing::warn!(
                kind = error.kind(),
                "The host-grant store could not be read for an enrolled-room lookup"
            );
            HostGrantUnavailable
        })
    }

    /// Whether `node` is the Space node of an enrolled scope. `Err` when
    /// that cannot be read.
    pub async fn space_enrolled(&self, node: &str) -> Result<bool, HostGrantUnavailable> {
        let Some(store) = &self.store else {
            return Ok(false);
        };
        store.enrolled_space_node(node).await.map_err(|error| {
            tracing::warn!(
                kind = error.kind(),
                "The host-grant store could not be read for an enrolled-Space lookup"
            );
            HostGrantUnavailable
        })
    }
}
