//! The session's presence, avatar and profile verbs: thin calls into the
//! shared protocol facade, each answered in the shape the UI needs.

use std::sync::Arc;

use waddle_xmpp_client_ffi::{
    WaddleActivity, WaddleError, WaddleMood, WaddlePepProfile, WaddleRosterContact, WaddleTune,
    WaddleVCard4,
};

use crate::avatars::Outcome;
use crate::presence::{OwnPresence, PresenceMode};
use crate::runtime;
use crate::session::Session;

/// Why a profile call failed, in words for a toast.
pub fn describe(error: &WaddleError) -> String {
    match error {
        WaddleError::NotConnected => "You are not connected.".to_string(),
        WaddleError::Timeout => "The server took too long to answer.".to_string(),
        WaddleError::InvalidArgument | WaddleError::InvalidJid => {
            "The server could not take that value.".to_string()
        }
        WaddleError::Stanza { condition, text } => {
            match text.as_deref().filter(|t| !t.is_empty()) {
                Some(text) => text.to_string(),
                None => format!("The server refused it ({condition})."),
            }
        }
        _ => "The request failed.".to_string(),
    }
}

impl Session {
    /// RFC 6121 presence: initial presence after connecting (without it
    /// the server treats this resource as unavailable and does not deliver
    /// 1:1 messages to it), and again whenever what it shows changes.
    pub fn announce(&self, presence: &OwnPresence) {
        let client = Arc::clone(&self.client);
        let show = presence.show.wire().map(str::to_string);
        let (status, idle_since) = (presence.status.clone(), presence.idle_since());
        runtime::spawn(async move { client.send_presence(status, show, idle_since).await });
    }

    /// The account's stored status mode; none when it could not be read.
    pub async fn stored_mode(&self) -> Option<PresenceMode> {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { client.fetch_status_preference().await }).await {
            Ok(Ok(preference)) => Some(PresenceMode::from_wire(preference)),
            Ok(Err(error)) => {
                tracing::debug!(?error, "could not read the stored status");
                None
            }
            Err(_) => None,
        }
    }

    /// Store `mode` for every device of the account. True when the
    /// server accepted it.
    pub async fn store_mode(&self, mode: PresenceMode) -> bool {
        let client = Arc::clone(&self.client);
        let outcome =
            runtime::spawn(async move { client.publish_status_preference(mode.to_wire()).await })
                .await;
        matches!(outcome, Ok(Ok(())))
    }

    /// `jid`'s avatar, skipping the download when its id is one of
    /// `known_ids`.
    pub async fn avatar(&self, jid: String, known_ids: Vec<String>) -> Outcome<Vec<u8>> {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { client.request_avatar(jid, known_ids).await }).await {
            Ok(Ok(Some(result))) => Outcome::Found {
                id: result.id,
                picture: result.avatar.map(|avatar| avatar.data),
            },
            Ok(Ok(None)) => Outcome::NoPicture,
            Ok(Err(error)) => {
                tracing::debug!(?error, "could not look up an avatar");
                Outcome::Failed
            }
            Err(_) => Outcome::Failed,
        }
    }

    /// Publish `png` as the avatar; the answer is its item id.
    pub async fn publish_avatar(
        &self,
        png: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<String, WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move {
            client
                .publish_avatar(png, "image/png".to_string(), width, height)
                .await
        })
        .await
        .unwrap_or(Err(WaddleError::Transport))
    }

    pub async fn remove_avatar(&self) -> Result<(), WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.disable_avatar().await })
            .await
            .unwrap_or(Err(WaddleError::Transport))
    }

    /// Someone's vCard; none when they publish none.
    pub async fn vcard(&self, jid: String) -> Result<Option<WaddleVCard4>, WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.fetch_vcard4(jid).await })
            .await
            .unwrap_or(Err(WaddleError::Transport))
    }

    /// This account's vCard for editing: a lookup that fails is an error,
    /// never an empty vCard, since saving replaces the whole of it.
    pub async fn own_vcard(&self) -> Result<Option<WaddleVCard4>, WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.fetch_own_vcard4().await })
            .await
            .unwrap_or(Err(WaddleError::Transport))
    }

    pub async fn publish_vcard(&self, vcard: WaddleVCard4) -> Result<(), WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.publish_vcard4(vcard).await })
            .await
            .unwrap_or(Err(WaddleError::Transport))
    }

    /// Someone's mood, activity and tune.
    pub async fn shared_status(&self, jid: String) -> Result<WaddlePepProfile, WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.fetch_user_pep_profile(jid).await })
            .await
            .unwrap_or(Err(WaddleError::Transport))
    }

    /// This account's activity; a lookup that fails is an error, so a
    /// call another device marks is not overwritten by mistake.
    pub async fn own_activity(&self) -> Result<Option<WaddleActivity>, WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.fetch_own_activity().await })
            .await
            .unwrap_or(Err(WaddleError::Transport))
    }

    pub async fn set_mood(&self, mood: Option<WaddleMood>) -> Result<(), WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move {
            match mood {
                Some(mood) => client.publish_mood(mood.kind, mood.text).await,
                None => client.retract_mood().await,
            }
        })
        .await
        .unwrap_or(Err(WaddleError::Transport))
    }

    pub async fn set_activity(&self, activity: Option<WaddleActivity>) -> Result<(), WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move {
            match activity {
                Some(activity) => {
                    client
                        .publish_activity(
                            activity.general,
                            activity.specific,
                            activity.text,
                            ui_language_tag(),
                        )
                        .await
                }
                None => client.retract_activity().await,
            }
        })
        .await
        .unwrap_or(Err(WaddleError::Transport))
    }

    pub async fn set_tune(&self, tune: Option<WaddleTune>) -> Result<(), WaddleError> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move {
            match tune {
                Some(tune) => client.publish_tune(tune).await,
                None => client.retract_tune().await,
            }
        })
        .await
        .unwrap_or(Err(WaddleError::Transport))
    }

    /// RFC 6121 roster; none when it could not be read.
    pub async fn contacts(&self) -> Option<Vec<WaddleRosterContact>> {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { client.fetch_roster_contacts().await }).await {
            Ok(Ok(contacts)) => Some(contacts),
            Ok(Err(error)) => {
                tracing::debug!(?error, "could not read the roster");
                None
            }
            Err(_) => None,
        }
    }
}

/// BCP 47 tag of the UI locale (for example `en-US`) taken from the GLib
/// language list, used as `xml:lang` on free-text XEP-0108 activity text.
fn ui_language_tag() -> Option<String> {
    gtk::glib::language_names()
        .iter()
        .map(|name| {
            name.split(['.', '@'])
                .next()
                .unwrap_or_default()
                .replace('_', "-")
        })
        .find(|tag| !tag.is_empty() && tag != "C" && tag != "POSIX")
}
