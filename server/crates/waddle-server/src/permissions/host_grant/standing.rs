//! Reads of a managed room's canonical grants outside an acceptance
//! transaction.
//!
//! The surfaces that do not seal a room manifest (joins, archive and
//! discovery reads, PubSub, room administration, the planner's audience
//! sources) decide from these reads. A read is one bounded
//! `REPEATABLE READ` snapshot of PostgreSQL that takes no row lock: it never
//! waits behind a mutation's guard and never delays one. Its answer is a
//! decision as of that snapshot, and each surface states how the snapshot is
//! ordered against its effect (see the "Writer fence and sink contracts"
//! section of `server/docs/planning/host-grant-authority-2047.md`).
//!
//! The surfaces read a room's grants only for a room the replica's enrolled
//! set names ([`super::EnrolledScopes`]), which this module also loads.
//!
//! A database whose schema holds no host-grant relation holds no enrolled
//! scope, so every room is unmanaged there.

use std::collections::BTreeMap;

use jid::BareJid;
use waddle_xmpp::identity::CanonicalBareJid;
use waddle_xmpp::pubsub::Affiliation;

use super::enrolled::EnrolledSet;
use super::error::HostGrantError;
use super::issuance::{IssuanceBasis, IssuancePolicy};
use super::model::{
    GrantGeneration, GrantSubject, RoomAffiliation, ScopeKey, ScopeLifecycleId,
    SpaceAffiliationChange, SpaceMembership, HOST_GRANT_SCOPE_MAX_GRANT_ROWS,
};
use super::policy::{grant_allows, honoured_affiliation, HostGrantAction};
use super::principal::{PrincipalEvidence, PrincipalPolicy};
use super::restore::AuthorityBinding;
use super::store::{
    affiliation_column, corrupt, load_binding, load_incarnation, load_scope, scope_admits,
    HostGrantStore,
};
use crate::db::Transaction;

pub(super) const MAX_SPACE_AFFILIATION_CHANGES: usize = 64;

pub(super) struct SpaceStanding {
    pub lifecycle: ScopeLifecycleId,
    pub generation: GrantGeneration,
    pub affiliation_generation: GrantGeneration,
    pub federated: bool,
    pub recorded: Affiliation,
    pub effective: Affiliation,
    pub issuance_basis: IssuanceBasis,
    projection_pending: bool,
}

/// The effective affiliations and exact scope generation read for the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceAffiliationListing {
    pub lifecycle: ScopeLifecycleId,
    pub generation: GrantGeneration,
    pub rows: Vec<(BareJid, Affiliation)>,
}

impl HostGrantStore {
    pub(super) async fn space_standing(
        &self,
        key: &ScopeKey,
        subject: &GrantSubject,
        attached: &AuthorityBinding,
    ) -> Result<Option<SpaceStanding>, HostGrantError> {
        let mut tx = self.begin_interactive_read().await?;
        let standing = space_standing_in_tx(&mut tx, key, subject, attached).await?;
        tx.rollback().await?;
        Ok(standing)
    }

    /// List the Space affiliations for an authenticated served account.
    /// The caller must fence the eventual response at the returned generation.
    pub async fn space_affiliations(
        &self,
        key: &ScopeKey,
        owner: &GrantSubject,
        attached: &AuthorityBinding,
    ) -> Result<Option<SpaceAffiliationListing>, HostGrantError> {
        let mut tx = self.begin_interactive_read().await?;
        let Some(standing) = space_standing_in_tx(&mut tx, key, owner, attached).await? else {
            return Ok(None);
        };
        if !standing.is_local_owner() {
            return Ok(None);
        }
        let affiliations = stored_space_affiliations(&mut tx, &standing)
            .await?
            .into_iter()
            .filter_map(|(subject, roles)| {
                let current = roles.current();
                (current != Affiliation::None).then_some((subject, current))
            })
            .collect();
        tx.rollback().await?;
        Ok(Some(SpaceAffiliationListing {
            lifecycle: standing.lifecycle,
            generation: standing.generation,
            rows: affiliations,
        }))
    }

