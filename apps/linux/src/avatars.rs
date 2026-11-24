//! Avatar bookkeeping (XEP-0084), without widgets or a connection.
//!
//! An avatar is fetched once per person and kept for the session, keyed by
//! the item id the metadata advertises. A `+notify` event revalidates it:
//! the core skips the data request when the advertised id is the one held
//! (XEP-0084 §4.2), so a change of id fetches and an unchanged one costs a
//! metadata read only. A person without a picture is remembered too, so a
//! row does not ask again every time it is shown.

use std::collections::HashMap;

/// Pictures are published at most this many pixels on a side.
pub const MAX_SIDE: i32 = 512;
/// Larger files are refused before they are decoded.
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// One person's avatar as far as this session knows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State<T> {
    /// A request is in flight; `previous` stays shown meanwhile.
    Loading {
        previous: Option<(String, T)>,
    },
    Ready {
        id: String,
        picture: T,
    },
    /// Confirmed to have none.
    NoPicture,
}

/// What a lookup learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    /// The id advertised, and the decoded picture when the core fetched it
    /// (it does not when the id was already held).
    Found { id: String, picture: Option<T> },
    /// Definitively no readable avatar.
    NoPicture,
    /// The lookup failed: keep what is shown.
    Failed,
}

/// A request the caller should make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub jid: String,
    /// Item ids whose bytes are held, so the core can skip the download.
    pub known_ids: Vec<String>,
}

#[derive(Debug)]
pub struct Cache<T> {
    entries: HashMap<String, State<T>>,
}

impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

fn key(jid: &str) -> String {
    jid.to_lowercase()
}

impl<T: Clone> Cache<T> {
    /// The picture to show, if there is one.
    pub fn get(&self, jid: &str) -> Option<&T> {
        match self.entries.get(&key(jid))? {
            State::Ready { picture, .. } => Some(picture),
            State::Loading { previous } => previous.as_ref().map(|(_, picture)| picture),
            State::NoPicture => None,
        }
    }

    /// Ask for `jid`'s avatar unless it is known or on its way.
    pub fn want(&mut self, jid: &str) -> Option<Request> {
        if self.entries.contains_key(&key(jid)) {
            return None;
        }
        Some(self.begin(jid))
    }

    /// An advertised change (`+notify`): `avatar_id` is the new item id,
    /// or none when the person removed their avatar. Returns whether what
    /// is shown changed at once, and any request to make.
    pub fn changed(&mut self, jid: &str, avatar_id: Option<&str>) -> (bool, Option<Request>) {
        let held_id = match self.entries.get(&key(jid)) {
            Some(State::Ready { id, .. }) => Some(id.clone()),
            _ => None,
        };
        let known = self.entries.contains_key(&key(jid));
        match avatar_id {
            None if matches!(self.entries.get(&key(jid)), Some(State::NoPicture)) => (false, None),
            None => {
                self.entries.insert(key(jid), State::NoPicture);
                (true, None)
            }
            Some(id) if held_id.as_deref() == Some(id) => (false, None),
            // Someone not shown anywhere yet is fetched when first shown.
            Some(_) if !known => (false, None),
            Some(_) => (false, Some(self.begin(jid))),
        }
    }

    fn begin(&mut self, jid: &str) -> Request {
        let previous = match self.entries.remove(&key(jid)) {
            Some(State::Ready { id, picture }) => Some((id, picture)),
            Some(State::Loading { previous }) => previous,
            _ => None,
        };
        let known_ids = previous
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        self.entries.insert(key(jid), State::Loading { previous });
        Request {
            jid: jid.to_string(),
            known_ids,
        }
    }

    /// Record what a lookup learned; true when what is shown changed.
    pub fn resolved(&mut self, jid: &str, outcome: Outcome<T>) -> bool {
        let previous = match self.entries.remove(&key(jid)) {
            Some(State::Loading { previous }) => previous,
            Some(other) => {
                // Not asked for (or answered twice): leave it as it was.
                self.entries.insert(key(jid), other);
                return false;
            }
            None => None,
        };
        let (state, changed) = match outcome {
            Outcome::Found {
                id,
                picture: Some(picture),
            } => (State::Ready { id, picture }, true),
            // The id is one already held: unchanged.
            Outcome::Found { id, picture: None } => match previous {
                Some((held, picture)) if held == id => (State::Ready { id, picture }, false),
                _ => (State::NoPicture, true),
            },
            Outcome::NoPicture => {
                let changed = previous.is_some();
                (State::NoPicture, changed)
            }
            Outcome::Failed => match previous {
                Some((id, picture)) => (State::Ready { id, picture }, false),
                // Nothing known: ask again next time it is shown.
                None => return false,
            },
        };
        self.entries.insert(key(jid), state);
        changed
    }

