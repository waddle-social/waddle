//! MUC room handler chain.
//!
//! Per #229 Q7 option C, MUC delivery runs through a stateless room
//! handler chain dispatched from the
//! [`super::event::OutboundEvent::DispatchToRoom`] interpreter arm. The
//! chain mirrors the user-side message pipeline shape (one handler per
//! XEP concern, registered in fixed order, ordered events emitted)
//! against a [`context::RoomContext`] frozen-at-dispatch-start snapshot.
//!
//! Locked Q7 chain order:
//!
//! 1. [`occupancy_validation::OccupancyValidationHandler`] — XEP-0045
//!    §7.4 (only occupants may send) + Waddle managed-room policy.
//! 2. [`canonicalize::MucCanonicalizeHandler`] — XEP-0359 stanza-id
//!    `by=room`, XEP-0421 occupant-id stamp, `from='room/nick'` rewrite.
//! 3. [`archive::MucArchiveHandler`] — XEP-0313 §5.1.3 archive-eligibility
//!    → emits [`super::event::OutboundEvent::ArchiveGroupchat`].
//! 4. [`reflector::ReflectorHandler`] — per-occupant fan-out via
//!    [`super::event::OutboundEvent::RouteToConnection`].

pub mod archive;
pub mod canonicalize;
pub mod context;
pub mod dispatch;
pub mod occupancy_validation;
pub mod reflector;
pub mod traits;

use std::sync::Arc;

pub use context::{OccupantSnapshot, RoomContext};
pub use dispatch::{RoomDispatchOutcome, RoomDispatcher};
pub use traits::{RoomHandler, RoomHandlerOutcome};

/// Register the locked Q7 room handler chain on `dispatcher` in order.
pub fn register_default_room_handlers(dispatcher: &mut RoomDispatcher) {
    dispatcher.register(Arc::new(occupancy_validation::OccupancyValidationHandler));
    dispatcher.register(Arc::new(canonicalize::MucCanonicalizeHandler));
    dispatcher.register(Arc::new(archive::MucArchiveHandler));
    dispatcher.register(Arc::new(reflector::ReflectorHandler));
}

/// Build a [`RoomDispatcher`] with the default chain registered.
pub fn default_room_dispatcher() -> RoomDispatcher {
    let mut d = RoomDispatcher::new();
    register_default_room_handlers(&mut d);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dispatcher_registers_four_handlers() {
        let d = default_room_dispatcher();
        assert_eq!(d.handler_count(), 4);
    }
}
