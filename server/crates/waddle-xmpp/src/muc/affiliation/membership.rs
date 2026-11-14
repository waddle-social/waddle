//! Durable room-membership source used for spawn-time hydration.
//!
//! A freshly spawned [`crate::muc::room_actor::RoomActor`] starts with an
//! empty in-memory affiliation list; before #1135 the durable inbox
//! recipient set in [`crate::muc::room_actor::GetRoomSnapshot`] was derived
//! only from joins and point mutations observed *since spawn*, so offline
//! members silently dropped out of inbox/notification fan-out after a
//! deploy or actor respawn. Implementations of this trait bridge to the
//! deployment's durable membership store (permission tuples) without
//! introducing a database dependency into `waddle-xmpp`.

use std::future::Future;
use std::pin::Pin;

use jid::BareJid;

use crate::XmppError;

/// Boxed future returned by [`DurableMembershipSource::list_durable_member_jids`].
///
/// Boxed (rather than RPITIT like [`super::AffiliationResolver`]) so the
/// trait stays dyn-compatible: the room registry holds it as
/// `Arc<dyn DurableMembershipSource>` and forwards it to each freshly
/// spawned room actor.
pub type DurableMembershipFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<BareJid>, XmppError>> + Send + 'a>>;

/// Source of durable (persisted) room membership.
///
/// Returns the bare JIDs of every user durably affiliated at
/// `Member` or above with the channel identified by
/// (`waddle_id`, `channel_id`) — the set that must receive durable
/// inbox rows / notification candidates for groupchat messages even
/// when they have not joined the current room-actor incarnation.
pub trait DurableMembershipSource: Send + Sync {
    /// List the bare JIDs durably affiliated at `Member`+ with the channel.
    fn list_durable_member_jids(
        &self,
        waddle_id: &str,
        channel_id: &str,
    ) -> DurableMembershipFuture<'_>;

    /// Whether the room at `room_jid` is bound members-only: its admission
    /// belongs to an authority outside the room actor that admits members
    /// only, so the actor spawned for it keeps a members-only configuration
    /// whatever its creation spec, durable state or a configuration change
    /// says. Answered without waiting; a source without such an authority
    /// answers `false`.
    fn binds_members_only(&self, _room_jid: &BareJid) -> bool {
        false
    }
}