    /// Classify requested changes against one fully projected canonical generation.
    /// The writer must use this lifecycle and generation as its compare-and-set.
    pub async fn plan_space_affiliation_changes(
        &self,
        key: &ScopeKey,
        owner: &GrantSubject,
        attached: &AuthorityBinding,
        policy: &IssuancePolicy,
        requested: &[(BareJid, Affiliation)],
    ) -> Result<Option<SpaceAffiliationPlan>, HostGrantError> {
        if requested.is_empty() || requested.len() > MAX_SPACE_AFFILIATION_CHANGES {
            return Err(HostGrantError::InvalidSpaceMembershipBatch);
        }
        let mut targets = BTreeMap::new();
        for (jid, affiliation) in requested {
            if CanonicalBareJid::new(jid.clone()).is_err()
                || targets.insert(jid.clone(), *affiliation).is_some()
            {
                return Err(HostGrantError::InvalidSpaceMembershipBatch);
            }
        }
        let mut tx = self.begin_interactive_read().await?;
        let Some(standing) = space_standing_in_tx(&mut tx, key, owner, attached).await? else {
            return Ok(None);
        };
        if !standing.is_local_owner()
            || !matches!(
                policy.check(owner, RoomAffiliation::Member),
                Ok(IssuanceBasis::Local)
            )
        {
            return Ok(None);
        }
        // Activation can change effective affiliations within one generation.
        if standing.projection_pending {
            return Err(HostGrantError::Busy);
        }
        let current = stored_space_affiliations(&mut tx, &standing)
            .await?
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let mut changes = Vec::new();
        let mut refused = Vec::new();
        for (jid, affiliation) in targets {
            let roles = current.get(&jid).copied().unwrap_or_default();
            if let Some(change) = plan_space_affiliation_change(&jid, affiliation, roles, policy) {
                changes.push(change);
            } else {
                refused.push((jid, roles.current()));
            }
        }
        tx.rollback().await?;
        Ok(Some(SpaceAffiliationPlan {
            lifecycle: standing.lifecycle,
            generation: standing.generation,
            changes,
            refused,
        }))
    }
}

/// An owner's requested changes and refused original affiliations at one generation.
#[derive(Clone, Debug)]
pub struct SpaceAffiliationPlan {
    pub lifecycle: ScopeLifecycleId,
    pub generation: GrantGeneration,
    pub changes: Vec<SpaceAffiliationChange>,
    pub refused: Vec<(BareJid, Affiliation)>,
}

impl SpaceStanding {
    fn is_local_owner(&self) -> bool {
        self.recorded == Affiliation::Owner
            && self.effective == Affiliation::Owner
            && self.issuance_basis == IssuanceBasis::Local
    }
}

#[derive(Clone, Copy)]
struct StoredSpaceAffiliation {
    recorded: Affiliation,
    effective: Affiliation,
}

impl Default for StoredSpaceAffiliation {
    fn default() -> Self {
        Self {
            recorded: Affiliation::None,
            effective: Affiliation::None,
        }
    }
}

impl StoredSpaceAffiliation {
    fn current(self) -> Affiliation {
        if self.recorded == Affiliation::Outcast {
            Affiliation::Outcast
        } else {
            self.effective
        }
    }
}

fn plan_space_affiliation_change(
    jid: &BareJid,
    requested: Affiliation,
    roles: StoredSpaceAffiliation,
    policy: &IssuancePolicy,
) -> Option<SpaceAffiliationChange> {
    if !matches!(roles.recorded, Affiliation::None | Affiliation::Member)
        || !matches!(roles.effective, Affiliation::None | Affiliation::Member)
    {
        return None;
    }
    let subject = GrantSubject::new(jid.clone()).ok()?;
    let affiliation = match requested {
        Affiliation::Member => {
            policy.check(&subject, RoomAffiliation::Member).ok()?;
            SpaceMembership::Member
        }
        Affiliation::None => SpaceMembership::None,
        _ => return None,
    };
    Some(SpaceAffiliationChange {
        subject,
        affiliation,
    })
}

