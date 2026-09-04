use jid::{BareJid, FullJid};

use super::affiliation::{self, AffiliationChange, AffiliationProvenance};
use super::room::{MucRoom, Occupant};
use super::room_actor::OccupancyWatermark;
use crate::types::{Affiliation, Role};
use waddle_xmpp_core::OccupancySessionGeneration;

impl MucRoom {
    /// Get the affiliation for a JID.
    pub fn get_affiliation(&self, jid: &BareJid) -> Affiliation {
        self.affiliation_list.get(jid)
    }

    /// Set the affiliation for a JID.
    ///
    /// Returns the change if the affiliation actually changed.
    /// Also updates any occupant with this JID, re-deriving their
    /// XEP-0045 role from the new affiliation. The role MUST track
    /// the affiliation here — XEP-0045 §5.1.3 says a server "will
    /// change the user's role in any room they are currently in"
    /// on an affiliation change, and several push-decision sites
    /// (XEP-0513 §"Multi-User Chats Permissions" §304 channel-
    /// broadcast gate, XEP-0045 §7.5 visitor-can-send gate) read
    /// `Occupant.role` as the source of truth — without re-deriving
    /// here, a demoted Owner / Admin would briefly retain
    /// `Role::Moderator` until they cycle presence, silently
    /// preserving their broadcast capabilities (adversarial review
    /// P2 on PR #738).
    pub fn set_affiliation(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
    ) -> Option<AffiliationChange> {
        self.set_affiliation_with_provenance(jid, affiliation, AffiliationProvenance::ExplicitGrant)
    }

    /// Set the affiliation for a JID, recording where the value came
    /// from (#1110). Explicit grants pin the room actor in memory
    /// (`MucRoom::is_dormant` stays false); resolver-derived entries
    /// are reconstructible on the next join and do not.
    pub fn set_affiliation_with_provenance(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
        provenance: AffiliationProvenance,
    ) -> Option<AffiliationChange> {
        let change =
            self.affiliation_list
                .set_with_provenance(jid.clone(), affiliation, provenance);

        if change.is_some() {
            let role = self.derive_role_from_affiliation(affiliation);
            for occupant in self.occupants.values_mut() {
                if occupant.real_jid.to_bare() == jid {
                    occupant.affiliation = affiliation;
                    occupant.role = role;
                }
            }
        }

        change
    }

    /// Sync an occupant's affiliation from the persistent list.
    ///
    /// Call this when an occupant joins to ensure their affiliation
    /// matches the stored value.
    pub fn sync_occupant_affiliation(&mut self, nick: &str) -> Option<Affiliation> {
        if let Some(occupant) = self.occupants.get_mut(nick) {
            let stored = self.affiliation_list.get(&occupant.real_jid.to_bare());
            if occupant.affiliation != stored {
                occupant.affiliation = stored;
            }
            Some(stored)
        } else {
            None
        }
    }

    /// Get all JIDs with a specific affiliation.
    pub fn get_jids_by_affiliation(&self, affiliation: Affiliation) -> Vec<BareJid> {
        self.affiliation_list.by_affiliation(affiliation)
    }

    /// Get all affiliation entries for the room.
    pub fn get_all_affiliations(&self) -> Vec<affiliation::AffiliationEntry> {
        self.affiliation_list.all()
    }

    /// Check if a JID has at least the specified affiliation.
    pub fn has_affiliation_at_least(&self, jid: &BareJid, min: Affiliation) -> bool {
        self.affiliation_list.has_at_least(jid, min)
    }

    /// Check if a user can join this room based on affiliation.
    ///
    /// For members-only rooms and group DMs, users need at least Member
    /// affiliation. The group-DM check is intentionally redundant with
    /// config normalization so malformed restored state fails closed.
    pub fn can_user_join(&self, jid: &BareJid) -> bool {
        if !self.config.requires_membership() {
            self.get_affiliation(jid) != Affiliation::Outcast
        } else {
            self.has_affiliation_at_least(jid, Affiliation::Member)
        }
    }

