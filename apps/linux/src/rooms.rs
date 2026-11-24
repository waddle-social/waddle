//! Room rules without widgets: addresses for new rooms, what an edit
//! changes, who may do what to whom in a room (XEP-0045 §5.2, §8.2, §9.1),
//! the merged member list, and what a group DM needs before it is created.
//!
//! The server decides every one of these requests; the rules here only
//! decide what the dialogs offer, so nobody is shown a button the server
//! will refuse for a reason the client already knows.

use std::cmp::Reverse;
use std::collections::HashMap;

use crate::session::{Conversation, Flavor, Kind};

/// XEP-0045 §5.2 affiliation, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Affiliation {
    Owner,
    Admin,
    Member,
    None,
    /// Banned (§9.1).
    Outcast,
}

impl Affiliation {
    pub const ALL: [Self; 5] = [
        Self::Owner,
        Self::Admin,
        Self::Member,
        Self::None,
        Self::Outcast,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Owner => "Owner",
            Self::Admin => "Admin",
            Self::Member => "Member",
            Self::None => "Guest",
            Self::Outcast => "Banned",
        }
    }

    /// Order of authority; a banned person ranks below everyone.
    fn rank(self) -> u8 {
        match self {
            Self::Owner => 4,
            Self::Admin => 3,
            Self::Member => 2,
            Self::None => 1,
            Self::Outcast => 0,
        }
    }
}

/// XEP-0045 §5.1 role: what a present occupant may do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Moderator,
    Participant,
    Visitor,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Moderator => "Moderator",
            Self::Participant => "Participant",
            Self::Visitor => "Visitor",
        }
    }
}

/// What this account is in a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    pub affiliation: Affiliation,
    pub role: Option<Role>,
}

impl Standing {
    pub fn is_owner(self) -> bool {
        self.affiliation == Affiliation::Owner
    }

    pub fn is_admin(self) -> bool {
        matches!(self.affiliation, Affiliation::Owner | Affiliation::Admin)
    }

    /// May eject occupants: a moderator role, or admin and owner
    /// affiliations (which carry it).
    pub fn can_moderate(self) -> bool {
        self.is_admin() || self.role == Some(Role::Moderator)
    }
}

/// One line of an affiliation list (§9.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affiliated {
    pub jid: String,
    pub nick: Option<String>,
    pub affiliation: Affiliation,
    pub reason: Option<String>,
}

/// An occupant seen in the room's presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    pub nick: String,
    /// Real bare JID, when the room exposes it.
    pub jid: Option<String>,
    pub affiliation: Option<Affiliation>,
    pub role: Option<Role>,
    /// XEP-0317 hat titles.
    pub hats: Vec<String>,
}

/// A person in the members list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub jid: Option<String>,
    /// The nick to kick by, while they are in the room.
    pub nick: Option<String>,
    pub name: String,
    pub affiliation: Affiliation,
    pub role: Option<Role>,
    pub hats: Vec<String>,
    pub here: bool,
    pub reason: Option<String>,
}

