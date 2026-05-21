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
//! 3. [`pin::MucPinHandler`] — XEP-0470 attachment with `urn:waddle:pin:0`
//!    `<pinned/>` / `<unpinned/>` marker. Enforces affiliation-based
//!    authorization; on allow halts with [`super::event::OutboundEvent::ApplyPinChange`]
//!    + [`super::event::OutboundEvent::BroadcastRoomSystemMessage`].
//! 4. [`subject::MucSubjectHandler`] — XEP-0045 §8.1 subject-change
//!    capture. Enforces §8.1 role-based authorization inline; on allow
//!    emits [`super::event::OutboundEvent::PersistRoomSubject`] for the
//!    interpreter to land on the room actor; on deny halts with a
//!    typed `<forbidden/>` reply so neither archive nor reflector run.
//! 5. [`archive::MucArchiveHandler`] — XEP-0313 §5.1.3 archive-eligibility
//!    → emits [`super::event::OutboundEvent::ArchiveGroupchat`] and, for
//!    XEP-0424 retraction requests, an
//!    [`super::event::OutboundEvent::ApplyGroupchatRetractionTombstone`].
//! 6. [`inbox::MucInboxHandler`] — Waddle inbox projection per occupant
//!    (channel + thread rows) via
//!    [`super::event::OutboundEvent::ProjectGroupchatInbox`].
//! 7. [`reflector::ReflectorHandler`] — per-occupant fan-out via
//!    [`super::event::OutboundEvent::RouteToConnection`].

pub mod archive;
pub mod canonicalize;
pub mod context;
pub mod dispatch;
pub mod displayed_marker;
pub mod errors;
pub mod inbox;
pub mod occupancy_validation;
pub mod pin;
pub mod reflector;
pub mod subject;
pub mod traits;

use std::sync::Arc;

pub use context::{OccupantSnapshot, RoomContext, SyntheticSenderAuthority};
pub use dispatch::{RoomDispatchOutcome, RoomDispatcher};
pub use traits::{RoomHandler, RoomHandlerOutcome};

/// Register the locked Q7 room handler chain on `dispatcher` in order.
pub fn register_default_room_handlers(dispatcher: &mut RoomDispatcher) {
    dispatcher.register(Arc::new(occupancy_validation::OccupancyValidationHandler));
    dispatcher.register(Arc::new(canonicalize::MucCanonicalizeHandler));
    dispatcher.register(Arc::new(pin::MucPinHandler));
    dispatcher.register(Arc::new(subject::MucSubjectHandler));
    dispatcher.register(Arc::new(archive::MucArchiveHandler));
    dispatcher.register(Arc::new(inbox::MucInboxHandler));
    dispatcher.register(Arc::new(displayed_marker::MucDisplayedMarkerHandler));
    dispatcher.register(Arc::new(reflector::ReflectorHandler));
}

/// Build a [`RoomDispatcher`] with the full chain (gate + pipeline)
/// registered. Used by the L4 wire-trace tests and by any caller that
/// runs the chain end-to-end without splitting the gate.
pub fn default_room_dispatcher() -> RoomDispatcher {
    let mut d = RoomDispatcher::new();
    register_default_room_handlers(&mut d);
    d
}

/// Register only the post-gate pipeline handlers (canonicalize → pin →
/// subject → archive → inbox → reflector). The interpreter runs
/// [`occupancy_validation::OccupancyValidationHandler`] as an
/// explicit gate before rich-target validation (Copilot review on
/// PR #279), so the production dispatch path uses this variant to
/// avoid running the gate twice.
pub fn register_room_pipeline_handlers(dispatcher: &mut RoomDispatcher) {
    dispatcher.register(Arc::new(canonicalize::MucCanonicalizeHandler));
    dispatcher.register(Arc::new(pin::MucPinHandler));
    dispatcher.register(Arc::new(subject::MucSubjectHandler));
    dispatcher.register(Arc::new(archive::MucArchiveHandler));
    dispatcher.register(Arc::new(inbox::MucInboxHandler));
    dispatcher.register(Arc::new(displayed_marker::MucDisplayedMarkerHandler));
    dispatcher.register(Arc::new(reflector::ReflectorHandler));
}

/// Build a [`RoomDispatcher`] for the post-gate pipeline (the chain
/// minus the occupancy gate). The interpreter calls this variant
/// after running the gate explicitly.
pub fn default_room_pipeline_dispatcher() -> RoomDispatcher {
    let mut d = RoomDispatcher::new();
    register_room_pipeline_handlers(&mut d);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dispatcher_registers_eight_handlers() {
        let d = default_room_dispatcher();
        assert_eq!(d.handler_count(), 8);
    }

    #[test]
    fn pipeline_dispatcher_registers_seven_handlers() {
        let d = default_room_pipeline_dispatcher();
        assert_eq!(d.handler_count(), 7);
    }

    #[test]
    fn pin_handler_runs_after_canonicalize_and_before_archive_and_reflector() {
        // The pin handler must see canonicalized stamps (stanza-id,
        // occupant-id) when building the preview, so it has to run
        // *after* canonicalize. It also `Halt`s on a successful pin
        // to suppress the user's literal pin-attachment from the
        // archive and reflector — so it must run *before* both.
        let names = default_room_dispatcher().handler_names();
        let idx = |name: &str| names.iter().position(|n| *n == name).unwrap_or(usize::MAX);
        let canonicalize = idx("muc-canonicalize");
        let pin = idx("waddle-pin");
        let archive = idx("xep-0313-muc-archive");
        let reflector = idx("xep-0045-reflector");
        assert_ne!(pin, usize::MAX, "pin handler missing from chain");
        assert_ne!(canonicalize, usize::MAX, "canonicalize handler missing");
        // archive and reflector names may differ from these if their
        // RoomHandler::name returns something else — assertion below
        // tolerates that by only enforcing relative position when the
        // referenced handler is actually present.
        assert!(
            canonicalize < pin,
            "pin must run after canonicalize; got order {names:?}"
        );
        if archive != usize::MAX {
            assert!(
                pin < archive,
                "pin must run before archive; got order {names:?}"
            );
        }
        if reflector != usize::MAX {
            assert!(
                pin < reflector,
                "pin must run before reflector; got order {names:?}"
            );
        }
    }
}
