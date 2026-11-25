//! Native Flutter operations for verified remote communities.
use crate::error::client_error_to_waddle;
use crate::{WaddleClient, WaddleError};
use jid::{BareJid, Jid};
use minidom::Element;
pub use prefs::WaddleSavedSpace;
pub use remote::{WaddleRemoteCommunity, WaddleRemoteSubscription};
use waddle_xmpp_client::messaging::MessagingExt;
use waddle_xmpp_client_community::{remote_community as remote, remote_preferences as prefs};

fn error(e: remote::Error<WaddleError>) -> WaddleError {
    match e {
        remote::Error::Invalid => WaddleError::InvalidArgument,
        remote::Error::Unexpected => WaddleError::MalformedResponse,
        remote::Error::Untrusted => WaddleError::UntrustedReply,
        remote::Error::Transport(e) => e,
    }
}
impl WaddleClient {
    fn remote_owner(&self) -> Result<BareJid, WaddleError> {
        self.config
            .jid
            .parse::<Jid>()
            .map(|jid| jid.to_bare())
            .map_err(|_| WaddleError::InvalidJid)
    }
    async fn remote_iq(&self, iq: Element) -> Result<Element, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        tokio::time::timeout(std::time::Duration::from_secs(45), handle.send_iq(iq))
            .await
            .map_err(|_| WaddleError::Timeout)?
            .map_err(|e| client_error_to_waddle(&e))
    }
    pub async fn describe_remote_community(
        &self,
        address: String,
    ) -> Result<WaddleRemoteCommunity, WaddleError> {
        remote::describe(|iq| self.remote_iq(iq), &address)
            .await
            .map_err(error)
    }
    pub async fn change_remote_subscription(
        &self,
        address: String,
        follow: bool,
        subid: Option<String>,
    ) -> Result<WaddleRemoteSubscription, WaddleError> {
        let owner = self.remote_owner()?;
        remote::subscribe(
            |iq| self.remote_iq(iq),
            &owner,
            &address,
            follow,
            subid.as_deref(),
        )
        .await
        .map_err(error)
    }
    pub async fn fetch_saved_remote_spaces(&self) -> Result<Vec<WaddleSavedSpace>, WaddleError> {
        let owner = self.remote_owner()?;
        match self.community_iq(prefs::list_iq()).await {
            Ok(result) => prefs::parse(&result, &owner).ok_or(WaddleError::MalformedResponse),
            Err(WaddleError::Stanza { condition, .. }) if condition == "item-not-found" => {
                Ok(vec![])
            }
            Err(e) => Err(e),
        }
    }
    pub async fn update_saved_remote_space(
        &self,
        entry: WaddleSavedSpace,
        forget: bool,
    ) -> Result<(), WaddleError> {
        let owner = self.remote_owner()?;
        prefs::update(|iq| self.community_iq(iq), &owner, &entry, forget)
            .await
            .map_err(error)
    }
    pub async fn update_remote_room_bookmark(
        &self,
        room_jid: String,
        name: Option<String>,
        autojoin: bool,
        forget: bool,
    ) -> Result<(), WaddleError> {
        use waddle_xmpp_client_community::remote_bookmark;
        let own = self.remote_owner()?;
        let result = match self.community_iq(remote_bookmark::read_iq()).await {
            Ok(r) => Some(r),
            Err(WaddleError::Stanza { condition, .. }) if condition == "item-not-found" => None,
            Err(e) => return Err(e),
        };
        remote_bookmark::update(
            |iq| self.community_iq(iq),
            &own,
            &room_jid,
            name,
            autojoin,
            forget,
            result.as_ref(),
        )
        .await
        .map_err(error)
    }
    pub async fn request_remote_room_presence(
        &self,
        room_jid: String,
        nick: String,
        join: bool,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        if join {
            let stanza = waddle_xmpp_client::messaging::build_muc_join_presence(&room, &nick)
                .map_err(|_| WaddleError::InvalidArgument)?;
            handle.send_stanza(stanza).await
        } else {
            handle.leave_room(room.as_str(), &nick).await
        }
        .map_err(|e| client_error_to_waddle(&e))
    }
}
