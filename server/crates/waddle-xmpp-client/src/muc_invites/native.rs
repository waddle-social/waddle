use jid::{BareJid, Jid};
use xmpp_parsers::muc::user::Reason;

use super::{build_muc_decline_message, build_muc_invite_message};
use crate::client::ClientHandle;
use crate::error::ClientResult;

impl ClientHandle {
    /// Queue a mediated invitation addressed to the room.
    ///
    /// A server refusal arrives through the event stream as a message error.
    /// # Errors
    /// Returns `ClientError::Disconnected` if the driver command channel is closed.
    pub async fn invite_to_room(
        &self,
        room: &BareJid,
        invitee: &Jid,
        reason: Option<Reason>,
    ) -> ClientResult<()> {
        self.send_stanza(build_muc_invite_message(room, invitee, reason).into())
            .await
    }

    /// Queue a decline addressed to the room that sent the invitation.
    ///
    /// A server refusal arrives through the event stream as a message error.
    /// # Errors
    /// Returns `ClientError::Disconnected` if the driver command channel is closed.
    pub async fn decline_room_invitation(
        &self,
        room: &BareJid,
        inviter: &Jid,
        reason: Option<Reason>,
    ) -> ClientResult<()> {
        self.send_stanza(build_muc_decline_message(room, inviter, reason).into())
            .await
    }
}
