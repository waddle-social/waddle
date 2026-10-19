//! Mentioning people who are not in the room.
//!
//! Occupants come from XEP-0045 presence. Everyone else is found in the
//! account domain's XEP-0055 user directory while an `@query` is typed in
//! a room: after a short pause, the latest query is searched, its results
//! are kept for that room, and a send resolves the typed `@username`s of
//! everyone the room's searches found. DMs never search.

use std::collections::HashMap;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::WaddleWindow;
use crate::composer::{self, Mentionable};
use crate::session::{Conversation, Kind};

/// How long typing must pause before the directory is searched.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Directory searches made from room composers in this session.
#[derive(Default)]
pub struct DirectorySearch {
    rooms: HashMap<String, RoomDirectory>,
    /// The most recent lookup; any other response is stale.
    latest: Option<Lookup>,
    serial: u64,
}

#[derive(Default)]
struct RoomDirectory {
    /// Results by query.
    results: HashMap<String, Vec<Mentionable>>,
    /// Everyone this room's searches found, once each by bare JID.
    seen: Vec<Mentionable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Lookup {
    serial: u64,
    room: String,
    query: String,
}

impl DirectorySearch {
    fn cached(&self, room: &str, query: &str) -> Option<&[Mentionable]> {
        Some(self.rooms.get(room)?.results.get(query)?.as_slice())
    }

    /// Start a lookup of `query` in `room` unless that same lookup is
    /// already the latest one; the returned serial identifies it.
    fn begin(&mut self, room: &str, query: &str) -> Option<u64> {
        if self
            .latest
            .as_ref()
            .is_some_and(|latest| latest.room == room && latest.query == query)
        {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.latest = Some(Lookup {
            serial: self.serial,
            room: room.to_string(),
            query: query.to_string(),
        });
        Some(self.serial)
    }

    fn is_latest(&self, serial: u64) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|latest| latest.serial == serial)
    }

    /// Record lookup `serial`'s outcome (`None` when the search failed),
    /// leaving out `own` bare JID. Only the latest lookup counts; true
    /// when it found something to show.
    fn finish(&mut self, serial: u64, found: Option<Vec<Mentionable>>, own: &str) -> bool {
        if !self.is_latest(serial) {
            return false;
        }
        let Some(lookup) = self.latest.take() else {
            return false;
        };
        let Some(found) = found else {
            return false;
        };
        let own = own.to_lowercase();
        let found: Vec<Mentionable> = found
            .into_iter()
            .filter(|person| person.jid.to_lowercase() != own)
            .collect();
        let room = self.rooms.entry(lookup.room).or_default();
        for person in &found {
            let jid = person.jid.to_lowercase();
            if !room.seen.iter().any(|seen| seen.jid.to_lowercase() == jid) {
                room.seen.push(person.clone());
            }
        }
        room.results.insert(lookup.query, found);
        true
    }

    fn seen(&self, room: &str) -> &[Mentionable] {
        self.rooms
            .get(room)
            .map_or(&[], |directory| directory.seen.as_slice())
    }
}

impl WaddleWindow {
    pub(super) fn clear_directory_search(&self) {
        self.imp()
            .directory_search
            .replace(DirectorySearch::default());
    }

    /// Directory users for the `@query` typed in this conversation. Until
    /// the search answers, nobody; it is started after a pause.
    pub(super) fn directory_mentionable(
        &self,
        conversation: &Conversation,
        query: &str,
    ) -> Vec<Mentionable> {
        if conversation.kind != Kind::Room || query.is_empty() {
            return Vec::new();
        }
        let imp = self.imp();
        if let Some(found) = imp
            .directory_search
            .borrow()
            .cached(&conversation.jid, query)
        {
            return found.to_vec();
        }
        let serial = imp
            .directory_search
            .borrow_mut()
            .begin(&conversation.jid, query);
        if let Some(serial) = serial {
            self.search_directory_soon(serial, query.to_string());
        }
        Vec::new()
    }

    fn search_directory_soon(&self, serial: u64, query: String) {
        let window = self.downgrade();
        let generation = self.imp().generation.get();
        glib::timeout_add_local_once(DEBOUNCE, move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let imp = window.imp();
            if imp.generation.get() != generation
                || !imp.directory_search.borrow().is_latest(serial)
            {
                return;
            }
            let Some(session) = imp.session.borrow().clone() else {
                imp.directory_search.borrow_mut().finish(serial, None, "");
                return;
            };
            let window = window.downgrade();
            glib::spawn_future_local(async move {
                let found = session.search_users(query).await;
                let Some(window) = window.upgrade() else {
                    return;
                };
                if window.imp().generation.get() != generation {
                    return;
                }
                let own = session.me.bare.to_string();
                let fresh = window
                    .imp()
                    .directory_search
                    .borrow_mut()
                    .finish(serial, found, &own);
                // A failed search leaves the occupants as they are.
                if fresh {
                    window.refresh_suggestions();
                }
            });
        });
    }

    /// Whom a send in this conversation resolves typed `@label`s to:
    /// occupants, and the directory users this room's searches found.
    pub(super) fn mention_targets(&self, conversation: &Conversation) -> Vec<Mentionable> {
        let occupants = self.mentionable(conversation);
        if conversation.kind != Kind::Room {
            return occupants;
        }
        let directory = self.imp().directory_search.borrow();
        composer::mention_targets(occupants, directory.seen(&conversation.jid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(username: &str) -> Mentionable {
        Mentionable {
            nick: username.into(),
            jid: format!("{username}@example.com"),
            display_name: None,
        }
    }

    #[test]
    fn only_the_latest_lookup_is_kept_and_cached_per_room_and_query() {
        let mut search = DirectorySearch::default();
        let first = search.begin("room@muc", "a").expect("a lookup");
        assert_eq!(search.begin("room@muc", "a"), None, "already waiting");
        let second = search.begin("room@muc", "an").expect("a newer lookup");
        assert!(!search.finish(first, Some(vec![user("al")]), "me@example.com"));
        assert_eq!(search.cached("room@muc", "a"), None, "stale responses drop");
        assert!(search.seen("room@muc").is_empty());
        assert!(search.finish(
            second,
            Some(vec![user("ann"), user("me")]),
            "ME@example.com"
        ));
        assert_eq!(search.cached("room@muc", "an"), Some(&[user("ann")][..]));
        assert_eq!(search.cached("other@muc", "an"), None);
        assert_eq!(search.seen("room@muc"), [user("ann")]);
    }

    #[test]
    fn a_failed_search_caches_nothing_and_can_be_retried() {
        let mut search = DirectorySearch::default();
        let serial = search.begin("room@muc", "an").expect("a lookup");
        assert!(!search.finish(serial, None, "me@example.com"));
        assert_eq!(search.cached("room@muc", "an"), None);
        assert!(search.begin("room@muc", "an").is_some());
    }

    #[test]
    fn everyone_a_room_search_found_is_remembered_once() {
        let mut search = DirectorySearch::default();
        let serial = search.begin("room@muc", "a").expect("a lookup");
        search.finish(serial, Some(vec![user("ann"), user("al")]), "me@x");
        let serial = search.begin("room@muc", "an").expect("a lookup");
        search.finish(serial, Some(vec![user("ann")]), "me@x");
        assert_eq!(search.seen("room@muc"), [user("ann"), user("al")]);
        assert!(search.seen("other@muc").is_empty());
    }
}
