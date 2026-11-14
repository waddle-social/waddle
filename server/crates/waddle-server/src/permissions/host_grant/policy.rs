//! The admission policy of a managed members-only room with explicit
//! affiliations.
//!
//! The policy is a table over a subject's effective affiliation in the room
//! and the action. It mirrors what the built-in authorization schema grants
//! the four affiliation relations (`permissions::schema`), without anything
//! inherited from a parent Space: this policy admits on explicit
//! affiliations only.
//!
//! - `Outcast` is denied every action. A subject holds one affiliation per
//!   room, so a ban replaces whatever the subject held, and admission also
//!   refuses a subject whose recorded affiliation is `Outcast` whatever its
//!   effective affiliation says.
//! - `None` is denied every action: the room is members-only.
//! - A message author's own correction and retraction are sends, so they
//!   need what `SendMessage` needs and an outcast author loses both.
//! - No explicit affiliation confers `Delete`: the schema grants channel
//!   deletion through the parent Space only.

use super::model::RoomAffiliation;

/// An action the policy protects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HostGrantAction {
    View,
    Read,
    SendMessage,
    Moderate,
    Manage,
    Delete,
    /// The author's correction of their own message.
    EditOwnMessage,
    /// The author's retraction of their own message.
    DeleteOwnMessage,
}

impl HostGrantAction {
    pub const ALL: [Self; 8] = [
        Self::View,
        Self::Read,
        Self::SendMessage,
        Self::Moderate,
        Self::Manage,
        Self::Delete,
        Self::EditOwnMessage,
        Self::DeleteOwnMessage,
    ];
}

/// Whether a subject whose effective affiliation is `affiliation` may
/// perform `action`.
pub fn affiliation_allows(affiliation: RoomAffiliation, action: HostGrantAction) -> bool {
    use HostGrantAction::{Delete, DeleteOwnMessage, EditOwnMessage, Read, SendMessage, View};
    match affiliation {
        RoomAffiliation::Outcast | RoomAffiliation::None => false,
        RoomAffiliation::Member => matches!(
            action,
            View | Read | SendMessage | EditOwnMessage | DeleteOwnMessage
        ),
        RoomAffiliation::Admin | RoomAffiliation::Owner => !matches!(action, Delete),
    }
}

/// Whether an initiator whose effective affiliation is `initiator` may
/// change a subject's affiliation from `current` to `requested`.
///
/// Only an owner grants or removes owner and admin standing; an owner or an
/// admin changes member, none and outcast standing of everyone else.
pub(crate) fn initiator_may_change(
    initiator: RoomAffiliation,
    current: RoomAffiliation,
    requested: RoomAffiliation,
) -> bool {
    let privileged = |affiliation: RoomAffiliation| affiliation.confers_privilege_or_moderation();
    if privileged(current) || privileged(requested) {
        initiator == RoomAffiliation::Owner
    } else {
        privileged(initiator)
    }
}
