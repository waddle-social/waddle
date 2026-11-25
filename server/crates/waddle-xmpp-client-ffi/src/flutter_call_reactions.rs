use crate::{WaddleClient, WaddleError};
use waddle_xmpp_client::messaging::{
    build_in_call_reaction_message, InCallReactionEmoji, InCallReactionRecipient, InCallSessionId,
};

impl WaddleClient {
    /// Send a transient reaction to a peer resource or a room using the active call's SID.
    ///
    /// # Errors
    /// Returns an error for an invalid destination, emoji or session identifier.
    pub async fn send_in_call_reaction(
        &self,
        to: String,
        msg_type: String,
        sid: String,
        emoji: String,
    ) -> Result<bool, WaddleError> {
        let recipient = match msg_type.as_str() {
            "chat" => {
                InCallReactionRecipient::Direct(to.parse().map_err(|_| WaddleError::InvalidJid)?)
            }
            "groupchat" => {
                InCallReactionRecipient::Room(to.parse().map_err(|_| WaddleError::InvalidJid)?)
            }
            _ => return Err(WaddleError::InvalidArgument),
        };
        let sid = InCallSessionId::try_new(sid).map_err(|_| WaddleError::InvalidArgument)?;
        let emoji =
            InCallReactionEmoji::try_new(emoji).map_err(|_| WaddleError::InvalidArgument)?;
        let message = build_in_call_reaction_message(&recipient, &sid, &emoji);
        Ok(self
            .send_stanza_or_error(message, "send_in_call_reaction")
            .await)
    }
}
