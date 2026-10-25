//! Room entry is confirmed by XEP-0045 self-presence, not the socket write.
//! The same presence stream tells who else is in each room, for mentions.
use std::collections::{HashMap, VecDeque};

use adw::prelude::{EntryExt, ObjectExt, WidgetExt};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use jid::{BareJid, Jid};
use waddle_xmpp_client_ffi::WaddlePresence;

use super::WaddleWindow;
use crate::composer::Mentionable;
use crate::session::{Conversation, Kind};

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoomState {
    Joining(u64),
    Joined,
    Failed,
}

#[derive(Default)]
pub struct Membership {
    rooms: HashMap<String, RoomState>,
    /// Each requested room's occupants: nick to real bare JID, when the
    /// room exposes it (XEP-0045 §7.2.3).
    occupants: HashMap<String, HashMap<String, Option<BareJid>>>,
    attempt: u64,
}

// Retain transport-acked messages too: a later stanza error can reject them.
// Bound the lookup to the most recent sends for this connection.
const RETAINED_SENDS: usize = 512;

#[derive(Default)]
pub struct OutboundSends {
    order: VecDeque<String>,
    messages: HashMap<String, Conversation>,
}

impl OutboundSends {
    pub(super) fn insert(&mut self, id: String, conversation: Conversation) {
        if self.messages.contains_key(&id) {
            return;
        }
        while self.order.len() >= RETAINED_SENDS {
            if let Some(oldest) = self.order.pop_front() {
                self.messages.remove(&oldest);
            }
        }
        self.order.push_back(id.clone());
        self.messages.insert(id, conversation);
    }

    fn get(&self, id: &str) -> Option<&Conversation> {
        self.messages.get(id)
    }

    pub(super) fn remove(&mut self, id: &str) {
        self.messages.remove(id);
        self.order.retain(|retained| retained != id);
    }

    fn clear(&mut self) {
        self.messages.clear();
        self.order.clear();
    }
}

impl Membership {
    fn begin(&mut self, jid: &str) -> Option<u64> {
        if matches!(
            self.rooms.get(jid),
            Some(RoomState::Joining(_) | RoomState::Joined)
        ) {
            return None;
        }
        self.attempt = self.attempt.wrapping_add(1);
        self.rooms
            .insert(jid.to_string(), RoomState::Joining(self.attempt));
        Some(self.attempt)
    }

    fn joined(&self, jid: &str) -> bool {
        self.rooms.get(jid) == Some(&RoomState::Joined)
    }

    fn expire(&mut self, jid: &str, attempt: u64) -> bool {
        if self.rooms.get(jid) != Some(&RoomState::Joining(attempt)) {
            return false;
        }
        self.fail(jid);
        true
    }

    fn fail(&mut self, jid: &str) {
        self.rooms.insert(jid.to_string(), RoomState::Failed);
        self.occupants.remove(jid);
    }

    fn clear(&mut self) {
        self.rooms.clear();
        self.occupants.clear();
    }

    /// Record an occupant's presence in a room this resource asked for.
    fn track(&mut self, room: &str, nick: &str, present: bool, real: Option<BareJid>) {
        if !self.rooms.contains_key(room) {
            return;
        }
        let occupants = self.occupants.entry(room.to_string()).or_default();
        if present {
            occupants.insert(nick.to_string(), real);
        } else {
            occupants.remove(nick);
        }
    }

    /// Whom a message in `room` can mention: present occupants with a
    /// known real JID, other than `own_nick`, by nick.
    fn mentionable(&self, room: &str, own_nick: &str) -> Vec<Mentionable> {
        let mut people: Vec<Mentionable> = self
            .occupants
            .get(room)
            .into_iter()
            .flatten()
            .filter(|(nick, _)| nick.as_str() != own_nick)
            .filter_map(|(nick, real)| {
                Some(Mentionable {
                    nick: nick.clone(),
                    jid: real.as_ref()?.to_string(),
                    display_name: None,
                })
            })
            .collect();
        people.sort_by_key(|person| person.nick.to_lowercase());
        people
    }
}

impl WaddleWindow {
    pub(super) fn clear_membership(&self) {
        self.imp().membership.borrow_mut().clear();
        self.imp().outbound.borrow_mut().clear();
        self.refresh_composer();
    }

    pub(super) fn can_send(&self, conversation: &Conversation) -> bool {
        self.imp().connected.get()
            && (conversation.kind == Kind::Direct
                || self.imp().membership.borrow().joined(&conversation.jid))
    }

