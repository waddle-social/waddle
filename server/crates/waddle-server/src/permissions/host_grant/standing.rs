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

use super::enrolled::EnrolledSet;
use super::error::HostGrantError;
use super::model::{GrantGeneration, RoomAffiliation, ScopeLifecycleId};
use super::policy::{grant_allows, honoured_affiliation, HostGrantAction};
use super::principal::{PrincipalEvidence, PrincipalPolicy};
use super::store::{
    affiliation_column, corrupt, load_incarnation, load_scope, scope_admits, HostGrantStore,
};
use crate::db::Transaction;

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