async fn stored_space_affiliations(
    tx: &mut Transaction<'_>,
    standing: &SpaceStanding,
) -> Result<Vec<(BareJid, StoredSpaceAffiliation)>, HostGrantError> {
    let mut rows = tx
        .query(
            "SELECT subject_jid, affiliation, effective_affiliation, changed_generation
               FROM host_grant_space_affiliations WHERE lifecycle_id = ?
              ORDER BY subject_jid LIMIT ?",
            crate::db_params![
                standing.lifecycle.to_storage(),
                i64::try_from(HOST_GRANT_SCOPE_MAX_GRANT_ROWS + 1).expect("bounded Space grants"),
            ],
        )
        .await?;
    let mut affiliations = Vec::new();
    while let Some(row) = rows.next().await? {
        if affiliations.len() == HOST_GRANT_SCOPE_MAX_GRANT_ROWS {
            return Err(corrupt("Space affiliation count"));
        }
        let subject = GrantSubject::from_storage(&row.get::<String>(0)?)
            .ok_or(corrupt("Space affiliation subject"))?;
        let recorded = row
            .get::<String>(1)?
            .parse::<Affiliation>()
            .map_err(|_| corrupt("Space affiliation"))?;
        let effective = row
            .get::<String>(2)?
            .parse::<Affiliation>()
            .map_err(|_| corrupt("effective Space affiliation"))?;
        GrantGeneration::new(row.get::<i64>(3)?)
            .filter(|generation| *generation <= standing.generation)
            .ok_or(corrupt("Space affiliation generation"))?;
        affiliations.push((
            subject.jid().clone(),
            StoredSpaceAffiliation {
                recorded,
                effective,
            },
        ));
    }
    Ok(affiliations)
}

async fn space_standing_in_tx(
    tx: &mut Transaction<'_>,
    key: &ScopeKey,
    subject: &GrantSubject,
    attached: &AuthorityBinding,
) -> Result<Option<SpaceStanding>, HostGrantError> {
    if load_binding(tx, "host_grant_authority").await? != *attached {
        return Err(HostGrantError::IncarnationChanged);
    }
    let mut rows = tx.query(
        "SELECT b.lifecycle_id, b.federated, g.affiliation, g.effective_affiliation, g.issuance_basis, g.changed_generation \
         FROM host_grant_space_bindings b \
         JOIN host_grant_scope_lifecycles l ON l.lifecycle_id = b.lifecycle_id \
         JOIN pubsub_nodes p ON p.owner_jid = b.service_jid AND p.node_name = b.space_node \
         JOIN host_grant_space_affiliations g ON g.lifecycle_id = b.lifecycle_id \
         WHERE b.service_jid = ? AND b.space_node = ? AND g.subject_jid = ? \
         AND b.authority_incarnation = ? AND l.retired_at_ms IS NULL \
         AND p.access_model = b.access_model AND (p.node_type IS NULL OR p.node_type = ?)",
        crate::db_params![
            key.service().as_str(), key.node(), subject.as_str(),
            attached.incarnation.to_storage(), waddle_xmpp::xep::xep0503::NS_SPACES
        ],
    ).await?;
    let Some(row) = rows.next().await? else {
        drop(rows);
        return Ok(None);
    };
    let lifecycle =
        ScopeLifecycleId::from_storage(&row.get::<String>(0)?).ok_or(corrupt("Space lifecycle"))?;
    let federated = row.get(1)?;
    let recorded = row
        .get::<String>(2)?
        .parse()
        .map_err(|_| corrupt("Space affiliation"))?;
    let effective = row
        .get::<String>(3)?
        .parse()
        .map_err(|_| corrupt("effective Space affiliation"))?;
    let issuance_basis = IssuanceBasis::from_storage(&row.get::<String>(4)?)
        .ok_or(corrupt("Space issuance basis"))?;
    let affiliation_generation =
        GrantGeneration::new(row.get::<i64>(5)?).ok_or(corrupt("Space affiliation generation"))?;
    drop(rows);
    let scope = load_scope(tx, lifecycle, false)
        .await?
        .ok_or(corrupt("Space scope"))?;
    Ok(
        scope_admits(&scope, attached.incarnation).then_some(SpaceStanding {
            lifecycle,
            generation: scope.generation,
            affiliation_generation,
            federated,
            recorded,
            effective,
            issuance_basis,
            projection_pending: scope.projection_pending(),
        }),
    )
}

