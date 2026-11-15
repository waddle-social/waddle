//! Attribute enums for the uniform per-actor instruments.

use super::sealed;
use super::MetricAttribute;

/// `actor` — which kind of actor a sample describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    /// The registry that owns room actors.
    RoomRegistry,
    /// One multi-user chat room.
    Room,
    /// The registry that owns user actors.
    UserRegistry,
    /// One bare JID's sessions.
    User,
    /// The permission store.
    Permission,
    /// The account mutation coordinator.
    AccountCoordinator,
    /// The database owner.
    Db,
    /// The cross-node relay.
    Relay,
    /// One client connection. Only its binding commands pass through the
    /// mailbox handler; stanza turns are `waddle.connection_actor.*`.
    Connection,
    /// An actor type not listed here.
    Other,
}

impl ActorKind {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 10] = [
        Self::RoomRegistry,
        Self::Room,
        Self::UserRegistry,
        Self::User,
        Self::Permission,
        Self::AccountCoordinator,
        Self::Db,
        Self::Relay,
        Self::Connection,
        Self::Other,
    ];
}

impl sealed::Sealed for ActorKind {}
impl MetricAttribute for ActorKind {
    fn key(&self) -> &'static str {
        "actor"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::RoomRegistry => "room_registry",
            Self::Room => "room",
            Self::UserRegistry => "user_registry",
            Self::User => "user",
            Self::Permission => "permission",
            Self::AccountCoordinator => "account_coordinator",
            Self::Db => "db",
            Self::Relay => "relay",
            Self::Connection => "connection",
            Self::Other => "other",
        }
    }
}

/// `reason` — whether an actor stopped because of a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorStopKind {
    /// A lifecycle hook or handler panicked or returned a fatal error.
    Panicked,
    /// Any other stop: normal completion, a kill, or a linked actor's death.
    Stopped,
}

impl ActorStopKind {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 2] = [Self::Panicked, Self::Stopped];
}

impl sealed::Sealed for ActorStopKind {}
impl MetricAttribute for ActorStopKind {
    fn key(&self) -> &'static str {
        "reason"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Panicked => "panicked",
            Self::Stopped => "stopped",
        }
    }
}
