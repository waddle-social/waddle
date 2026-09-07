//! Frozen membership operations for early message handlers.
use crate::server::routes::websocket::handlers::message::{group_dm_invite, muc_invite};

#[derive(Clone, Debug)]
pub enum RoomMembershipMutation {
    GroupDm(Box<group_dm_invite::GroupDmMembershipMutation>),
    Muc(Box<muc_invite::MucMembershipMutation>),
}

/// Whether execution changed affiliation or preserved a concurrently granted
/// membership. Phase C binds this outcome before executing dependent effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MembershipOutcome {
    Granted {
        previous_affiliation: waddle_xmpp::Affiliation,
    },
    Preserved,
}