/// How a room is named when its binding is looked up.
#[derive(Clone, Copy, Debug)]
pub enum RoomBinding<'a> {
    /// The room's address. Bindings are keyed by the canonical form.
    Room(&'a BareJid),
    /// The room's authorization object.
    Channel(&'a str),
}

/// What the room bindings hold for a room.
#[derive(Clone, Debug)]
pub enum RoomLookup {
    /// No enrolled scope manages the room.
    Unmanaged,
    /// The address is not in the identity profile's canonical form, so
    /// whether a scope manages the room it reaches cannot be decided.
    NotCanonical,
    Managed(RoomGrants),
}

/// The canonical grants of one managed room, as one snapshot read them.
#[derive(Clone, Debug)]
pub struct RoomGrants {
    pub lifecycle: ScopeLifecycleId,
    /// The scope generation the snapshot saw.
    pub generation: GrantGeneration,
    pub room: BareJid,
    pub channel_id: String,
    /// Whether the scope admits anything: it is active under the current
    /// authority incarnation and policy version.
    pub scope_admits: bool,
    /// Recorded and effective affiliation by subject.
    grants: BTreeMap<String, (RoomAffiliation, RoomAffiliation)>,
}

impl RoomGrants {
    /// Whether `subject`, reaching the decision with `evidence`, may perform
    /// `action`, decided by the admission policy (`policy::grant_allows`)
    /// with the grants `principals` honours.
    pub fn allows(
        &self,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
        action: HostGrantAction,
        principals: &PrincipalPolicy,
    ) -> bool {
        self.scope_admits
            && self
                .grants
                .get(subject.as_str())
                .is_some_and(|(recorded, effective)| {
                    let witness = principals.witness(subject, evidence);
                    grant_allows(*recorded, *effective, &witness, action)
                })
    }

    /// The standing `subject`, reaching the decision with `evidence`, is
    /// honoured with: `Outcast` when its recorded or effective affiliation
    /// is outcast; otherwise its effective affiliation as admission honours
    /// it, or `None` when it holds no grant, the grant is not honoured or
    /// the scope admits nothing.
    pub fn standing(
        &self,
        subject: &BareJid,
        evidence: &PrincipalEvidence,
        principals: &PrincipalPolicy,
    ) -> RoomAffiliation {
        match self.grants.get(subject.as_str()) {
            None => RoomAffiliation::None,
            Some((RoomAffiliation::Outcast, _) | (_, RoomAffiliation::Outcast)) => {
                RoomAffiliation::Outcast
            }
            Some((_, effective)) if self.scope_admits => {
                honoured_affiliation(*effective, &principals.witness(subject, evidence))
            }
            Some(_) => RoomAffiliation::None,
        }
    }

    /// The subjects whose grants allow `action`, in canonical order. Each
    /// is decided without stream evidence: a listed subject is named, not
    /// requesting.
    pub fn admitted(&self, action: HostGrantAction, principals: &PrincipalPolicy) -> Vec<BareJid> {
        self.subjects()
            .filter(|subject| {
                self.allows(subject, &PrincipalEvidence::NoStream, action, principals)
            })
            .collect()
    }

    /// The affiliation recorded for `subject`: what the room's
    /// administration last set, whether or not it is effective yet.
    pub fn recorded_affiliation(&self, subject: &BareJid) -> RoomAffiliation {
        self.grants
            .get(subject.as_str())
            .map_or(RoomAffiliation::None, |(recorded, _)| *recorded)
    }

    /// The affiliation in effect for `subject`: the one admission honours.
    /// A recorded increase that is not yet effective does not show here.
    pub fn effective_affiliation(&self, subject: &BareJid) -> RoomAffiliation {
        self.grants
            .get(subject.as_str())
            .map_or(RoomAffiliation::None, |(_, effective)| *effective)
    }

    /// The subjects whose recorded affiliation is `affiliation`, in
    /// canonical order. A recorded increase that is not yet effective is
    /// listed; this is the room's list as administered, not as admitted.
    pub fn recorded(&self, affiliation: RoomAffiliation) -> Vec<BareJid> {
        self.subjects()
            .filter(|subject| {
                self.grants
                    .get(subject.as_str())
                    .is_some_and(|(recorded, _)| *recorded == affiliation)
            })
            .collect()
    }

    fn subjects(&self) -> impl Iterator<Item = BareJid> + '_ {
        self.grants
            .keys()
            .filter_map(|subject| subject.parse::<BareJid>().ok())
    }
}