/// Affiliation lists and present occupants as one list: the strongest
/// affiliation first, then those here, then by name. An occupant is matched
/// to an affiliation by real JID, else by nick. Banned people are only
/// listed when `include_banned`.
pub fn merge_people(
    lists: &[Affiliated],
    occupants: &[Occupant],
    include_banned: bool,
) -> Vec<Person> {
    let mut people: Vec<Person> = Vec::new();
    let mut taken = vec![false; occupants.len()];
    for entry in lists {
        if entry.affiliation == Affiliation::Outcast && !include_banned {
            continue;
        }
        let here = occupants.iter().enumerate().find(|(index, occupant)| {
            !taken[*index]
                && (occupant
                    .jid
                    .as_deref()
                    .is_some_and(|jid| jid.eq_ignore_ascii_case(&entry.jid))
                    || (occupant.jid.is_none()
                        && entry.nick.as_deref() == Some(occupant.nick.as_str())))
        });
        if let Some((index, _)) = here {
            taken[index] = true;
        }
        let occupant = here.map(|(_, occupant)| occupant);
        people.push(Person {
            jid: Some(entry.jid.clone()),
            nick: occupant.map(|occupant| occupant.nick.clone()),
            name: occupant
                .map(|occupant| occupant.nick.clone())
                .or_else(|| entry.nick.clone())
                .unwrap_or_else(|| local_part(&entry.jid)),
            affiliation: entry.affiliation,
            role: occupant.and_then(|occupant| occupant.role),
            hats: occupant.map(|o| o.hats.clone()).unwrap_or_default(),
            here: occupant.is_some(),
            reason: entry.reason.clone(),
        });
    }
    for (index, occupant) in occupants.iter().enumerate() {
        if taken[index] {
            continue;
        }
        let affiliation = occupant.affiliation.unwrap_or(Affiliation::None);
        if affiliation == Affiliation::Outcast {
            continue;
        }
        people.push(Person {
            jid: occupant.jid.clone(),
            nick: Some(occupant.nick.clone()),
            name: occupant.nick.clone(),
            affiliation,
            role: occupant.role,
            hats: occupant.hats.clone(),
            here: true,
            reason: None,
        });
    }
    people.sort_by_cached_key(|person| {
        (
            Reverse(person.affiliation.rank()),
            Reverse(person.here),
            person.name.to_lowercase(),
        )
    });
    people
}

fn local_part(jid: &str) -> String {
    jid.split('@').next().unwrap_or(jid).to_string()
}

/// What the members list can do to one person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Change the affiliation (a promotion, demotion, removal or unban).
    SetAffiliation(Affiliation),
    /// Eject now; they may come back (§8.2).
    Kick,
    /// Ban (§9.1), after confirmation.
    Ban,
}

/// The actions `me` may be offered for `target`. Nothing for oneself.
pub fn actions_for(me: Standing, target: &Person, own_jid: &str) -> Vec<Action> {
    let own = target
        .jid
        .as_deref()
        .is_some_and(|jid| jid.eq_ignore_ascii_case(own_jid));
    if own {
        return Vec::new();
    }
    let mut actions = Vec::new();
    let Some(_) = target.jid.as_ref() else {
        // Without a real JID only an ejection by nick is possible.
        if me.can_moderate() && target.here && target.affiliation.rank() < Affiliation::Admin.rank()
        {
            actions.push(Action::Kick);
        }
        return actions;
    };
    // Owners change anything; admins only people who are neither admin
    // nor owner (§5.2.1 and §5.2.2 matrix).
    let may_change = me.is_owner()
        || (me.affiliation == Affiliation::Admin
            && !matches!(target.affiliation, Affiliation::Owner | Affiliation::Admin));
    if may_change {
        let choices: &[Affiliation] = if me.is_owner() {
            &[
                Affiliation::Owner,
                Affiliation::Admin,
                Affiliation::Member,
                Affiliation::None,
            ]
        } else {
            &[Affiliation::Member, Affiliation::None]
        };
        for choice in choices {
            if *choice == target.affiliation {
                continue;
            }
            // A guest is already "none"; unbanning ends at none too.
            actions.push(Action::SetAffiliation(*choice));
        }
    }
    if me.can_moderate()
        && target.here
        && target.nick.is_some()
        && (me.is_owner() || target.affiliation.rank() < Affiliation::Admin.rank())
    {
        actions.push(Action::Kick);
    }
    if may_change && target.affiliation != Affiliation::Outcast {
        actions.push(Action::Ban);
    }
    actions
}

/// Menu text for an action.
pub fn action_label(action: Action, target: &Person) -> String {
    match action {
        Action::SetAffiliation(Affiliation::None) if target.affiliation == Affiliation::Outcast => {
            "Unban".to_string()
        }
        Action::SetAffiliation(Affiliation::None) => "Remove from Members".to_string(),
        Action::SetAffiliation(affiliation) => format!("Make {}", affiliation.label()),
        Action::Kick => "Kick".to_string(),
        Action::Ban => "Ban".to_string(),
    }
}