    /// Derive the initial role for a user based on their affiliation.
    ///
    /// Per XEP-0045:
    /// - Owner/Admin -> Moderator role
    /// - Member -> Participant role (in moderated rooms, may be Visitor otherwise)
    /// - None -> Participant (if allowed) or Visitor
    pub fn derive_role_from_affiliation(&self, affiliation: Affiliation) -> Role {
        match affiliation {
            Affiliation::Owner | Affiliation::Admin => Role::Moderator,
            Affiliation::Member => Role::Participant,
            Affiliation::None => {
                if self.config.moderated {
                    Role::Visitor
                } else {
                    Role::Participant
                }
            }
            Affiliation::Outcast => Role::None,
        }
    }

    /// Add an occupant with affiliation looked up from the list.
    ///
    /// This is the preferred way to add occupants as it ensures
    /// affiliation consistency.
    pub fn add_occupant_with_affiliation(
        &mut self,
        real_jid: FullJid,
        nick: String,
        local_domain: Option<&str>,
        watermark: OccupancyWatermark,
        session: OccupancySessionGeneration,
    ) -> &Occupant {
        if let Some(existing) = self.occupants.get(&nick) {
            if existing.real_jid.to_bare() == real_jid.to_bare() {
                let sessions = self
                    .occupant_sessions
                    .entry(nick.clone())
                    .or_insert_with(|| vec![existing.real_jid.clone()]);
                if !sessions.iter().any(|session| session == &real_jid) {
                    sessions.push(real_jid.clone());
                }
                self.set_session_watermark(real_jid.clone(), watermark);
                self.set_session_generation(&real_jid, session);
                return self
                    .occupants
                    .get(&nick)
                    .expect("occupant exists while adding same-bare session");
            }
        }

        let bare_jid = real_jid.to_bare();
        let affiliation = self.affiliation_list.get(&bare_jid);
        let role = self.derive_role_from_affiliation(affiliation);

        let (is_remote, home_server) = match local_domain {
            Some(domain) => {
                let jid_domain = real_jid.domain().as_str();
                let remote = jid_domain != domain;
                let server = if remote {
                    Some(jid_domain.to_string())
                } else {
                    None
                };
                (remote, server)
            }
            None => (false, None),
        };

        let occupant = Occupant {
            real_jid: real_jid.clone(),
            nick: nick.clone(),
            role,
            affiliation,
            is_remote,
            home_server,
        };

        let floor = self.generation_floor;
        let gen = self
            .nickname_generation
            .entry(nick.clone())
            .or_insert(floor);
        *gen += 1;

        self.set_session_watermark(real_jid.clone(), watermark);
        self.set_session_generation(&real_jid, session);
        self.occupant_sessions.insert(nick.clone(), vec![real_jid]);
        self.occupants.insert(nick.clone(), occupant);
        self.occupants
            .get(&nick)
            .expect("occupant just inserted on previous line")
    }

    /// Update affiliations from the join-path authz resolver.
    ///
    /// Recorded as [`AffiliationProvenance::ResolverDerived`] (#1110):
    /// the resolver re-derives this value on the next join of the same
    /// bare JID, so the entry never blocks room dormancy.
    pub fn update_affiliation_from_resolver(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
    ) -> Option<AffiliationChange> {
        self.set_affiliation_with_provenance(
            jid,
            affiliation,
            AffiliationProvenance::ResolverDerived,
        )
    }

    /// Check if the room has at least one owner.
    pub fn has_owner(&self) -> bool {
        self.affiliation_list.has_owner()
    }

