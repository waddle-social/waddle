//! UniFFI verbs and boundary conversion for XEP-0191 Blocking Command.

use jid::Jid;
use minidom::Element;
use uuid::Uuid;
use waddle_xmpp_client::blocking::{
    build_block_iq, build_block_list_iq, build_unblock_all_iq, build_unblock_iq,
    parse_block_list_result, BlockingPush,
};
use xmpp_parsers::iq::Iq;

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleBlockListChange, WaddleClient, WaddleError};

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Fetch the account's XEP-0191 block list after authentication and
    /// before changing it. This session receives block-list pushes only
    /// after it requests the list.
    pub async fn fetch_block_list(&self) -> Result<Vec<Jid>, WaddleError> {
        let result = self
            .send_blocking_iq(build_block_list_iq(&Uuid::new_v4().to_string()))
            .await?;
        let iq = Iq::try_from(result).map_err(|_| WaddleError::MalformedResponse)?;
        parse_block_list_result(&iq).ok_or(WaddleError::MalformedResponse)
    }

    /// Block one or more JIDs.
    pub async fn block_jids(&self, jids: Vec<String>) -> Result<(), WaddleError> {
        let iq = build_block_iq(&Uuid::new_v4().to_string(), parse_jids(&jids)?)
            .map_err(|_| WaddleError::InvalidArgument)?;
        self.send_blocking_iq(iq).await?;
        Ok(())
    }

    /// Unblock one or more JIDs.
    pub async fn unblock_jids(&self, jids: Vec<String>) -> Result<(), WaddleError> {
        let iq = build_unblock_iq(&Uuid::new_v4().to_string(), parse_jids(&jids)?)
            .map_err(|_| WaddleError::InvalidArgument)?;
        self.send_blocking_iq(iq).await?;
        Ok(())
    }

    /// Unblock every JID on the account's block list.
    pub async fn unblock_all(&self) -> Result<(), WaddleError> {
        self.send_blocking_iq(build_unblock_all_iq(&Uuid::new_v4().to_string()))
            .await?;
        Ok(())
    }
}

impl WaddleClient {
    async fn send_blocking_iq(&self, iq: Iq) -> Result<Element, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        send_iq_with_timeout(&handle, iq.into())
            .await
            .map_err(|error| client_error_to_waddle(&error))
    }
}

fn parse_jids(jids: &[String]) -> Result<Vec<Jid>, WaddleError> {
    jids.iter()
        .map(|jid| jid.parse().map_err(|_| WaddleError::InvalidJid))
        .collect()
}

impl From<BlockingPush> for WaddleBlockListChange {
    fn from(change: BlockingPush) -> Self {
        match change {
            BlockingPush::Blocked(jids) => Self::Blocked { jids },
            BlockingPush::Unblocked(jids) => Self::Unblocked { jids },
            BlockingPush::UnblockedAll => Self::UnblockedAll,
        }
    }
}
