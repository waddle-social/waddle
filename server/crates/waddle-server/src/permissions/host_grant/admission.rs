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
//! active under the current authority incarnation and policy version, that
//! incarnation, its restore epoch, the start of the PostgreSQL server
//! answering and the database it answers for are the ones the replica's
//! attachment verified, and the
//! sender's honoured affiliation allows the action. The decision also names
//! the audience: of the principals the acceptance would deliver to, project
//! for or notify, those whose honoured affiliation allows `Read`. The caller
//! accepts nothing for anyone else. A subject whose recorded affiliation is
//! `outcast` is refused whatever else is stored.
//!
//! Whose grants are honoured is decided by the replica's
//! [`PrincipalPolicy`] at this decision (ADR-0020 D1 and D8, gate G17): the
//! sender with the evidence of how its request arrived, and every named
//! recipient without stream evidence. A remote principal's grants
//! contribute nothing unless its domain is admitted under the S2S snapshot
//! the replica runs, and an owner or admin grant of a remote principal only
//! for a trusted home on a `pkix` stream. The witness names what the
//! sender was decided as.

use std::collections::BTreeMap;

use jid::BareJid;
use waddle_xmpp::identity::CanonicalBareJid;

use super::model::{
    guard_queue_key, AuthorityIncarnation, GrantGeneration, GrantOperationId, RoomAffiliation,
    ScopeLifecycleId, HOST_GRANT_POLICY_VERSION,
};
use super::policy::{grant_allows, HostGrantAction};
use super::principal::{PrincipalEvidence, PrincipalPolicy, PrincipalWitness};
use super::restore::AuthorityBinding;
use super::store::{binding_query, decode_binding};
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
    /// How the sender's request reached the acceptance.
    pub sender_evidence: PrincipalEvidence,
    /// The action the sender performs.
    pub action: HostGrantAction,
    /// The bare JID of every principal the acceptance names as a recipient:
    /// whoever it would deliver to, project an inbox row for or notify.
    pub audience: &'a [BareJid],
    /// The served account domains and the S2S snapshot the replica runs.
    pub principals: &'a PrincipalPolicy,
    /// The authority the replica's attachment verified: its incarnation,
    /// restore epoch, PostgreSQL server start and database.
    pub binding: &'a AuthorityBinding,
}

/// The canonical state an admission was decided under. It is valid for the
/// transaction that holds the scope's guard, and for nothing after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionWitness {
    pub lifecycle: ScopeLifecycleId,
    pub incarnation: AuthorityIncarnation,
    pub generation: GrantGeneration,
    pub operation: GrantOperationId,
    /// What the sender was decided as: a served account, or a remote
    /// principal with its domain, stream evidence, D1 admission decision
    /// and trusted-home status under the snapshot the replica runs.
    pub sender: PrincipalWitness,
}

/// Why a managed room's acceptance is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDenial {
    /// The scope is not active under the current incarnation and policy
    /// version, or its stored state cannot be read as valid.
    ScopeClosed,
    /// The authority incarnation, its restore epoch, the PostgreSQL server
    /// answering or its database is not the one this replica verified when
    /// it attached: a restore transition was made, or the database was
    /// restored, moved or restarted, since. The replica admits no managed
    /// room until it has passed the restore fence again.
    AuthorityChanged,
    /// The sender's honoured affiliation does not allow the action.
    SenderNotAdmitted,
}

/// The decision for the acceptance of a managed room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomAdmission {
    /// The sender is admitted. `audience` holds the named principals whose
    /// honoured affiliation allows `Read`, in the order they were named;
    /// nothing may be accepted for a principal outside it.
    Admitted {
        witness: AdmissionWitness,
        audience: Vec<BareJid>,
    },
    Denied(AdmissionDenial),
}

/// The scope that manages a room, as an acceptance transaction found it.
pub(crate) struct ManagingScope {
    lifecycle: String,
}

/// What the room bindings hold for a room address.
pub(crate) enum RoomManagement {
    /// No scope manages the room.
    Unmanaged,
    Managed(ManagingScope),
    /// The address is not in the identity profile's canonical form. Room
    /// bindings are keyed by the canonical form, so whether a scope manages
    /// the room this address reaches cannot be decided from it.
    NotCanonical,
}

/// Find the scope that manages `room`, if any. A room binding is immutable,
/// so the lookup takes no lock; a room without one is not this authority's
/// concern. The lookup is made by the canonical address only.
pub(crate) async fn managing_scope(
    tx: &mut Transaction<'_>,
    relations: &AdmissionRelations,
    room: &BareJid,
) -> Result<RoomManagement, DatabaseError> {
    let Ok(room) = CanonicalBareJid::new(room.clone()) else {
        return Ok(RoomManagement::NotCanonical);
    };
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
        Some(row) => Ok(RoomManagement::Managed(ManagingScope {
            lifecycle: row.get(0)?,
        })),
        None => Ok(RoomManagement::Unmanaged),
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

    // The queue in front of the guard, shared: an acceptance that arrives
    // while a mutation waits for the guard waits behind that mutation
    // instead of joining the acceptances that keep it waiting
    // (`model::guard_queue_key`).
    tx.execute(
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)",
        crate::db_params![guard_queue_key(lifecycle)],
    )
    .await?;

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

    let mut rows = tx.query(&binding_query(&relations.authority), ()).await?;
    let authority = match rows.next().await? {
        Some(row) => decode_binding(&row)?,
        None => None,
    };
    drop(rows);

    let (Some(incarnation), Some(generation), Some(operation), Some(read)) =
        (incarnation, generation, operation, authority)
    else {
        return Ok(closed);
    };
    if *acceptance.binding != read {
        return Ok(RoomAdmission::Denied(AdmissionDenial::AuthorityChanged));
    }
    let current = read.incarnation;
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

    let allows = |subject: &BareJid, witness: &PrincipalWitness, action: HostGrantAction| {
        held.get(subject.as_str())
            .is_some_and(|(recorded, effective)| {
                grant_allows(*recorded, *effective, witness, action)
            })
    };
    let sender = acceptance
        .principals
        .witness(acceptance.sender, &acceptance.sender_evidence);
    if !allows(acceptance.sender, &sender, acceptance.action) {
        return Ok(RoomAdmission::Denied(AdmissionDenial::SenderNotAdmitted));
    }
    let audience = acceptance
        .audience
        .iter()
        .filter(|recipient| {
            let witness = acceptance
                .principals
                .witness(recipient, &PrincipalEvidence::NoStream);
            allows(recipient, &witness, HostGrantAction::Read)
        })
        .cloned()
        .collect();
    Ok(RoomAdmission::Admitted {
        witness: AdmissionWitness {
            lifecycle,
            incarnation,
            generation,
            operation,
            sender,
        },
        audience,
    })
}