/// An action that asks first, and what it says.
pub fn confirmation(action: Action, name: &str) -> Option<(String, String, &'static str)> {
    match action {
        Action::Kick => Some((
            format!("Kick {name}?"),
            "They are removed from the room now and can come back.".to_string(),
            "Kick",
        )),
        Action::Ban => Some((
            format!("Ban {name}?"),
            "They are removed and cannot join the room again until you unban them.".to_string(),
            "Ban",
        )),
        Action::SetAffiliation(Affiliation::Owner) => Some((
            format!("Make {name} an Owner?"),
            "Owners can change the room's settings, change other owners and delete the room."
                .to_string(),
            "Make Owner",
        )),
        Action::SetAffiliation(_) => None,
    }
}

/// An address for a room named `name`: lowercase letters, digits and
/// hyphens. `None` when nothing usable is left.
pub fn address_for(name: &str) -> Option<String> {
    let mut slug = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(48).collect();
    let slug = slug.trim_end_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

/// What a room's owner can set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub name: String,
    pub description: String,
    pub members_only: bool,
    /// Listed in the room directory.
    pub public: bool,
    pub forum: bool,
}

/// A change to a room's settings: only what differs, `None` otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    pub name: Option<String>,
    pub description: Option<String>,
    pub members_only: Option<bool>,
    pub public: Option<bool>,
    pub forum: Option<bool>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What differs between the settings the room reported and the ones the
/// owner wants. Whitespace around the name and description is dropped.
pub fn changes(before: &Settings, after: &Settings) -> Changes {
    let name = after.name.trim();
    let description = after.description.trim();
    Changes {
        name: (name != before.name.trim() && !name.is_empty()).then(|| name.to_string()),
        description: (description != before.description.trim()).then(|| description.to_string()),
        members_only: (after.members_only != before.members_only).then_some(after.members_only),
        public: (after.public != before.public).then_some(after.public),
        forum: (after.forum != before.forum).then_some(after.forum),
    }
}

/// Why a name cannot be used for a new room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    NoAddress,
    Taken,
}

impl NameProblem {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Give the room a name.",
            Self::NoAddress => "Use letters or digits in the name.",
            Self::Taken => "A room with that name already exists.",
        }
    }
}

/// The address a new room named `name` gets, unless that is not possible.
/// `existing` are the bare JIDs of rooms already known.
pub fn new_room_address(name: &str, existing: &[String]) -> Result<String, NameProblem> {
    if name.trim().is_empty() {
        return Err(NameProblem::Empty);
    }
    let address = address_for(name).ok_or(NameProblem::NoAddress)?;
    let taken = existing.iter().any(|jid| {
        jid.split('@')
            .next()
            .is_some_and(|node| node.eq_ignore_ascii_case(&address))
    });
    if taken {
        Err(NameProblem::Taken)
    } else {
        Ok(address)
    }
}

/// Why a group DM cannot be created yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupProblem {
    NeedsPeople,
}

impl GroupProblem {
    pub fn message(self) -> &'static str {
        "Pick at least one other person."
    }
}

/// The member list to send: the account first, then the chosen people
/// once each (by bare JID, ignoring case). The server needs at least two.
pub fn group_members(own: &str, picked: &[String]) -> Result<Vec<String>, GroupProblem> {
    let mut members = vec![own.to_string()];
    for jid in picked {
        if !members.iter().any(|known| known.eq_ignore_ascii_case(jid)) {
            members.push(jid.clone());
        }
    }
    if members.len() < 2 {
        Err(GroupProblem::NeedsPeople)
    } else {
        Ok(members)
    }
}

/// A group DM's name: the chosen one, else the people in it.
pub fn group_title(name: &str, members: &[String], own: &str) -> String {
    let name = name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    let others: Vec<String> = members
        .iter()
        .filter(|jid| !jid.eq_ignore_ascii_case(own))
        .map(|jid| local_part(jid))
        .collect();
    if others.is_empty() {
        "Group chat".to_string()
    } else {
        others.join(", ")
    }
}