    /// Whom a message in this conversation can mention.
    pub(super) fn mentionable(&self, conversation: &Conversation) -> Vec<Mentionable> {
        let imp = self.imp();
        if conversation.kind != Kind::Room {
            return Vec::new();
        }
        let Some(own_nick) = imp.session.borrow().as_ref().map(|s| s.me.nick.clone()) else {
            return Vec::new();
        };
        imp.membership
            .borrow()
            .mentionable(&conversation.jid, &own_nick)
    }

    /// The composer is usable when the open conversation can take a
    /// message and no upload is in flight.
    pub(super) fn refresh_composer(&self) {
        let ready = self
            .current_conversation()
            .is_some_and(|c| self.can_send(&c));
        self.imp()
            .composer
            .set_sensitive(ready && !self.imp().uploading.get());
        if let Some(conversation) = self.current_conversation() {
            let hint = if !self.imp().connected.get() {
                "Reconnecting…".to_string()
            } else if ready {
                format!(
                    "Message {}",
                    if conversation.kind == Kind::Room {
                        format!("#{}", conversation.name)
                    } else {
                        conversation.name
                    }
                )
            } else if matches!(
                self.imp().membership.borrow().rooms.get(&conversation.jid),
                Some(RoomState::Failed)
            ) {
                "Room unavailable · reopen to retry".to_string()
            } else {
                "Joining room…".to_string()
            };
            self.imp().composer_entry.set_placeholder_text(Some(&hint));
        }
    }

    pub(super) fn ensure_room_joined(&self, room: &Conversation) {
        let imp = self.imp();
        if room.kind != Kind::Room || !imp.connected.get() {
            return;
        }
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let Some(attempt) = imp.membership.borrow_mut().begin(&room.jid) else {
            return;
        };
        session.join(room);
        self.refresh_composer();
        let window = self.downgrade();
        let jid = room.jid.clone();
        let generation = imp.generation.get();
        glib::timeout_add_seconds_local_once(20, move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            if window.imp().generation.get() != generation {
                return;
            }
            let expired = window.imp().membership.borrow_mut().expire(&jid, attempt);
            if expired {
                window.toast("The room did not confirm that you joined. Reopen it to try again.");
                window.refresh_composer();
            }
        });
    }

    pub(super) fn on_room_presence(&self, presence: WaddlePresence) {
        let imp = self.imp();
        if !imp.connected.get() {
            return;
        }
        let Some(from) = presence.from.as_deref().and_then(|s| s.parse::<Jid>().ok()) else {
            return;
        };
        let room = from.to_bare().to_string();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        if let Some(nick) = from.resource() {
            let present = matches!(presence.presence_type.as_str(), "available" | "");
            let real = presence
                .muc_jid
                .as_deref()
                .and_then(|real| real.parse::<Jid>().ok())
                .map(|real| real.to_bare());
            imp.membership
                .borrow_mut()
                .track(&room, nick.as_str(), present, real);
        }
        let expected_nick = from
            .resource()
            .is_some_and(|r| r.as_str() == session.me.nick);
        let own = presence.muc_status_codes.contains(&110);
        let joining = matches!(
            imp.membership.borrow().rooms.get(&room),
            Some(RoomState::Joining(_))
        );
        let failed = (presence.presence_type == "error" && expected_nick && joining)
            || (own && presence.presence_type == "unavailable");
        let state = if failed {
            Some(RoomState::Failed)
        } else if own && matches!(presence.presence_type.as_str(), "available" | "") {
            Some(RoomState::Joined)
        } else {
            None
        };
        let Some(state) = state else { return };
        // Only rooms requested by this resource can make a composer ready.
        if !imp.membership.borrow().rooms.contains_key(&room) {
            return;
        }
        if state == RoomState::Failed {
            imp.membership.borrow_mut().fail(&room);
        } else {
            imp.membership.borrow_mut().rooms.insert(room, state);
        }
        if state == RoomState::Failed {
            let reason = presence
                .error_text
                .as_deref()
                .or(presence.error_condition.as_deref());
            self.toast(&reason.map_or_else(
                || "You are no longer joined to this room.".to_string(),
                |reason| format!("Could not join the room: {reason}"),
            ));
        }
        self.refresh_composer();
    }

    pub(super) fn on_message_rejected(&self, id: String, from: String, to: Option<String>) {
        let imp = self.imp();
        let Some(conversation) = imp.outbound.borrow().get(&id).cloned() else {
            return;
        };
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        if !rejection_matches(
            &conversation.jid,
            conversation.kind,
            &session.me.bare,
            &from,
            to.as_deref(),
        ) {
            return;
        }
        imp.outbound.borrow_mut().remove(&id);
        self.edit_conversation(&conversation.key(), |timeline| timeline.reject_send(&id));
        self.toast("The server rejected the message.");
    }
}