    /// Replace the entire in-memory affiliation list with `entries`
    /// (ADR-0017 Phase 3 Slice 7 restore-before-join path): used when a
    /// freshly spawned or newly-claimed `RoomActor` restores durable
    /// affiliation state from Postgres. Every occupant's role is
    /// re-derived exactly like [`Self::set_affiliation`] — restore only
    /// ever runs before any join for the current actor incarnation (see
    /// `RoomActor`'s `RestoreDurableRoomState` handler), so
    /// `self.occupants` is always empty at the point this runs, but the
    /// re-derivation stays defensive if that invariant is ever violated.
    pub fn restore_affiliations(&mut self, entries: Vec<affiliation::AffiliationEntry>) {
        for entry in entries {
            self.set_affiliation(entry.jid, entry.affiliation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muc::room::RoomConfig;

    fn bare(value: &str) -> BareJid {
        value.parse().expect("valid bare JID")
    }

    fn full(value: &str) -> FullJid {
        value.parse().expect("valid full JID")
    }

    /// XEP-0045 §5.1.3: changing a user's affiliation MUST also update
    /// their per-session role in any room they are currently in.
    /// Without this sync, push-decision gates that read `Occupant.role`
    /// (XEP-0513 §"Multi-User Chats Permissions" §304 channel-broadcast
    /// gate, XEP-0045 §7.5 visitor-can-send gate) would honor a stale
    /// role from before the demotion — silently preserving the demoted
    /// user's broadcast capability until they next rejoin (adversarial
    /// review P2 on PR #738).
    #[test]
    fn set_affiliation_resyncs_occupant_role_on_demotion() {
        let mut room = MucRoom::new(
            bare("team@conf.example.com"),
            "waddle-id".to_string(),
            "channel-id".to_string(),
            RoomConfig::default(),
        );
        let owner_bare = bare("alice@example.com");
        room.affiliation_list
            .set(owner_bare.clone(), Affiliation::Owner);
        room.add_occupant_with_affiliation(
            full("alice@example.com/web"),
            "alice".to_string(),
            None,
            OccupancyWatermark::initial(),
            OccupancySessionGeneration::mint(),
        );

        let alice = room.occupants.get("alice").expect("alice joined");
        assert_eq!(alice.affiliation, Affiliation::Owner);
        assert_eq!(alice.role, Role::Moderator);

        // Demote to Member — role MUST track affiliation.
        room.set_affiliation(owner_bare.clone(), Affiliation::Member);
        let alice = room.occupants.get("alice").expect("still in room");
        assert_eq!(alice.affiliation, Affiliation::Member);
        assert_eq!(
            alice.role,
            Role::Participant,
            "Role::Moderator MUST be re-derived to Role::Participant on \
             Owner→Member demotion; a stale role would silently preserve \
             channel-broadcast permission for a demoted user"
        );

        // Re-promote to Admin — role tracks back up.
        room.set_affiliation(owner_bare, Affiliation::Admin);
        let alice = room.occupants.get("alice").expect("still in room");
        assert_eq!(alice.affiliation, Affiliation::Admin);
        assert_eq!(alice.role, Role::Moderator);
    }

    /// Banning (Outcast) MUST drop `Role::None` per
    /// `derive_role_from_affiliation`. The occupant entry remains in
    /// the room map until the kick handler removes it; the role
    /// downgrade here is the in-place demotion that gates further
    /// sends — particularly the XEP-0513 channel-broadcast gate.
    #[test]
    fn set_affiliation_to_outcast_zeroes_role() {
        let mut room = MucRoom::new(
            bare("team@conf.example.com"),
            "waddle-id".to_string(),
            "channel-id".to_string(),
            RoomConfig::default(),
        );
        let bob_bare = bare("bob@example.com");
        room.affiliation_list
            .set(bob_bare.clone(), Affiliation::Member);
        room.add_occupant_with_affiliation(
            full("bob@example.com/desk"),
            "bob".to_string(),
            None,
            OccupancyWatermark::initial(),
            OccupancySessionGeneration::mint(),
        );
        assert_eq!(
            room.occupants.get("bob").expect("bob joined").role,
            Role::Participant
        );

        room.set_affiliation(bob_bare, Affiliation::Outcast);
        let bob = room.occupants.get("bob").expect("still in occupant map");
        assert_eq!(bob.affiliation, Affiliation::Outcast);
        assert_eq!(bob.role, Role::None);
    }
}
