//! Room lifecycle, members and group DMs over the session's `WaddleClient`.
//!
//! Like `session_tools.rs` this only adapts: wire records become the plain
//! types `rooms.rs` works with, and the server's stanza condition is kept
//! so a refusal can say why.

use std::sync::Arc;

use waddle_xmpp_client_ffi::{
    WaddleError, WaddleGroupDmHistoryAccess, WaddleMucAffiliation, WaddleMucRole, WaddleNotifyMode,
    WaddlePresence, WaddleRoomConfig, WaddleRoomConfigPatch, WaddleRoomMemberEntry,
    WaddleRoomMembership,
};

use crate::composer::Mentionable;
use crate::rooms::{Affiliated, Affiliation, Changes, Occupant, Role, Settings};
use crate::runtime;
use crate::session::Session;

/// Why a room request failed: the server's stanza condition, when it
/// answered one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomError {
    pub condition: Option<String>,
}

impl RoomError {
    fn lost() -> Self {
        Self { condition: None }
    }
}

impl From<WaddleError> for RoomError {
    fn from(error: WaddleError) -> Self {
        match error {
            WaddleError::Stanza { condition, .. } => Self {
                condition: Some(condition),
            },
            _ => Self::lost(),
        }
    }
}

type Outcome<T> = Result<T, RoomError>;

/// The room's members, by tier, and what could not be read. A tier a
/// requester may not read (`forbidden`) is left out, not an error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Members {
    pub listed: Vec<Affiliated>,
    /// The banned list was not readable.
    pub bans_hidden: bool,
}

/// What a room's settings dialog shows: the owner's configuration form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomDetails {
    pub settings: Settings,
}

/// An extension bot that has posted in the room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    pub jid: String,
    pub name: Option<String>,
}

pub fn affiliation_from(value: WaddleMucAffiliation) -> Affiliation {
    match value {
        WaddleMucAffiliation::Owner => Affiliation::Owner,
        WaddleMucAffiliation::Admin => Affiliation::Admin,
        WaddleMucAffiliation::Member => Affiliation::Member,
        WaddleMucAffiliation::Outcast => Affiliation::Outcast,
        WaddleMucAffiliation::None => Affiliation::None,
    }
}

fn affiliation_ffi(value: Affiliation) -> WaddleMucAffiliation {
    match value {
        Affiliation::Owner => WaddleMucAffiliation::Owner,
        Affiliation::Admin => WaddleMucAffiliation::Admin,
        Affiliation::Member => WaddleMucAffiliation::Member,
        Affiliation::Outcast => WaddleMucAffiliation::Outcast,
        Affiliation::None => WaddleMucAffiliation::None,
    }
}

fn role_from(value: WaddleMucRole) -> Option<Role> {
    match value {
        WaddleMucRole::Moderator => Some(Role::Moderator),
        WaddleMucRole::Participant => Some(Role::Participant),
        WaddleMucRole::Visitor => Some(Role::Visitor),
        WaddleMucRole::None => None,
    }
}

fn member_from(entry: WaddleRoomMemberEntry) -> Affiliated {
    Affiliated {
        affiliation: affiliation_from(entry.affiliation),
        jid: entry.jid,
        nick: entry.nick,
        reason: entry.reason,
    }
}

fn settings_from(config: WaddleRoomConfig) -> Settings {
    Settings {
        name: config.name.unwrap_or_default(),
        description: config.description.unwrap_or_default(),
        members_only: config.members_only.unwrap_or(false),
        public: config.public_room.unwrap_or(true),
        forum: config.forum.unwrap_or(false),
    }
}

fn patch_from(changes: &Changes) -> WaddleRoomConfigPatch {
    WaddleRoomConfigPatch {
        name: changes.name.clone(),
        description: changes.description.clone(),
        members_only: changes.members_only,
        public_room: changes.public,
        forum: changes.forum,
        ..Default::default()
    }
}

/// An occupant's presence as the room roster keeps it: the room, the nick,
/// and the occupant, or `None` when they left. Presence without a nick
/// (from the room itself) is no occupant.
pub fn occupant_from(presence: &WaddlePresence) -> Option<(String, String, Option<Occupant>)> {
    let from = presence.from.as_deref()?;
    let (room, nick) = from.split_once('/')?;
    if nick.is_empty() {
        return None;
    }
    let present = matches!(presence.presence_type.as_str(), "available" | "");
    if !present {
        return Some((room.to_string(), nick.to_string(), None));
    }
    let real = presence
        .muc_jid
        .as_deref()
        .and_then(|jid| jid.parse::<jid::Jid>().ok())
        .map(|jid| jid.to_bare().to_string());
    Some((
        room.to_string(),
        nick.to_string(),
        Some(Occupant {
            nick: nick.to_string(),
            jid: real,
            affiliation: presence.muc_affiliation.map(affiliation_from),
            role: presence.muc_role.and_then(role_from),
            hats: presence.hats.iter().map(|hat| hat.title.clone()).collect(),
        }),
    ))
}