impl HostGrantStore {
    /// The canonical grants of the room `binding` names, read in one bounded
    /// snapshot that takes no lock, under the interactive bounds (500 ms for
    /// a lock, 1 s for a statement).
    ///
    /// Whether a scope manages the room is decided before the address's
    /// form: an address that is not canonical is [`RoomLookup::NotCanonical`]
    /// only when its canonical form is bound to a room. One that is bound to
    /// none, or that has no canonical form, names no managed room.
    pub async fn room_grants(
        &self,
        binding: RoomBinding<'_>,
    ) -> Result<RoomLookup, HostGrantError> {
        let canonical_room;
        let mut not_canonical = false;
        let (column, key) = match binding {
            RoomBinding::Room(room) => {
                let canonical = match CanonicalBareJid::new(room.clone()) {
                    Ok(canonical) => canonical,
                    Err(_) => match CanonicalBareJid::canonicalize(room) {
                        Ok(canonical) => {
                            not_canonical = true;
                            canonical
                        }
                        Err(_) => return Ok(RoomLookup::Unmanaged),
                    },
                };
                canonical_room = canonical.into_bare_jid();
                ("room_jid", canonical_room.as_str())
            }
            RoomBinding::Channel(channel) => ("channel_id", channel),
        };
        let mut tx = self.begin_interactive_read().await?;
        if !relations_present(&mut tx).await? {
            tx.rollback().await?;
            return Ok(RoomLookup::Unmanaged);
        }
        let mut rows = tx
            .query(
                &format!(
                    "SELECT room_jid, channel_id, lifecycle_id FROM host_grant_rooms \
                     WHERE {column} = ?"
                ),
                crate::db_params![key],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            drop(rows);
            tx.rollback().await?;
            return Ok(RoomLookup::Unmanaged);
        };
        if not_canonical {
            drop(rows);
            tx.rollback().await?;
            return Ok(RoomLookup::NotCanonical);
        }
        let room: BareJid = row
            .get::<String>(0)?
            .parse()
            .map_err(|_| corrupt("room_jid"))?;
        let channel_id: String = row.get(1)?;
        let lifecycle = ScopeLifecycleId::from_storage(&row.get::<String>(2)?)
            .ok_or(corrupt("lifecycle_id"))?;
        drop(rows);
        let scope = load_scope(&mut tx, lifecycle, false)
            .await?
            .ok_or(corrupt("room scope"))?;
        let current = load_incarnation(&mut tx).await?;
        let mut rows = tx
            .query(
                "SELECT subject_jid, affiliation, effective_affiliation FROM host_grants \
                 WHERE lifecycle_id = ? AND room_jid = ?",
                crate::db_params![lifecycle.to_storage(), room.as_str()],
            )
            .await?;
        let mut grants = BTreeMap::new();
        while let Some(row) = rows.next().await? {
            grants.insert(
                row.get::<String>(0)?,
                (
                    affiliation_column(&row, 1, "affiliation")?,
                    affiliation_column(&row, 2, "effective_affiliation")?,
                ),
            );
        }
        drop(rows);
        tx.rollback().await?;
        Ok(RoomLookup::Managed(RoomGrants {
            lifecycle,
            generation: scope.generation,
            room,
            channel_id,
            scope_admits: scope_admits(&scope, current),
            grants,
        }))
    }

    /// Every object enrolled scopes manage, retired scopes included: a
    /// retired scope does not hand its rooms back to the legacy stores. One
    /// background snapshot ([`super::EnrolledScopes`] calls it); `None` when
    /// the schema holds no host-grant relation.
    pub async fn enrolled_set(&self) -> Result<Option<EnrolledSet>, HostGrantError> {
        let mut tx = self.begin_read().await?;
        if !relations_present(&mut tx).await? {
            tx.rollback().await?;
            return Ok(None);
        }
        let mut set = EnrolledSet::default();
        let mut rows = tx
            .query(
                "SELECT lifecycle_id, space_node FROM host_grant_scope_lifecycles",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let lifecycle = ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
                .ok_or(corrupt("lifecycle_id"))?;
            set.insert_scope(lifecycle, row.get::<String>(1)?);
        }
        drop(rows);
        let mut rows = tx
            .query(
                "SELECT room_jid, channel_id, lifecycle_id FROM host_grant_rooms",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let lifecycle = ScopeLifecycleId::from_storage(&row.get::<String>(2)?)
                .ok_or(corrupt("lifecycle_id"))?;
            set.insert_room(lifecycle, row.get::<String>(0)?, row.get::<String>(1)?);
        }
        drop(rows);
        tx.rollback().await?;
        Ok(Some(set))
    }
}

/// Whether the schema holds the host-grant relations. A schema built
/// without the migration catalog holds none and so no enrolled scope.
async fn relations_present(tx: &mut Transaction<'_>) -> Result<bool, HostGrantError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.to_regclass('host_grant_rooms') IS NOT NULL",
            (),
        )
        .await?;
    let present = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    drop(rows);
    Ok(present)
}
