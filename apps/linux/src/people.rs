//! The People view's data, without widgets: everyone the account knows by
//! address. Its contacts (shown by the name the account saved for them),
//! anyone whose presence it has seen, people it has direct messages with,
//! participants of the rooms it is in whose real address is exposed, and
//! the community directory when a name is searched.

use std::collections::HashMap;

use jid::{BareJid, Jid};
use waddle_xmpp_client_ffi::WaddlePresence;

use crate::presence::Show;

/// What the account has seen of each person's presence, by bare JID.
/// Several resources can be online at once; the person shows as the most
/// available of them.
#[derive(Debug, Default)]
pub struct PresenceBook {
    resources: HashMap<String, HashMap<String, (Show, Option<String>)>>,
}

/// A person's presence as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub show: Show,
    pub status: Option<String>,
}

fn rank(show: Show) -> u8 {
    match show {
        Show::Chat => 5,
        Show::Available => 4,
        Show::DoNotDisturb => 3,
        Show::Away => 2,
        Show::ExtendedAway => 1,
    }
}

impl PresenceBook {
    /// Record a one-to-one presence; false when it said nothing about a
    /// person (a room's, an error, or a subscription request).
    pub fn apply(&mut self, presence: &WaddlePresence) -> bool {
        if presence.muc_role.is_some()
            || presence.muc_affiliation.is_some()
            || !presence.muc_status_codes.is_empty()
        {
            return false;
        }
        let Some(from) = presence
            .from
            .as_deref()
            .and_then(|jid| jid.parse::<Jid>().ok())
        else {
            return false;
        };
        let resource = from
            .resource()
            .map(|resource| resource.as_str().to_string())
            .unwrap_or_default();
        let bare = from.to_bare().to_string().to_lowercase();
        match presence.presence_type.as_str() {
            "available" | "" => {
                let status = presence
                    .status
                    .as_deref()
                    .map(str::trim)
                    .filter(|status| !status.is_empty())
                    .map(str::to_string);
                self.resources.entry(bare).or_default().insert(
                    resource,
                    (Show::from_wire(presence.show.as_deref()), status),
                );
                true
            }
            "unavailable" => {
                if let Some(resources) = self.resources.get_mut(&bare) {
                    resources.remove(&resource);
                    if resources.is_empty() {
                        self.resources.remove(&bare);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The person's presence, or none when offline (or never seen).
    pub fn seen(&self, jid: &str) -> Option<Seen> {
        self.resources
            .get(&jid.to_lowercase())?
            .values()
            .max_by_key(|(show, _)| rank(*show))
            .map(|(show, status)| Seen {
                show: *show,
                status: status.clone(),
            })
    }

    /// Everyone with presence.
    pub fn jids(&self) -> impl Iterator<Item = &String> {
        self.resources.keys()
    }

    pub fn clear(&mut self) {
        self.resources.clear();
    }
}

/// Where a person was found; a name is taken from the most specific.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// A contact, with the name the account saved for it.
    pub contacts: Vec<(String, Option<String>)>,
    /// People with a direct-message conversation: bare JID and its name.
    pub direct: Vec<(String, String)>,
    /// Room occupants whose real address is known: bare JID and nick.
    pub occupants: Vec<(String, String)>,
    /// Directory results for the search typed: bare JID, username and
    /// display name.
    pub directory: Vec<(String, String, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub jid: String,
    pub name: String,
    pub seen: Option<Seen>,
}

/// The sections of the view.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// Online, free to chat or do not disturb.
    pub here: Vec<Person>,
    /// Away, extended away and offline.
    pub away: Vec<Person>,
    /// Found in the directory only.
    pub directory: Vec<Person>,
}

impl Listing {
    pub fn is_empty(&self) -> bool {
        self.here.is_empty() && self.away.is_empty() && self.directory.is_empty()
    }
}

fn local_part(jid: &str) -> String {
    jid.parse::<BareJid>()
        .ok()
        .and_then(|bare| bare.node().map(|node| node.as_str().to_string()))
        .unwrap_or_else(|| jid.to_string())
}

/// Whether `person` matches a typed `query`: every word is part of the
/// name or the address, whatever the case.
pub fn matches(person: &Person, query: &str) -> bool {
    let name = person.name.to_lowercase();
    let jid = person.jid.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| name.contains(word) || jid.contains(word))
}

/// Build the view for `query`, leaving out `own`.
pub fn list(sources: &Sources, book: &PresenceBook, own: &str, query: &str) -> Listing {
    let own = own.to_lowercase();
    // By lowercase bare JID: the name from the first source that has one,
    // in the order a person is best known by.
    let mut names: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut add = |jid: &str, name: Option<&str>, order: &mut Vec<String>| {
        let key = jid.to_lowercase();
        if key == own {
            return;
        }
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        match names.get_mut(&key) {
            Some(_) => {}
            None => {
                names.insert(
                    key.clone(),
                    name.map_or_else(|| local_part(&key), str::to_string),
                );
                order.push(key);
                return;
            }
        }
        // Known already, but by its address only: a later source may
        // have a better name.
        if let Some(name) = name {
            if names
                .get(&key)
                .is_some_and(|known| *known == local_part(&key))
            {
                names.insert(key, name.to_string());
            }
        }
    };
    for (jid, name) in &sources.contacts {
        add(jid, name.as_deref(), &mut order);
    }
    for (jid, name) in &sources.direct {
        add(jid, Some(name), &mut order);
    }
    for jid in book.jids() {
        add(jid, None, &mut order);
    }
    for (jid, nick) in &sources.occupants {
        add(jid, Some(nick), &mut order);
    }
    let known = order.len();
    let mut found = Vec::new();
    for (jid, username, display) in &sources.directory {
        let before = order.len();
        add(jid, display.as_deref().or(Some(username)), &mut order);
        if order.len() > before {
            found.push(jid.to_lowercase());
        }
    }
    debug_assert!(known <= order.len());

    let mut listing = Listing::default();
    for key in order {
        let person = Person {
            name: names.get(&key).cloned().unwrap_or_default(),
            seen: book.seen(&key),
            jid: key.clone(),
        };
        if !matches(&person, query) {
            continue;
        }
        if found.contains(&key) {
            listing.directory.push(person);
        } else if person.seen.as_ref().is_some_and(|seen| seen.show.is_here()) {
            listing.here.push(person);
        } else {
            listing.away.push(person);
        }
    }
    for section in [&mut listing.here, &mut listing.away, &mut listing.directory] {
        section.sort_by_key(|person| (person.name.to_lowercase(), person.jid.clone()));
    }
    listing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(from: &str, kind: &str, show: Option<&str>) -> WaddlePresence {
        WaddlePresence {
            from: Some(from.to_string()),
            to: None,
            presence_type: kind.to_string(),
            show: show.map(str::to_string),
            status: None,
            hats: Vec::new(),
            muc_affiliation: None,
            muc_role: None,
            muc_jid: None,
            muc_status_codes: Vec::new(),
            muc_destroyed: false,
            vcard_avatar: None,
            idle_since: None,
            error_condition: None,
            error_type: None,
            error_text: None,
            hand_raised: false,
            muted: false,
            muji: None,
        }
    }

    #[test]
    fn a_person_shows_as_their_most_available_resource() {
        let mut book = PresenceBook::default();
        assert!(book.apply(&presence("ann@x.test/phone", "available", Some("away"))));
        assert!(book.apply(&presence("ann@x.test/laptop", "available", None)));
        assert_eq!(
            book.seen("ANN@x.test").map(|seen| seen.show),
            Some(Show::Available)
        );
        assert!(book.apply(&presence("ann@x.test/laptop", "unavailable", None)));
        assert_eq!(
            book.seen("ann@x.test").map(|seen| seen.show),
            Some(Show::Away)
        );
        assert!(book.apply(&presence("ann@x.test/phone", "unavailable", None)));
        assert_eq!(book.seen("ann@x.test"), None);
        assert_eq!(book.jids().count(), 0);
    }

    #[test]
    fn room_presence_and_errors_say_nothing_about_a_person() {
        let mut book = PresenceBook::default();
        let mut room = presence("room@muc.x.test/ann", "available", None);
        room.muc_role = Some(waddle_xmpp_client_ffi::WaddleMucRole::Participant);
        assert!(!book.apply(&room));
        assert!(!book.apply(&presence("ann@x.test", "error", None)));
        assert!(!book.apply(&presence("ann@x.test", "subscribe", None)));
        assert_eq!(book.jids().count(), 0);
    }

    fn sources() -> Sources {
        Sources {
            contacts: vec![
                ("ann@x.test".to_string(), Some("Ann Example".to_string())),
                ("bo@x.test".to_string(), None),
            ],
            direct: vec![
                ("bo@x.test".to_string(), "bo".to_string()),
                ("cy@x.test".to_string(), "cy".to_string()),
            ],
            occupants: vec![("di@x.test".to_string(), "Di".to_string())],
            directory: Vec::new(),
        }
    }

    #[test]
    fn everyone_known_is_listed_once_split_by_whether_they_are_here() {
        let mut book = PresenceBook::default();
        book.apply(&presence("ann@x.test/a", "available", None));
        book.apply(&presence("cy@x.test/a", "available", Some("away")));
        book.apply(&presence("dnd@x.test/a", "available", Some("dnd")));
        let listing = list(&sources(), &book, "me@x.test", "");
        let names = |people: &[Person]| -> Vec<String> {
            people.iter().map(|person| person.name.clone()).collect()
        };
        assert_eq!(names(&listing.here), ["Ann Example", "dnd"]);
        assert_eq!(names(&listing.away), ["bo", "cy", "Di"]);
        assert!(listing.directory.is_empty());
    }

    #[test]
    fn a_contacts_saved_name_beats_the_address_and_the_account_itself_is_left_out() {
        let mut sources = sources();
        sources.contacts.push(("ME@x.test".to_string(), None));
        sources
            .direct
            .push(("ann@x.test".to_string(), "ann".to_string()));
        let listing = list(&sources, &PresenceBook::default(), "me@x.test", "");
        assert!(listing.away.iter().all(|person| person.jid != "me@x.test"));
        let ann = listing
            .away
            .iter()
            .find(|person| person.jid == "ann@x.test")
            .expect("ann");
        assert_eq!(ann.name, "Ann Example");
    }

    #[test]
    fn a_better_name_replaces_one_made_of_the_address() {
        let sources = Sources {
            contacts: vec![("bo@x.test".to_string(), None)],
            occupants: vec![("bo@x.test".to_string(), "Bo the Builder".to_string())],
            ..Sources::default()
        };
        let listing = list(&sources, &PresenceBook::default(), "me@x.test", "");
        assert_eq!(listing.away[0].name, "Bo the Builder");
    }

    #[test]
    fn a_search_matches_names_and_addresses_in_any_case() {
        let listing = list(
            &sources(),
            &PresenceBook::default(),
            "me@x.test",
            "EXAMPLE ann",
        );
        assert_eq!(listing.away.len(), 1);
        assert_eq!(listing.away[0].jid, "ann@x.test");
        let by_address = list(&sources(), &PresenceBook::default(), "me@x.test", "cy@x");
        assert_eq!(by_address.away.len(), 1);
        assert!(list(&sources(), &PresenceBook::default(), "me@x.test", "zz").is_empty());
    }

    #[test]
    fn the_directory_adds_only_people_not_already_known() {
        let mut sources = sources();
        sources.directory = vec![
            ("ann@x.test".to_string(), "ann".to_string(), None),
            (
                "eve@x.test".to_string(),
                "eve".to_string(),
                Some("Eve".to_string()),
            ),
            ("me@x.test".to_string(), "me".to_string(), None),
        ];
        let listing = list(&sources, &PresenceBook::default(), "me@x.test", "e");
        assert_eq!(listing.directory.len(), 1);
        assert_eq!(listing.directory[0].name, "Eve");
        assert!(listing.away.iter().all(|person| person.jid != "eve@x.test"));
    }
}