impl Session {
    /// XEP-0045 §10.1: create `address` in the MUC service, configured
    /// with `settings`. Returns the new room's bare JID.
    pub async fn create_room(&self, address: String, settings: Settings) -> Outcome<String> {
        let client = Arc::clone(&self.client);
        let nick = self.me.nick.clone();
        let patch = WaddleRoomConfigPatch {
            name: Some(settings.name.trim().to_string()),
            description: Some(settings.description.trim().to_string()),
            members_only: Some(settings.members_only),
            public_room: Some(settings.public),
            persistent_room: Some(true),
            forum: settings.forum.then_some(true),
            ..Default::default()
        };
        run(async move { client.create_room(address, nick, patch).await }).await
    }

    /// XEP-0045 §10.2: the room's configuration. Owners only.
    pub async fn room_details(&self, room: &str) -> Outcome<RoomDetails> {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        let config = run(async move { client.fetch_room_config(room).await }).await?;
        Ok(RoomDetails {
            settings: settings_from(config),
        })
    }

    /// Submit what an owner changed.
    pub async fn edit_room(&self, room: &str, changes: &Changes) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        let patch = patch_from(changes);
        run(async move { client.submit_room_config(room, patch).await }).await
    }

    /// XEP-0045 §10.9: delete the room. Owners only.
    pub async fn destroy_room(&self, room: &str) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        run(async move { client.destroy_room(room, None).await }).await
    }

    /// Leave a room on every device: unavailable presence, then the
    /// XEP-0402 bookmark with autojoin off.
    pub async fn leave_room(&self, room: &str, name: &str) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let (room, name, nick) = (room.to_string(), name.to_string(), self.me.nick.clone());
        run(async move {
            client.leave_room(room.clone(), nick).await;
            client
                .set_room_membership(room, WaddleRoomMembership::Left, Some(name))
                .await
                .map(|_| ())
        })
        .await
    }

    /// Join a room on every device: the XEP-0402 bookmark with autojoin on.
    pub async fn bookmark_room(&self, room: &str, name: &str) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let (room, name) = (room.to_string(), name.to_string());
        run(async move {
            client
                .set_room_membership(
                    room,
                    WaddleRoomMembership::Joined {
                        notify_mode: WaddleNotifyMode::OnMention,
                    },
                    Some(name),
                )
                .await
                .map(|_| ())
        })
        .await
    }

    /// XEP-0045 §9.5: the room's affiliation lists, one request a tier.
    /// Fails only when no tier could be read.
    pub async fn members(&self, room: &str) -> Outcome<Members> {
        let mut members = Members::default();
        let mut failure = None;
        let mut read = 0;
        for tier in [
            Affiliation::Owner,
            Affiliation::Admin,
            Affiliation::Member,
            Affiliation::Outcast,
        ] {
            let client = Arc::clone(&self.client);
            let room = room.to_string();
            let tier_ffi = affiliation_ffi(tier);
            match run(async move { client.list_room_members(room, tier_ffi).await }).await {
                Ok(entries) => {
                    read += 1;
                    members.listed.extend(
                        entries
                            .into_iter()
                            .map(member_from)
                            .filter(|entry| entry.affiliation == tier),
                    );
                }
                Err(error) => {
                    if tier == Affiliation::Outcast {
                        members.bans_hidden = true;
                    }
                    failure = Some(error);
                }
            }
        }
        match failure {
            Some(error) if read == 0 => Err(error),
            _ => Ok(members),
        }
    }

    /// The extension bots that have posted in the room.
    pub async fn bots(&self, room: &str) -> Vec<Bot> {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        run(async move { client.list_room_bots(room).await })
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|bot| Bot {
                jid: bot.jid,
                name: bot.name,
            })
            .collect()
    }

    /// XEP-0045 §5.2 and §9.1: set a person's affiliation (a ban is
    /// `Outcast`).
    pub async fn set_affiliation(
        &self,
        room: &str,
        jid: &str,
        affiliation: Affiliation,
    ) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let (room, jid) = (room.to_string(), jid.to_string());
        let ffi = affiliation_ffi(affiliation);
        run(async move { client.set_room_affiliation(room, jid, ffi, None).await }).await
    }

    /// XEP-0045 §8.2: eject an occupant by nick.
    pub async fn kick(&self, room: &str, nick: &str) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let (room, nick) = (room.to_string(), nick.to_string());
        run(async move { client.kick_occupant(room, nick, None).await }).await
    }

    /// Create a group DM with these members (the account included).
    /// Returns the room's bare JID.
    pub async fn create_group_dm(&self, name: String, members: Vec<String>) -> Outcome<String> {
        let client = Arc::clone(&self.client);
        run(async move { client.create_group_dm(name, members).await }).await
    }

    /// Set a group DM's name; an empty name removes it.
    pub async fn rename_group_dm(&self, room: &str, name: &str) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        let name = Some(name.trim().to_string()).filter(|name| !name.is_empty());
        run(async move { client.rename_group_dm(room, name).await }).await
    }

    /// Leave a group DM for good.
    pub async fn leave_group_dm(&self, room: &str) -> Outcome<()> {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        run(async move { client.leave_group_dm(room).await }).await
    }

    /// Add people to a group DM, either with all earlier messages or only
    /// new ones. The invite is a message the server answers later if it
    /// refuses, so this only reports that it was written.
    pub async fn invite_to_group_dm(&self, room: &str, people: Vec<String>, full: bool) -> usize {
        let history = if full {
            WaddleGroupDmHistoryAccess::Full
        } else {
            WaddleGroupDmHistoryAccess::FromJoin
        };
        let mut sent = 0;
        for jid in people {
            let client = Arc::clone(&self.client);
            let room = room.to_string();
            if run(async move { client.invite_to_group_dm(room, jid, history).await })
                .await
                .is_ok()
            {
                sent += 1;
            }
        }
        sent
    }

    /// People in the user directory whose username matches `query`.
    pub async fn find_people(&self, query: String) -> Outcome<Vec<Mentionable>> {
        self.search_users(query).await.ok_or_else(RoomError::lost)
    }
}