    /// After a reconnect events may have been missed: ask again for
    /// everyone held, with the held ids so unchanged pictures are not
    /// downloaded again.
    pub fn revalidate_all(&mut self) -> Vec<Request> {
        let jids: Vec<String> = self.entries.keys().cloned().collect();
        jids.iter().map(|jid| self.begin(jid)).collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Where a picture of `width`×`height` lands once scaled to fit
/// [`MAX_SIDE`] on both sides, never enlarged and never empty.
pub fn publish_size(width: i32, height: i32) -> (i32, i32) {
    if width <= 0 || height <= 0 {
        return (1, 1);
    }
    let scale = f64::min(
        1.0,
        f64::min(
            f64::from(MAX_SIDE) / f64::from(width),
            f64::from(MAX_SIDE) / f64::from(height),
        ),
    );
    (
        ((f64::from(width) * scale).round() as i32).max(1),
        ((f64::from(height) * scale).round() as i32).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestCache = Cache<&'static str>;

    fn found(id: &str, picture: &'static str) -> Outcome<&'static str> {
        Outcome::Found {
            id: id.to_string(),
            picture: Some(picture),
        }
    }

    #[test]
    fn a_person_is_asked_for_once_however_often_they_are_shown() {
        let mut cache = TestCache::default();
        let request = cache.want("Ann@x.test").expect("first");
        assert_eq!(request.known_ids, Vec::<String>::new());
        assert!(cache.want("ann@x.test").is_none());
        assert!(cache.resolved("ann@x.test", found("a1", "pic")));
        assert_eq!(cache.get("ANN@x.test"), Some(&"pic"));
        assert!(cache.want("ann@x.test").is_none());
    }

    #[test]
    fn someone_without_a_picture_is_remembered_so_rows_do_not_ask_again() {
        let mut cache = TestCache::default();
        cache.want("bo@x.test");
        assert!(!cache.resolved("bo@x.test", Outcome::NoPicture));
        assert_eq!(cache.get("bo@x.test"), None);
        assert!(cache.want("bo@x.test").is_none());
    }

    #[test]
    fn a_failed_first_lookup_is_retried_on_the_next_showing() {
        let mut cache = TestCache::default();
        cache.want("bo@x.test");
        assert!(!cache.resolved("bo@x.test", Outcome::Failed));
        assert!(cache.want("bo@x.test").is_some());
    }

    #[test]
    fn a_changed_id_refetches_with_the_held_id_and_keeps_the_old_picture_meanwhile() {
        let mut cache = TestCache::default();
        cache.want("ann@x.test");
        cache.resolved("ann@x.test", found("a1", "old"));
        let (shown_changed, request) = cache.changed("ann@x.test", Some("a2"));
        assert!(!shown_changed);
        assert_eq!(request.expect("refetch").known_ids, vec!["a1".to_string()]);
        assert_eq!(cache.get("ann@x.test"), Some(&"old"));
        assert!(cache.resolved("ann@x.test", found("a2", "new")));
        assert_eq!(cache.get("ann@x.test"), Some(&"new"));
    }

    #[test]
    fn an_unchanged_id_costs_nothing() {
        let mut cache = TestCache::default();
        cache.want("ann@x.test");
        cache.resolved("ann@x.test", found("a1", "pic"));
        assert_eq!(cache.changed("ann@x.test", Some("a1")), (false, None));
    }

    #[test]
    fn an_advertised_id_the_core_already_held_keeps_the_picture() {
        let mut cache = TestCache::default();
        cache.want("ann@x.test");
        cache.resolved("ann@x.test", found("a1", "pic"));
        cache.changed("ann@x.test", Some("a1-again"));
        let outcome = Outcome::Found {
            id: "a1".to_string(),
            picture: None,
        };
        assert!(!cache.resolved("ann@x.test", outcome));
        assert_eq!(cache.get("ann@x.test"), Some(&"pic"));
    }

    #[test]
    fn removing_the_avatar_clears_the_picture_at_once() {
        let mut cache = TestCache::default();
        cache.want("ann@x.test");
        cache.resolved("ann@x.test", found("a1", "pic"));
        assert_eq!(cache.changed("ann@x.test", None), (true, None));
        assert_eq!(cache.get("ann@x.test"), None);
        // Already none: nothing to redraw.
        assert_eq!(cache.changed("ann@x.test", None), (false, None));
    }

    #[test]
    fn a_failed_revalidation_keeps_what_is_shown() {
        let mut cache = TestCache::default();
        cache.want("ann@x.test");
        cache.resolved("ann@x.test", found("a1", "pic"));
        cache.changed("ann@x.test", Some("a2"));
        assert!(!cache.resolved("ann@x.test", Outcome::Failed));
        assert_eq!(cache.get("ann@x.test"), Some(&"pic"));
    }

    #[test]
    fn an_event_for_someone_not_shown_yet_waits_until_they_are() {
        let mut cache = TestCache::default();
        assert_eq!(cache.changed("ann@x.test", Some("a1")), (false, None));
        assert!(cache.want("ann@x.test").is_some());
    }

    #[test]
    fn a_reconnect_asks_again_with_the_held_ids() {
        let mut cache = TestCache::default();
        cache.want("ann@x.test");
        cache.resolved("ann@x.test", found("a1", "pic"));
        cache.want("bo@x.test");
        cache.resolved("bo@x.test", Outcome::NoPicture);
        let mut requests = cache.revalidate_all();
        requests.sort_by(|a, b| a.jid.cmp(&b.jid));
        assert_eq!(requests[0].known_ids, vec!["a1".to_string()]);
        assert!(requests[1].known_ids.is_empty());
        // The picture stays shown while it is checked.
        assert_eq!(cache.get("ann@x.test"), Some(&"pic"));
    }

    #[test]
    fn pictures_publish_at_most_512_on_a_side_and_are_never_enlarged() {
        assert_eq!(publish_size(2048, 1024), (512, 256));
        assert_eq!(publish_size(600, 1200), (256, 512));
        assert_eq!(publish_size(512, 512), (512, 512));
        assert_eq!(publish_size(64, 32), (64, 32));
        assert_eq!(publish_size(10_000, 1), (512, 1));
        assert_eq!(publish_size(0, 5), (1, 1));
    }
}
