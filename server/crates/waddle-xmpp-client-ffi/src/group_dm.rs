//! Group-DM lifecycle verb exports: the `urn:waddle:group-dm:*`
//! XEP-0050 commands (create to the bare server domain, rename to the
//! room, leave to the bare server domain) plus the XEP-0045 §7.8.2
//! mediated add-member invite with the Waddle history-access
//! extension.
//!
//! Every method parses its untyped FFI inputs (JID strings) into
//! typed values immediately, then delegates to the shared
//! `waddle_xmpp_client::group_dm` builders/parsers. All verbs are
//! `Result<_, WaddleError>` surfaces: the group-DM UI needs the
//! stanza-error condition (`forbidden`, `conflict`, …) to explain a
//! refusal.

use uuid::Uuid;

use waddle_xmpp_client::group_dm as core;

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};

impl WaddleClient {
    /// Send one group-DM command IQ and hand back the raw result
    /// element. Error IQs surface as typed stanza errors.
    async fn send_group_dm_command(
        &self,
        iq: minidom::Element,
    ) -> Result<minidom::Element, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// `urn:waddle:group-dm:create:0`: create a hidden members-only
    /// group-DM room. `member_jids` must include the caller; the
    /// server requires at least two distinct JIDs. Returns the bare
    /// JID of the new room.
    pub async fn create_group_dm(
        &self,
        name: String,
        member_jids: Vec<String>,
    ) -> Result<String, WaddleError> {
        let member_jids = member_jids
            .iter()
            .map(|jid| self.require_bare_jid(jid))
            .collect::<Result<Vec<_>, WaddleError>>()?;
        let args = core::GroupDmCreateArgs { name, member_jids };
        let iq = core::build_group_dm_create_iq(
            &self.server_domain(),
            &args,
            &Uuid::new_v4().to_string(),
        );
        let result = self.send_group_dm_command(iq).await?;
        core::parse_group_dm_create_result(&result)
            .map(|created| created.room_jid.to_string())
            .map_err(|_| WaddleError::MalformedResponse)
    }

    /// `urn:waddle:group-dm:rename:0`: set (or, with `None`, clear)
    /// the group-DM display name. The server rewrites every member's
    /// bookmark, so a later topology refresh picks up the new name.
    pub async fn rename_group_dm(
        &self,
        room_jid: String,
        name: Option<String>,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let iq =
            core::build_group_dm_rename_iq(&room, name.as_deref(), &Uuid::new_v4().to_string());
        let result = self.send_group_dm_command(iq).await?;
        core::parse_group_dm_rename_result(&result)
            .map(|_| ())
            .map_err(|_| WaddleError::MalformedResponse)
    }

    /// `urn:waddle:group-dm:leave:0`: leave a group DM. The server
    /// retracts the caller's bookmark, so the room drops out of the
    /// next topology refresh.
    pub async fn leave_group_dm(&self, room_jid: String) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let iq = core::build_group_dm_leave_iq(
            &self.server_domain(),
            &room,
            &Uuid::new_v4().to_string(),
        );
        let result = self.send_group_dm_command(iq).await?;
        core::parse_group_dm_leave_result(&result)
            .map(|_| ())
            .map_err(|_| WaddleError::MalformedResponse)
    }

    /// XEP-0045 §7.8.2 mediated invite adding `invitee_jid` to a
    /// group DM. `full_history` requests the full-archive grant via
    /// the Waddle history-access extension; `false` omits the child
    /// and the server applies its from-join default. Fire-and-forget
    /// at the stanza level: the server answers refusals with a typed
    /// message error, delivered through the regular event stream.
    pub async fn invite_to_group_dm(
        &self,
        room_jid: String,
        invitee_jid: String,
        full_history: bool,
    ) -> Result<(), WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        let invitee = self.require_bare_jid(&invitee_jid)?;
        let access = if full_history {
            core::GroupDmHistoryAccess::Full
        } else {
            core::GroupDmHistoryAccess::FromJoin
        };
        let message = core::build_group_dm_invite_message(&room, &invitee, access);
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        handle
            .send_stanza(message)
            .await
            .map_err(|e| client_error_to_waddle(&e))
    }
}
