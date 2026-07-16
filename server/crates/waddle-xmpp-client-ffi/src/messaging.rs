use jid::{BareJid, Jid};
use waddle_xmpp_client::{
    avatar::AvatarExt, discovery::DiscoveryExt, mam::MamExt, messaging::MessagingExt,
};

use crate::boundary_convert::{
    empty_mam_page, empty_topology, jid_domain, mam_page_to_ffi, topology_to_ffi,
    upload_slot_to_ffi,
};
use crate::convert::send_options_from_ffi;
use crate::send_outcome::send_failure_outcome;
use crate::{
    WaddleAvatar, WaddleClient, WaddleMamPage, WaddleSendMessageOutcome, WaddleSendOptions,
    WaddleTopology, WaddleUploadSlot,
};

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    pub async fn fetch_room_history(
        &self,
        room_jid: String,
        max_messages: u32,
        before_id: Option<String>,
    ) -> WaddleMamPage {
        let Some(handle) = self.clone_handle().await else {
            return empty_mam_page();
        };

        match handle
            .fetch_room_history(&room_jid, max_messages, before_id.as_deref())
            .await
        {
            Ok(page) => mam_page_to_ffi(page),
            Err(e) => {
                self.emit_error(format!("fetch_room_history failed: {e}"));
                empty_mam_page()
            }
        }
    }

    pub async fn fetch_dm_history(
        &self,
        peer_jid: String,
        max_messages: u32,
        before_id: Option<String>,
    ) -> WaddleMamPage {
        let Some(handle) = self.clone_handle().await else {
            return empty_mam_page();
        };

        match handle
            .fetch_dm_history(&peer_jid, max_messages, before_id.as_deref())
            .await
        {
            Ok(page) => mam_page_to_ffi(page),
            Err(e) => {
                self.emit_error(format!("fetch_dm_history failed: {e}"));
                empty_mam_page()
            }
        }
    }

    /// Full-text search of a room archive (XEP-0313 extended form field
    /// `{urn:xmpp:fulltext:0}fulltext`). Returns the newest matching page
    /// first; an empty page plus an `Error` event on failure, mirroring
    /// `fetch_room_history`.
    pub async fn search_room_history(
        &self,
        room_jid: String,
        query: String,
        max_messages: u32,
    ) -> WaddleMamPage {
        let Some(handle) = self.clone_handle().await else {
            return empty_mam_page();
        };

        match handle
            .search_room_history(&room_jid, &query, max_messages)
            .await
        {
            Ok(page) => mam_page_to_ffi(page),
            Err(e) => {
                self.emit_error(format!("search_room_history failed: {e}"));
                empty_mam_page()
            }
        }
    }

    /// Full-text search of the account's personal archive, filtered to a
    /// 1:1 peer via the standard `with` field. Returns the newest matching
    /// page first; an empty page plus an `Error` event on failure,
    /// mirroring `fetch_dm_history`.
    pub async fn search_dm_history(
        &self,
        peer_jid: String,
        query: String,
        max_messages: u32,
    ) -> WaddleMamPage {
        let Some(handle) = self.clone_handle().await else {
            return empty_mam_page();
        };

        match handle
            .search_dm_history(&peer_jid, &query, max_messages)
            .await
        {
            Ok(page) => mam_page_to_ffi(page),
            Err(e) => {
                self.emit_error(format!("search_dm_history failed: {e}"));
                empty_mam_page()
            }
        }
    }

    pub async fn send_groupchat_message(
        &self,
        room_jid: String,
        body: String,
        options: Option<WaddleSendOptions>,
    ) -> WaddleSendMessageOutcome {
        let room_jid = match room_jid.parse::<BareJid>() {
            Ok(jid) => jid,
            Err(e) => {
                self.emit_error(format!(
                    "send_groupchat_message failed: invalid room JID: {e}"
                ));
                return WaddleSendMessageOutcome::InvalidRecipient;
            }
        };
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.emit_error(format!("send_groupchat_message failed: {e}"));
                return WaddleSendMessageOutcome::InvalidOptions;
            }
        };
        let Some(handle) = self.clone_handle().await else {
            return WaddleSendMessageOutcome::NotConnected;
        };

        match handle
            .send_groupchat_message(room_jid.as_str(), &body, &opts)
            .await
        {
            Ok(stanza_id) => WaddleSendMessageOutcome::Sent {
                stanza_id: stanza_id.to_string(),
            },
            Err(e) => {
                self.emit_error(format!("send_groupchat_message failed: {e}"));
                send_failure_outcome(&e)
            }
        }
    }

    pub async fn send_chat_message(
        &self,
        peer_jid: String,
        body: String,
        options: Option<WaddleSendOptions>,
    ) -> WaddleSendMessageOutcome {
        let peer_jid = match peer_jid.parse::<Jid>() {
            Ok(jid) => jid,
            Err(e) => {
                self.emit_error(format!("send_chat_message failed: invalid peer JID: {e}"));
                return WaddleSendMessageOutcome::InvalidRecipient;
            }
        };
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.emit_error(format!("send_chat_message failed: {e}"));
                return WaddleSendMessageOutcome::InvalidOptions;
            }
        };
        let Some(handle) = self.clone_handle().await else {
            return WaddleSendMessageOutcome::NotConnected;
        };

        match handle
            .send_chat_message(peer_jid.as_str(), &body, &opts)
            .await
        {
            Ok(stanza_id) => WaddleSendMessageOutcome::Sent {
                stanza_id: stanza_id.to_string(),
            },
            Err(e) => {
                self.emit_error(format!("send_chat_message failed: {e}"));
                send_failure_outcome(&e)
            }
        }
    }

    pub async fn join_room(&self, room_jid: String, nick: String) {
        let Some(handle) = self.clone_handle().await else {
            return;
        };

        if let Err(e) = handle.join_room(&room_jid, &nick).await {
            self.emit_error(format!("join_room failed: {e}"));
        }
    }

    pub async fn leave_room(&self, room_jid: String, nick: String) {
        let Some(handle) = self.clone_handle().await else {
            return;
        };

        if let Err(e) = handle.leave_room(&room_jid, &nick).await {
            self.emit_error(format!("leave_room failed: {e}"));
        }
    }

    pub async fn discover_topology(&self) -> WaddleTopology {
        let Some(handle) = self.clone_handle().await else {
            return empty_topology();
        };
        let server_domain = jid_domain(&self.config.jid);

        match handle.discover_topology(server_domain).await {
            Ok(topology) => topology_to_ffi(topology),
            Err(e) => {
                self.emit_error(format!(
                    "discover_topology failed: server_domain={server_domain} error={e}"
                ));
                empty_topology()
            }
        }
    }

    pub async fn send_presence(
        &self,
        status: Option<String>,
        show: Option<String>,
        idle_since: Option<String>,
    ) {
        let Some(handle) = self.clone_handle().await else {
            return;
        };

        if let Err(e) = handle
            .send_presence(status.as_deref(), show.as_deref(), idle_since.as_deref())
            .await
        {
            self.emit_error(format!("send_presence failed: {e}"));
        }
    }

    /// Request the XEP-0084 avatar for a user. Returns `None` when the target
    /// JID hasn't published an avatar or the fetch failed; errors are
    /// reported on the event listener so the caller can treat `None` as
    /// "fall back to initials".
    pub async fn request_avatar(&self, jid: String) -> Option<WaddleAvatar> {
        let bare: BareJid = match jid.parse() {
            Ok(j) => j,
            Err(e) => {
                self.emit_error(format!("Invalid JID for avatar fetch: {e}"));
                return None;
            }
        };
        let handle = self.clone_handle().await?;

        match handle.request_avatar(&bare).await {
            Ok(Some(avatar)) => Some(WaddleAvatar {
                jid: avatar.jid.to_string(),
                id: avatar.id,
                mime_type: avatar.mime_type,
                data: avatar.data,
                url: avatar.url,
            }),
            Ok(None) => None,
            Err(e) => {
                self.emit_error(format!("request_avatar failed: {e}"));
                None
            }
        }
    }

    pub async fn discover_upload_service(&self) -> Option<String> {
        let handle = self.clone_handle().await?;

        match handle
            .discover_upload_service(jid_domain(&self.config.jid))
            .await
        {
            Ok(service_jid) => service_jid,
            Err(e) => {
                self.emit_error(format!("discover_upload_service failed: {e}"));
                None
            }
        }
    }

    pub async fn request_upload_slot(
        &self,
        service_jid: String,
        filename: String,
        size: u64,
        content_type: String,
    ) -> Option<WaddleUploadSlot> {
        let handle = self.clone_handle().await?;

        match handle
            .request_upload_slot(&service_jid, &filename, size, &content_type)
            .await
        {
            Ok(slot) => Some(upload_slot_to_ffi(slot)),
            Err(e) => {
                self.emit_error(format!("request_upload_slot failed: {e}"));
                None
            }
        }
    }
}