/// People the directory found that can still be added: not the account,
/// not already chosen or in the group.
pub fn addable<'a, T>(
    found: &'a [T],
    jid_of: impl Fn(&T) -> &str,
    own: &str,
    excluded: &[String],
) -> Vec<&'a T> {
    found
        .iter()
        .filter(|person| {
            let jid = jid_of(person);
            !jid.eq_ignore_ascii_case(own)
                && !excluded.iter().any(|known| known.eq_ignore_ascii_case(jid))
        })
        .collect()
}

/// Who is in a room, from its presence: nick to occupant, so a later
/// presence replaces an earlier one and a departure removes it.
#[derive(Debug, Default, Clone)]
pub struct Roster {
    occupants: HashMap<String, Occupant>,
}

impl Roster {
    pub fn set(&mut self, occupant: Occupant) {
        self.occupants.insert(occupant.nick.clone(), occupant);
    }

    pub fn remove(&mut self, nick: &str) {
        self.occupants.remove(nick);
    }

    pub fn occupants(&self) -> Vec<Occupant> {
        let mut all: Vec<Occupant> = self.occupants.values().cloned().collect();
        all.sort_by(|a, b| a.nick.cmp(&b.nick));
        all
    }
}

/// Where a new conversation goes in the sidebar order: rooms, then group
/// DMs, then DMs; after the last of its own group and past that room's
/// thread rows.
pub fn placement(existing: &[Conversation], flavor: Flavor) -> usize {
    let belongs = |conversation: &Conversation| {
        conversation.thread.is_none()
            && conversation.kind == Kind::Room
            && (flavor == Flavor::Group || conversation.flavor != Flavor::Group)
    };
    let Some(last) = existing.iter().rposition(belongs) else {
        return 0;
    };
    let mut index = last + 1;
    while existing
        .get(index)
        .is_some_and(|conversation| conversation.thread.is_some())
    {
        index += 1;
    }
    index
}