async fn run<T, F>(future: F) -> Outcome<T>
where
    F: std::future::Future<Output = Result<T, WaddleError>> + Send + 'static,
    T: Send + 'static,
{
    match runtime::spawn(future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::debug!(?error, "room request failed");
            Err(error.into())
        }
        Err(_) => Err(RoomError::lost()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stanza_errors_keep_their_condition() {
        let error: RoomError = WaddleError::Stanza {
            condition: "forbidden".into(),
            text: None,
        }
        .into();
        assert_eq!(error.condition.as_deref(), Some("forbidden"));
        assert_eq!(RoomError::from(WaddleError::Timeout).condition, None);
    }

    #[test]
    fn config_defaults_to_a_listed_open_text_room() {
        let settings = settings_from(WaddleRoomConfig {
            name: Some("Lounge".into()),
            description: None,
            members_only: None,
            public_room: None,
            moderated: None,
            forum: Some(true),
            pin_permission: None,
        });
        assert_eq!(settings.name, "Lounge");
        assert!(settings.public && !settings.members_only && settings.forum);
    }

    #[test]
    fn a_patch_carries_only_the_changes() {
        let patch = patch_from(&Changes {
            description: Some(String::new()),
            members_only: Some(true),
            ..Default::default()
        });
        assert_eq!(patch.name, None);
        assert_eq!(patch.description, Some(String::new()));
        assert_eq!(patch.members_only, Some(true));
        assert_eq!(patch.public_room, None);
        assert_eq!(patch.forum, None);
        assert_eq!(patch.persistent_room, None);
    }

    #[test]
    fn presence_becomes_an_occupant_or_a_departure() {
        let mut presence = WaddlePresence {
            from: Some("room@muc.example.com/Sam".into()),
            to: None,
            presence_type: "available".into(),
            show: None,
            status: None,
            hats: vec![waddle_xmpp_client_ffi::WaddlePresenceHat {
                uri: "urn:x".into(),
                title: "Founder".into(),
            }],
            muc_affiliation: Some(WaddleMucAffiliation::Admin),
            muc_role: Some(WaddleMucRole::Moderator),
            muc_jid: Some("sam@example.com/phone".into()),
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
        };
        let (room, nick, occupant) = occupant_from(&presence).expect("an occupant");
        assert_eq!(nick, "Sam");
        assert_eq!(room, "room@muc.example.com");
        let occupant = occupant.expect("present");
        assert_eq!(occupant.jid.as_deref(), Some("sam@example.com"));
        assert_eq!(occupant.affiliation, Some(Affiliation::Admin));
        assert_eq!(occupant.role, Some(Role::Moderator));
        assert_eq!(occupant.hats, ["Founder"]);
        presence.presence_type = "unavailable".into();
        assert_eq!(occupant_from(&presence).map(|(_, _, o)| o), Some(None));
        presence.from = Some("room@muc.example.com".into());
        assert!(occupant_from(&presence).is_none());
    }
}
