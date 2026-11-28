//! UniFFI verbs and event records for XEP-0045 mediated invitations.

use jid::{BareJid, Jid};
use waddle_xmpp_client::muc_invites::MucInvitationEvent;
use xmpp_parsers::muc::user::Reason;

use crate::error::client_error_to_waddle;
use crate::{WaddleClient, WaddleError};

/// An invitation to join a room, relayed by that room.
#[derive(uniffi::Record, Clone)]
pub struct WaddleMucInvitation {
    pub room: BareJid,
    pub inviter: Jid,
    pub reason: Option<String>,
    pub password: Option<String>,
    pub continuation: Option<WaddleMucInvitationContinuation>,
}

/// The invitation asks to continue a private conversation in the room.
#[derive(uniffi::Record, Clone)]
pub struct WaddleMucInvitationContinuation {
    pub thread: Option<String>,
}

/// An invitee's decline, relayed to the inviter by the room.
#[derive(uniffi::Record, Clone)]
pub struct WaddleMucInvitationDecline {
    pub room: BareJid,
    pub invitee: Jid,
    pub reason: Option<String>,
}

/// The room-mediated invitation traffic received by this session.
#[derive(uniffi::Enum, Clone)]
pub enum WaddleMucInvitationEvent {
    Invited { invitation: WaddleMucInvitation },
    Declined { decline: WaddleMucInvitationDecline },
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Send a mediated invitation to the room. Use a bare invitee JID for Waddle rooms.
    /// Failure reported by the server arrives as a message rejection event.
    pub async fn invite_to_room(
        &self,
        room_jid: String,
        invitee_jid: String,
        reason: Option<String>,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let invitee = Jid::new(&invitee_jid).map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        handle
            .invite_to_room(&room, &invitee, reason.map(Reason))
            .await
            .map_err(|error| client_error_to_waddle(&error))
    }

    /// Send a decline to the room that relayed the invitation.
    pub async fn decline_room_invitation(
        &self,
        room_jid: String,
        inviter_jid: String,
        reason: Option<String>,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let inviter = Jid::new(&inviter_jid).map_err(|_| WaddleError::InvalidJid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        handle
            .decline_room_invitation(&room, &inviter, reason.map(Reason))
            .await
            .map_err(|error| client_error_to_waddle(&error))
    }
}

impl From<MucInvitationEvent> for WaddleMucInvitationEvent {
    fn from(event: MucInvitationEvent) -> Self {
        match event {
            MucInvitationEvent::Invited(invitation) => Self::Invited {
                invitation: WaddleMucInvitation {
                    room: invitation.room,
                    inviter: invitation.inviter,
                    reason: invitation.reason.map(|reason| reason.0),
                    password: invitation
                        .password
                        .map(|password| password.as_str().to_owned()),
                    continuation: invitation.continuation.map(|continuation| {
                        WaddleMucInvitationContinuation {
                            thread: continuation.thread,
                        }
                    }),
                },
            },
            MucInvitationEvent::Declined(decline) => Self::Declined {
                decline: WaddleMucInvitationDecline {
                    room: decline.room,
                    invitee: decline.invitee,
                    reason: decline.reason.map(|reason| reason.0),
                },
            },
        }
    }
}