/// A message for a failed request, from the server's stanza condition.
pub fn failure_text(action: &str, condition: Option<&str>) -> String {
    match condition {
        Some("forbidden" | "not-allowed") => {
            format!("{action}: the room does not allow you to.")
        }
        Some("conflict") => format!("{action}: that conflicts with what the room already has."),
        Some("item-not-found") => format!("{action}: the room or person was not found."),
        Some("service-unavailable") => format!("{action}: the server does not offer that."),
        Some(other) => format!("{action}: the server answered {other}."),
        None => format!("{action} did not finish. Try again."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "me@example.com";

    fn standing(affiliation: Affiliation, role: Option<Role>) -> Standing {
        Standing { affiliation, role }
    }

    fn person(jid: &str, affiliation: Affiliation, here: bool) -> Person {
        Person {
            jid: Some(jid.to_string()),
            nick: here.then(|| local_part(jid)),
            name: local_part(jid),
            affiliation,
            role: None,
            hats: Vec::new(),
            here,
            reason: None,
        }
    }

    #[test]
    fn addresses_are_lowercase_slugs() {
        assert_eq!(address_for("  Team Chat! "), Some("team-chat".into()));
        assert_eq!(address_for("a__b"), Some("a-b".into()));
        assert_eq!(address_for("***"), None);
        assert_eq!(address_for("Ünï"), Some("n".into()));
        assert_eq!(address_for(&"x".repeat(80)).map(|a| a.len()), Some(48));
    }

    #[test]
    fn new_rooms_cannot_reuse_an_address() {
        let rooms = vec!["general@muc.example.com".to_string()];
        assert_eq!(new_room_address(" ", &rooms), Err(NameProblem::Empty));
        assert_eq!(new_room_address("!!", &rooms), Err(NameProblem::NoAddress));
        assert_eq!(new_room_address("General", &rooms), Err(NameProblem::Taken));
        assert_eq!(new_room_address("Random", &rooms), Ok("random".into()));
    }

    #[test]
    fn changes_name_only_what_differs() {
        let before = Settings {
            name: "General".into(),
            description: "Hello".into(),
            members_only: false,
            public: true,
            forum: false,
        };
        assert!(changes(&before, &before).is_empty());
        let after = Settings {
            name: " General ".into(),
            description: "".into(),
            members_only: true,
            ..before.clone()
        };
        let diff = changes(&before, &after);
        assert_eq!(diff.name, None);
        assert_eq!(diff.description, Some(String::new()));
        assert_eq!(diff.members_only, Some(true));
        assert_eq!(diff.public, None);
        assert_eq!(diff.forum, None);
        let blank = Settings {
            name: " ".into(),
            ..before.clone()
        };
        assert_eq!(changes(&before, &blank).name, None);
    }

    #[test]
    fn owners_may_change_anyone_but_themselves() {
        let owner = standing(Affiliation::Owner, Some(Role::Moderator));
        let admin = person("a@example.com", Affiliation::Admin, true);
        let actions = actions_for(owner, &admin, ME);
        assert!(actions.contains(&Action::SetAffiliation(Affiliation::Owner)));
        assert!(actions.contains(&Action::SetAffiliation(Affiliation::Member)));
        assert!(!actions.contains(&Action::SetAffiliation(Affiliation::Admin)));
        assert!(actions.contains(&Action::Kick));
        assert!(actions.contains(&Action::Ban));
        assert!(actions_for(owner, &person(ME, Affiliation::Owner, true), ME).is_empty());
    }

    #[test]
    fn admins_leave_owners_and_admins_alone() {
        let admin = standing(Affiliation::Admin, Some(Role::Moderator));
        let owner = person("o@example.com", Affiliation::Owner, true);
        assert!(actions_for(admin, &owner, ME).is_empty());
        let peer = person("p@example.com", Affiliation::Admin, true);
        assert!(actions_for(admin, &peer, ME).is_empty());
        let guest = person("g@example.com", Affiliation::None, true);
        let actions = actions_for(admin, &guest, ME);
        assert_eq!(
            actions,
            vec![
                Action::SetAffiliation(Affiliation::Member),
                Action::Kick,
                Action::Ban
            ]
        );
    }

    #[test]
    fn moderators_only_kick_and_only_people_here() {
        let moderator = standing(Affiliation::None, Some(Role::Moderator));
        let away = person("g@example.com", Affiliation::Member, false);
        assert!(actions_for(moderator, &away, ME).is_empty());
        let here = person("g@example.com", Affiliation::Member, true);
        assert_eq!(actions_for(moderator, &here, ME), vec![Action::Kick]);
        let admin = person("a@example.com", Affiliation::Admin, true);
        assert!(actions_for(moderator, &admin, ME).is_empty());
        let plain = standing(Affiliation::Member, Some(Role::Participant));
        assert!(actions_for(plain, &here, ME).is_empty());
    }

    #[test]
    fn a_banned_person_can_be_unbanned_not_banned_again() {
        let admin = standing(Affiliation::Admin, Some(Role::Moderator));
        let banned = person("b@example.com", Affiliation::Outcast, false);
        let actions = actions_for(admin, &banned, ME);
        assert!(actions.contains(&Action::SetAffiliation(Affiliation::None)));
        assert!(!actions.contains(&Action::Ban));
        assert_eq!(
            action_label(Action::SetAffiliation(Affiliation::None), &banned),
            "Unban"
        );
    }

    #[test]
    fn kicks_and_bans_ask_first() {
        assert!(confirmation(Action::Ban, "Sam").is_some());
        assert!(confirmation(Action::Kick, "Sam").is_some());
        assert!(confirmation(Action::SetAffiliation(Affiliation::Owner), "Sam").is_some());
        assert!(confirmation(Action::SetAffiliation(Affiliation::Member), "Sam").is_none());
    }

    #[test]
    fn occupants_join_their_affiliation_by_jid_or_nick() {
        let lists = vec![
            Affiliated {
                jid: "o@example.com".into(),
                nick: None,
                affiliation: Affiliation::Owner,
                reason: None,
            },
            Affiliated {
                jid: "m@example.com".into(),
                nick: Some("mo".into()),
                affiliation: Affiliation::Member,
                reason: None,
            },
            Affiliated {
                jid: "x@example.com".into(),
                nick: None,
                affiliation: Affiliation::Outcast,
                reason: Some("spam".into()),
            },
        ];
        let occupants = vec![
            Occupant {
                nick: "Olive".into(),
                jid: Some("O@example.com".into()),
                affiliation: Some(Affiliation::Owner),
                role: Some(Role::Moderator),
                hats: vec!["Founder".into()],
            },
            Occupant {
                nick: "visitor".into(),
                jid: None,
                affiliation: None,
                role: Some(Role::Visitor),
                hats: Vec::new(),
            },
        ];
        let people = merge_people(&lists, &occupants, false);
        let names: Vec<&str> = people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Olive", "mo", "visitor"]);
        assert!(people[0].here && people[0].hats == ["Founder"]);
        assert!(!people[1].here);
        assert_eq!(people[2].affiliation, Affiliation::None);
        let all = merge_people(&lists, &occupants, true);
        assert_eq!(
            all.last().map(|p| p.affiliation),
            Some(Affiliation::Outcast)
        );
        assert_eq!(all.last().and_then(|p| p.reason.as_deref()), Some("spam"));
    }

    #[test]
    fn group_dms_need_the_account_and_one_other() {
        assert_eq!(group_members(ME, &[]), Err(GroupProblem::NeedsPeople));
        assert_eq!(
            group_members(ME, &["ME@example.com".into()]),
            Err(GroupProblem::NeedsPeople)
        );
        assert_eq!(
            group_members(
                ME,
                &[
                    "a@example.com".into(),
                    "A@example.com".into(),
                    "b@example.com".into()
                ]
            ),
            Ok(vec![
                ME.to_string(),
                "a@example.com".into(),
                "b@example.com".into()
            ])
        );
    }

    #[test]
    fn group_titles_fall_back_to_the_people() {
        let members = vec![
            ME.to_string(),
            "a@example.com".into(),
            "b@example.com".into(),
        ];
        assert_eq!(group_title(" Trip ", &members, ME), "Trip");
        assert_eq!(group_title("", &members, ME), "a, b");
        assert_eq!(group_title("", &[ME.to_string()], ME), "Group chat");
    }

    #[test]
    fn addable_people_exclude_self_and_members() {
        let found = vec!["me@example.com", "a@example.com", "b@example.com"];
        let taken = vec!["A@example.com".to_string()];
        let rest = addable(&found, |jid| jid, ME, &taken);
        assert_eq!(rest, vec![&"b@example.com"]);
    }

    #[test]
    fn roster_tracks_arrivals_and_departures() {
        let mut roster = Roster::default();
        let occupant = |nick: &str| Occupant {
            nick: nick.into(),
            jid: None,
            affiliation: None,
            role: None,
            hats: Vec::new(),
        };
        roster.set(occupant("b"));
        roster.set(occupant("a"));
        assert_eq!(roster.occupants().len(), 2);
        roster.remove("a");
        assert_eq!(roster.occupants()[0].nick, "b");
    }

    fn conversation(jid: &str, kind: Kind, flavor: Flavor, thread: bool) -> Conversation {
        Conversation {
            jid: jid.into(),
            name: jid.into(),
            kind,
            autojoin: true,
            flavor,
            thread: thread.then(|| crate::threads::Thread {
                id: "t".into(),
                parent: None,
            }),
        }
    }

    #[test]
    fn rooms_go_before_group_dms_and_dms() {
        let list = vec![
            conversation("a", Kind::Room, Flavor::Plain, false),
            conversation("a", Kind::Room, Flavor::Plain, true),
            conversation("g", Kind::Room, Flavor::Group, false),
            conversation("d", Kind::Direct, Flavor::Plain, false),
        ];
        assert_eq!(placement(&list, Flavor::Plain), 2);
        assert_eq!(placement(&list, Flavor::Forum), 2);
        assert_eq!(placement(&list, Flavor::Group), 3);
        assert_eq!(placement(&[], Flavor::Group), 0);
        assert_eq!(placement(&list[3..], Flavor::Plain), 0);
    }

    #[test]
    fn failures_name_the_condition() {
        assert!(failure_text("Ban", Some("forbidden")).contains("does not allow"));
        assert!(failure_text("Ban", None).contains("did not finish"));
    }
}
