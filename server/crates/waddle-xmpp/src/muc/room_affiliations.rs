use jid::{BareJid, FullJid};

use super::affiliation::{self, AffiliationChange};
use super::room::{MucRoom, Occupant};
use crate::types::{Affiliation, Role};

impl MucRoom {
    /// Get the affiliation for a JID.
    pub fn get_affiliation(&self, jid: &BareJid) -> Affiliation {
        self.affiliation_list.get(jid)
    }

    /// Set the affiliation for a JID.
    ///
    /// Returns the change if the affiliation actually changed.
    /// Also updates any occupant with this JID.
    pub fn set_affiliation(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
    ) -> Option<AffiliationChange> {
        let change = self.affiliation_list.set(jid.clone(), affiliation);

        if change.is_some() {
            for occupant in self.occupants.values_mut() {
                if occupant.real_jid.to_bare() == jid {
                    occupant.affiliation = affiliation;
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
    /// For members-only rooms, users need at least Member affiliation.
    pub fn can_user_join(&self, jid: &BareJid) -> bool {
        if !self.config.members_only {
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
    ) -> &Occupant {
        if let Some(existing) = self.occupants.get(&nick) {
            if existing.real_jid.to_bare() == real_jid.to_bare() {
                let sessions = self
                    .occupant_sessions
                    .entry(nick.clone())
                    .or_insert_with(|| vec![existing.real_jid.clone()]);
                if !sessions.iter().any(|session| session == &real_jid) {
                    sessions.push(real_jid);
                }
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

        self.occupant_sessions.insert(nick.clone(), vec![real_jid]);
        self.occupants.insert(nick.clone(), occupant);
        self.occupants
            .get(&nick)
            .expect("occupant just inserted on previous line")
    }

    /// Update affiliations from a resolver (async operation).
    pub fn update_affiliation_from_resolver(
        &mut self,
        jid: BareJid,
        affiliation: Affiliation,
    ) -> Option<AffiliationChange> {
        self.set_affiliation(jid, affiliation)
    }

    /// Check if the room has at least one owner.
    pub fn has_owner(&self) -> bool {
        self.affiliation_list.has_owner()
    }
}
