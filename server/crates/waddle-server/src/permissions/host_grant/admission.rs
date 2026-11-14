//! Admission of a fresh room acceptance from the canonical policy state.
//!
//! The acceptance transaction calls [`managing_scope`] and, for a managed
//! room, [`admit_room_acceptance`] before it records anything it accepts.
//! The latter locks the scope's state row
//! `FOR SHARE` and reads the policy state under that lock, so the state it
//! decides from cannot change until the acceptance transaction ends: a
//! mutation of the scope waits for it, and an acceptance that starts after a
//! mutation committed reads the mutated state. Nothing outside PostgreSQL is
//! consulted.
//!
//! A room that no scope manages is not this authority's concern and keeps
//! its existing admission. A managed room is admitted only when its scope is
//! active under the current authority incarnation and policy version, the
//! sender's effective affiliation allows the action, and every recipient's
//! effective affiliation allows `Read`. A subject whose recorded affiliation
//! is `outcast` is refused whatever else is stored.

use std::collections::BTreeMap;

use jid::BareJid;

use super::model::{
    AuthorityIncarnation, GrantGeneration, GrantOperationId, RoomAffiliation, ScopeLifecycleId,
    HOST_GRANT_POLICY_VERSION,
};
use super::policy::{affiliation_allows, HostGrantAction};
use crate::db::{DatabaseError, Transaction};

/// The relations admission reads, as the acceptance transaction names them.
pub(crate) struct AdmissionRelations {
    pub(crate) rooms: String,
    pub(crate) scopes: String,
    pub(crate) grants: String,
    pub(crate) authority: String,
}

/// What an acceptance transaction is about to accept for a room.
#[derive(Clone, Copy, Debug)]
pub struct RoomAcceptance<'a> {
    pub room: &'a BareJid,
    /// The bare JID of the principal the acceptance is made for.
    pub sender: &'a BareJid,
    /// The action the sender performs.
    pub action: HostGrantAction,
    /// The bare JID of every recipient the acceptance freezes.
    pub audience: &'a [BareJid],
}

/// The canonical state an admission was decided under. It is valid for the
/// transaction that holds the scope's guard, and for nothing after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionWitness {
    pub lifecycle: ScopeLifecycleId,
    pub incarnation: AuthorityIncarnation,
    pub generation: GrantGeneration,
    pub operation: GrantOperationId,
}

/// Why a managed room's acceptance is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDenial {
    /// The scope is not active under the current incarnation and policy
    /// version, or its stored state cannot be read as valid.
    ScopeClosed,
    /// The sender's effective affiliation does not allow the action.
    SenderNotAdmitted,
    /// A recipient's effective affiliation does not allow `Read`.
    AudienceNotAdmitted,
}

/// The decision for the acceptance of a managed room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomAdmission {
    Admitted(AdmissionWitness),
    Denied(AdmissionDenial),
}

/// The scope that manages a room, as an acceptance transaction found it.
pub(crate) struct ManagingScope {
    lifecycle: String,
}

/// Find the scope that manages `room`, if any. A room binding is immutable,
/// so the lookup takes no lock; a room without one is not this authority's
/// concern.
pub(crate) async fn managing_scope(
    tx: &mut Transaction<'_>,
    relations: &AdmissionRelations,
    room: &BareJid,
) -> Result<Option<ManagingScope>, DatabaseError> {
    let mut rows = tx
        .query(
            &format!(
                "SELECT lifecycle_id FROM {} WHERE room_jid = ?",
                relations.rooms
            ),
            crate::db_params![room.as_str()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(ManagingScope {
            lifecycle: row.get(0)?,
        })),
        None => Ok(None),
    }
}

/// Decide the acceptance of a managed room in the caller's transaction and
/// keep the scope's guard, shared, until that transaction ends.
pub(crate) async fn admit_room_acceptance(
    tx: &mut Transaction<'_>,
    relations: &AdmissionRelations,
    scope: &ManagingScope,
    acceptance: &RoomAcceptance<'_>,
) -> Result<RoomAdmission, DatabaseError> {
    let closed = RoomAdmission::Denied(AdmissionDenial::ScopeClosed);
    let lifecycle_text = scope.lifecycle.as_str();
    let Some(lifecycle) = ScopeLifecycleId::from_storage(lifecycle_text) else {
        return Ok(closed);
    };

    // The guard: shared with other acceptances, exclusive with mutations.
    let mut rows = tx
        .query(
            &format!(
                "SELECT authority_incarnation, policy_version, generation, operation_id, \
                 admission_state FROM {} WHERE lifecycle_id = ? FOR SHARE",
                relations.scopes
            ),
            crate::db_params![lifecycle_text],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(closed);
    };
    let incarnation = AuthorityIncarnation::from_storage(&row.get::<String>(0)?);
    let policy_version: i64 = row.get(1)?;
    let generation = GrantGeneration::new(row.get::<i64>(2)?);
    let operation = GrantOperationId::from_storage(&row.get::<String>(3)?);
    let active = row.get::<String>(4)? == "active";
    drop(rows);

    let mut rows = tx
        .query(
            &format!(
                "SELECT incarnation FROM {} WHERE singleton",
                relations.authority
            ),
            (),
        )
        .await?;
    let current = match rows.next().await? {
        Some(row) => AuthorityIncarnation::from_storage(&row.get::<String>(0)?),
        None => None,
    };
    drop(rows);

    let (Some(incarnation), Some(generation), Some(operation), Some(current)) =
        (incarnation, generation, operation, current)
    else {
        return Ok(closed);
    };
    if !active || incarnation != current || policy_version != i64::from(HOST_GRANT_POLICY_VERSION) {
        return Ok(closed);
    }

    let mut rows = tx
        .query(
            &format!(
                "SELECT subject_jid, affiliation, effective_affiliation FROM {} \
                 WHERE lifecycle_id = ? AND room_jid = ?",
                relations.grants
            ),
            crate::db_params![lifecycle_text, acceptance.room.as_str()],
        )
        .await?;
    let mut held: BTreeMap<String, (RoomAffiliation, RoomAffiliation)> = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let (Some(recorded), Some(effective)) = (
            RoomAffiliation::from_storage(&row.get::<String>(1)?),
            RoomAffiliation::from_storage(&row.get::<String>(2)?),
        ) else {
            return Ok(closed);
        };
        held.insert(row.get::<String>(0)?, (recorded, effective));
    }
    drop(rows);

    let allows = |subject: &BareJid, action: HostGrantAction| {
        held.get(subject.as_str())
            .is_some_and(|(recorded, effective)| {
                *recorded != RoomAffiliation::Outcast && affiliation_allows(*effective, action)
            })
    };
    if !allows(acceptance.sender, acceptance.action) {
        return Ok(RoomAdmission::Denied(AdmissionDenial::SenderNotAdmitted));
    }
    if !acceptance
        .audience
        .iter()
        .all(|recipient| allows(recipient, HostGrantAction::Read))
    {
        return Ok(RoomAdmission::Denied(AdmissionDenial::AudienceNotAdmitted));
    }
    Ok(RoomAdmission::Admitted(AdmissionWitness {
        lifecycle,
        incarnation,
        generation,
        operation,
    }))
}