fn rejection_matches(
    peer: &str,
    kind: Kind,
    account: &BareJid,
    from: &str,
    to: Option<&str>,
) -> bool {
    let (Ok(peer), Ok(from)) = (peer.parse::<BareJid>(), from.parse::<Jid>()) else {
        return false;
    };
    if !to.is_none_or(|to| to.parse::<Jid>().is_ok_and(|to| &to.to_bare() == account)) {
        return false;
    }
    // RFC 6120 permits errors from the local server or recipient service.
    if from.node().is_none()
        && from.resource().is_none()
        && (from.domain() == account.domain() || from.domain() == peer.domain())
    {
        return true;
    }
    from.to_bare() == peer && (kind == Kind::Direct || from.resource().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_retention_is_bounded_and_removal_releases_the_slot() {
        let mut sends = OutboundSends::default();
        let conversation = Conversation {
            thread: None,
            jid: "room@example.com".into(),
            name: "room".into(),
            kind: Kind::Room,
            autojoin: false,
        };
        for id in 0..=RETAINED_SENDS {
            sends.insert(id.to_string(), conversation.clone());
        }
        assert!(sends.get("0").is_none());
        assert!(sends.get(&RETAINED_SENDS.to_string()).is_some());
        assert_eq!(sends.messages.len(), RETAINED_SENDS);
        sends.remove("1");
        assert_eq!(sends.order.len(), RETAINED_SENDS - 1);
        sends.clear();
        assert!(sends.messages.is_empty());
        assert!(sends.order.is_empty());
    }

    #[test]
    fn joining_is_not_membership_and_old_timeouts_cannot_expire_retries() {
        let mut membership = Membership::default();
        let first = membership.begin("room@example.com").unwrap();
        assert!(!membership.joined("room@example.com"));
        assert!(membership.begin("room@example.com").is_none());
        assert!(membership.expire("room@example.com", first));
        let retry = membership.begin("room@example.com").unwrap();
        assert!(!membership.expire("room@example.com", first));
        membership
            .rooms
            .insert("room@example.com".into(), RoomState::Joined);
        assert!(!membership.expire("room@example.com", retry));
        assert!(membership.joined("room@example.com"));
        membership.rooms.clear();
        assert!(!membership.joined("room@example.com"));
    }

    #[test]
    fn occupants_of_requested_rooms_are_mentionable_until_they_leave() {
        let mut membership = Membership::default();
        let ann: BareJid = "ann@example.com".parse().unwrap();
        membership.track("room@example.com", "ann", true, Some(ann.clone()));
        assert!(
            membership.mentionable("room@example.com", "me").is_empty(),
            "a room this resource never asked for"
        );
        membership.begin("room@example.com");
        membership.track("room@example.com", "ann", true, Some(ann));
        membership.track("room@example.com", "anon", true, None);
        membership.track(
            "room@example.com",
            "me",
            true,
            Some("me@example.com".parse().unwrap()),
        );
        assert_eq!(
            membership.mentionable("room@example.com", "me"),
            [Mentionable {
                nick: "ann".into(),
                jid: "ann@example.com".into(),
                display_name: None,
            }]
        );
        membership.track("room@example.com", "ann", false, None);
        assert!(membership.mentionable("room@example.com", "me").is_empty());
        membership.track(
            "room@example.com",
            "ann",
            true,
            Some("ann@example.com".parse().unwrap()),
        );
        membership.fail("room@example.com");
        assert!(membership.mentionable("room@example.com", "me").is_empty());
    }

    #[test]
    fn rejections_must_match_both_retained_addresses() {
        let account: BareJid = "me@example.com".parse().unwrap();
        assert!(rejection_matches(
            "room@example.com",
            Kind::Room,
            &account,
            "room@example.com",
            Some("me@example.com/linux")
        ));
        assert!(!rejection_matches(
            "room@example.com",
            Kind::Room,
            &account,
            "other@example.com",
            Some("me@example.com")
        ));
        assert!(!rejection_matches(
            "room@example.com",
            Kind::Room,
            &account,
            "room@example.com/other",
            Some("me@example.com")
        ));
        assert!(rejection_matches(
            "room@example.com",
            Kind::Room,
            &account,
            "example.com",
            Some("me@example.com")
        ));
        assert!(!rejection_matches(
            "room@example.com",
            Kind::Room,
            &account,
            "room@example.com",
            Some("other@example.com")
        ));
        assert!(!rejection_matches(
            "room@example.com",
            Kind::Room,
            &account,
            "room@example.com",
            Some("invalid jid")
        ));
    }
}
