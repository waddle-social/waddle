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
//! A database whose schema holds no host-grant relation holds no enrolled
//! scope, so every room is unmanaged there.

use std::collections::{BTreeMap, BTreeSet};

use jid::BareJid;
use waddle_xmpp::identity::CanonicalBareJid;

use super::error::HostGrantError;
use super::issuance::ServedAccountDomains;
use super::model::{GrantGeneration, RoomAffiliation, ScopeLifecycleId};
use super::policy::{grant_allows, honoured_affiliation, HostGrantAction};
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
    /// Whether `subject` may perform `action`, decided by the admission
    /// policy (`policy::grant_allows`) for a deployment serving `served`.
    pub fn allows(
        &self,
        subject: &BareJid,
        action: HostGrantAction,
        served: &ServedAccountDomains,
    ) -> bool {
        self.scope_admits
            && self
                .grants
                .get(subject.as_str())
                .is_some_and(|(recorded, effective)| {
                    grant_allows(*recorded, *effective, served.home_of(subject), action)
                })
    }

    /// The standing `subject` is honoured with: `Outcast` when its recorded
    /// or effective affiliation is outcast; otherwise its effective
    /// affiliation as admission honours it, or `None` when it holds no
    /// grant or the scope admits nothing.
    pub fn standing(&self, subject: &BareJid, served: &ServedAccountDomains) -> RoomAffiliation {
        match self.grants.get(subject.as_str()) {
            None => RoomAffiliation::None,
            Some((RoomAffiliation::Outcast, _) | (_, RoomAffiliation::Outcast)) => {
                RoomAffiliation::Outcast
            }
            Some((_, effective)) if self.scope_admits => {
                honoured_affiliation(*effective, served.home_of(subject))
            }
            Some(_) => RoomAffiliation::None,
        }
    }

    /// The subjects whose grants allow `action`, in canonical order.
    pub fn admitted(&self, action: HostGrantAction, served: &ServedAccountDomains) -> Vec<BareJid> {
        self.subjects()
            .filter(|subject| self.allows(subject, action, served))
            .collect()
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
    /// snapshot that takes no lock.
    pub async fn room_grants(
        &self,
        binding: RoomBinding<'_>,
    ) -> Result<RoomLookup, HostGrantError> {
        let canonical_room;
        let (column, key) = match binding {
            RoomBinding::Room(room) => {
                let Ok(room) = CanonicalBareJid::new(room.clone()) else {
                    return Ok(RoomLookup::NotCanonical);
                };
                canonical_room = room.into_bare_jid();
                ("room_jid", canonical_room.as_str())
            }
            RoomBinding::Channel(channel) => ("channel_id", channel),
        };
        let mut tx = self.begin_read().await?;
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

    /// Which of `channel_ids` are the authorization objects of rooms that
    /// enrolled scopes manage. A retired scope's rooms count: retirement
    /// does not hand a room back to the legacy stores.
    pub async fn enrolled_channels(
        &self,
        channel_ids: &[&str],
    ) -> Result<BTreeSet<String>, HostGrantError> {
        let mut enrolled = BTreeSet::new();
        if channel_ids.is_empty() {
            return Ok(enrolled);
        }
        let mut tx = self.begin_read().await?;
        if !relations_present(&mut tx).await? {
            tx.rollback().await?;
            return Ok(enrolled);
        }
        for chunk in channel_ids.chunks(ENROLLED_CHANNEL_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut rows = tx
                .query(
                    &format!(
                        "SELECT channel_id FROM host_grant_rooms \
                         WHERE channel_id IN ({placeholders})"
                    ),
                    chunk
                        .iter()
                        .map(|id| crate::db::Value::from(*id))
                        .collect::<Vec<_>>(),
                )
                .await?;
            while let Some(row) = rows.next().await? {
                enrolled.insert(row.get::<String>(0)?);
            }
        }
        tx.rollback().await?;
        Ok(enrolled)
    }

    /// Whether `node` is the Space node of an enrolled scope, retired or
    /// not.
    pub async fn enrolled_space_node(&self, node: &str) -> Result<bool, HostGrantError> {
        let mut tx = self.begin_read().await?;
        if !relations_present(&mut tx).await? {
            tx.rollback().await?;
            return Ok(false);
        }
        let mut rows = tx
            .query(
                "SELECT 1 FROM host_grant_scope_lifecycles WHERE space_node = ? LIMIT 1",
                crate::db_params![node],
            )
            .await?;
        let enrolled = rows.next().await?.is_some();
        drop(rows);
        tx.rollback().await?;
        Ok(enrolled)
    }
}

/// Most channel identifiers one statement of [`HostGrantStore::enrolled_channels`] binds.
const ENROLLED_CHANNEL_CHUNK: usize = 256;

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
