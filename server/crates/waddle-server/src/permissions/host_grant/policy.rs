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
//! - A grant is honoured as the principal's witness says
//!   ([`PrincipalWitness::honour`], ADR-0020 D1 and D8): as stored for an
//!   account of a domain this deployment serves; for a remote principal
//!   only while its domain is admitted, and an owner or admin grant only
//!   for a trusted home on a `pkix` stream. A grant that is not honoured
//!   contributes nothing: neither `moderate` nor the `read` and
//!   `send_message` the table would derive from it.

use super::issuance::SubjectHome;
use super::model::RoomAffiliation;
use super::principal::PrincipalWitness;

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

/// The standing an effective affiliation is honoured as for the principal
/// `witness` describes. A grant that is not honoured is honoured as no
/// standing at all.
pub fn honoured_affiliation(
    effective: RoomAffiliation,
    witness: &PrincipalWitness,
) -> RoomAffiliation {
    witness.honour(effective)
}

/// Whether the principal `witness` describes, whose grant row holds
/// `recorded` and `effective`, may perform `action`. A recorded `Outcast`
/// denies every action whatever the effective column holds.
pub fn grant_allows(
    recorded: RoomAffiliation,
    effective: RoomAffiliation,
    witness: &PrincipalWitness,
    action: HostGrantAction,
) -> bool {
    recorded != RoomAffiliation::Outcast
        && affiliation_allows(honoured_affiliation(effective, witness), action)
}

/// The least an effective affiliation is honoured as for an account homed
/// at `home` on any decision that honours its grants at all: as stored for
/// a served account, and without privilege or moderation for a remote one,
/// whose owner or admin grant is honoured only on some streams. A change
/// that must never raise what admission honours compares under this
/// floor, which does not depend on the S2S snapshot or on any stream.
pub(crate) fn honoured_floor(effective: RoomAffiliation, home: SubjectHome) -> RoomAffiliation {
    if home == SubjectHome::Remote && effective.confers_privilege_or_moderation() {
        RoomAffiliation::None
    } else {
        effective
    }
}

/// Whether an initiator whose honoured affiliation is `initiator` may
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
