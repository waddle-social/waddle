use std::time::Duration;

use jid::BareJid;

use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
use crate::event::ClientEvent;

use super::ids::next_id;
use super::iq::{
    build_disable_push_iq, build_disco_info_iq, build_disco_items_iq, build_enable_push_iq,
    build_inbox_iq, build_pubsub_items_iq, build_upload_slot_iq,
};
use super::parsing::{
    parse_disco_info_result, parse_disco_items_result, parse_inbox_result,
    parse_space_channels_result, parse_upload_slot, space_from_disco_item,
};
use super::types::{
    DiscoInfoResult, DiscoItem, DiscoveredChannel, DiscoveredChannelType, DiscoveredSpace,
    DiscoveredTopology, InboxEntry, SpaceNode, UploadSlot,
};
use super::{UPLOAD_NS, WADDLE_ROOM_METADATA_FORM_TYPE};

// ── Private helpers ───────────────────────────────────────────────────────────

#[cfg(feature = "native")]
fn parse_error() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "bad-request".to_string(),
        text: Some("response could not be parsed".to_string()),
    })
}

// ── Extension trait ──────────────────────────────────────────────────────────

#[allow(async_fn_in_trait)]
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait DiscoveryExt {
    /// Query `disco#info` for a JID, optionally scoped to a node.
    async fn discover_info(&self, jid: &str, node: Option<&str>) -> ClientResult<DiscoInfoResult>;

    /// Query `disco#items` for a JID.
    async fn discover_items(&self, jid: &str, node: Option<&str>) -> ClientResult<Vec<DiscoItem>>;

    /// Discover the HTTP upload service under `server_domain`.
    ///
    /// Queries `disco#items` on the domain and then `disco#info` on each item
    /// until one advertises `urn:xmpp:http:upload:0`.  Returns its JID, or
    /// `None` if no matching component is found.
    async fn discover_upload_service(&self, server_domain: &str) -> ClientResult<Option<String>>;

    /// Request an HTTP upload slot from `service_jid` (XEP-0363).
    async fn request_upload_slot(
        &self,
        service_jid: &str,
        filename: &str,
        size: u64,
        content_type: &str,
    ) -> ClientResult<UploadSlot>;

    /// Fetch inbox entries (XEP-0430 / erlang-solutions inbox extension).
    ///
    /// Sends the inbox IQ and collects streamed `UnhandledStanza` results until
    /// the final `<fin>` IQ result arrives or the 30-second timeout fires.
    async fn fetch_inbox(&self, max: u32) -> ClientResult<Vec<InboxEntry>>;

    /// Enable push notifications via a push service (XEP-0357).
    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        token: &str,
    ) -> ClientResult<()>;

    /// Disable push notifications for a previously-registered push node.
    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
    ) -> ClientResult<()>;

    /// Discover XEP-0503 spaces from the spaces service.
    async fn discover_spaces(&self, spaces_jid: &BareJid) -> ClientResult<Vec<DiscoveredSpace>>;

    /// Discover bookmark-backed MUC channels within one XEP-0503 space.
    async fn discover_space_channels(
        &self,
        spaces_jid: &BareJid,
        space_id: &SpaceNode,
    ) -> ClientResult<Vec<DiscoveredChannel>>;

    /// Discover the native spaces + channels topology.
    async fn discover_topology(&self, spaces_jid: &BareJid) -> ClientResult<DiscoveredTopology>;
}

// ── Implementation ────────────────────────────────────────────────────────────

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl DiscoveryExt for ClientHandle {
    async fn discover_info(&self, jid: &str, node: Option<&str>) -> ClientResult<DiscoInfoResult> {
        let iq = build_disco_info_iq(jid, node);
        let result = self.send_iq(iq).await?;
        parse_disco_info_result(&result, jid).ok_or_else(parse_error)
    }

    async fn discover_items(&self, jid: &str, node: Option<&str>) -> ClientResult<Vec<DiscoItem>> {
        let iq = build_disco_items_iq(jid, node);
        let result = self.send_iq(iq).await?;
        parse_disco_items_result(&result).ok_or_else(parse_error)
    }

    async fn discover_upload_service(&self, server_domain: &str) -> ClientResult<Option<String>> {
        let items = self.discover_items(server_domain, None).await?;
        for item in items {
            if let Ok(info) = self.discover_info(&item.jid, None).await {
                if info.has_feature(UPLOAD_NS) {
                    return Ok(Some(item.jid));
                }
            }
        }
        Ok(None)
    }

    async fn request_upload_slot(
        &self,
        service_jid: &str,
        filename: &str,
        size: u64,
        content_type: &str,
    ) -> ClientResult<UploadSlot> {
        let iq = build_upload_slot_iq(service_jid, filename, size, content_type);
        let result = self.send_iq(iq).await?;
        parse_upload_slot(&result).ok_or_else(parse_error)
    }

    async fn fetch_inbox(&self, max: u32) -> ClientResult<Vec<InboxEntry>> {
        // Subscribe before sending so we don't miss early stanzas.
        let mut events = self.events();

        let query_id = format!("inbox-q-{}", next_id());
        let snapshot = self.snapshot();
        let binding = snapshot.binding.ok_or(ClientError::Disconnected)?;
        let bare_jid = {
            let full = binding.jid.to_string();
            if let Some(idx) = full.rfind('/') {
                full[..idx].to_string()
            } else {
                full
            }
        };
        let iq = build_inbox_iq(&bare_jid, &query_id, max);

        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<ClientResult<()>>();
        let handle = self.clone();
        tokio::spawn(async move {
            let res = handle.send_iq(iq).await.map(|_| ());
            let _ = done_tx.send(res);
        });

        let mut entries = Vec::new();
        let sleep = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                result = &mut done_rx => {
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => return Err(e),
                        Err(_) => return Err(ClientError::Disconnected),
                    }
                    break;
                }
                event = events.recv() => {
                    if let Ok(ClientEvent::UnhandledStanza(el)) = event {
                        if let Some(entry) = parse_inbox_result(&el) {
                            entries.push(entry);
                        }
                    }
                }
                _ = &mut sleep => {
                    break;
                }
            }
        }

        Ok(entries)
    }

    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        token: &str,
    ) -> ClientResult<()> {
        let iq = build_enable_push_iq(push_service_jid, node, token);
        self.send_iq(iq).await.map(|_| ())
    }

    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
    ) -> ClientResult<()> {
        let iq = build_disable_push_iq(push_service_jid, node);
        self.send_iq(iq).await.map(|_| ())
    }

    async fn discover_spaces(&self, spaces_jid: &BareJid) -> ClientResult<Vec<DiscoveredSpace>> {
        let items = self.discover_items(&spaces_jid.to_string(), None).await?;
        let mut spaces = Vec::new();
        for item in items {
            let Some(node) = item.node.as_deref() else {
                continue;
            };
            let Ok(info) = self
                .discover_info(&spaces_jid.to_string(), Some(node))
                .await
            else {
                continue;
            };
            if let Some(space) = space_from_disco_item(spaces_jid, item, &info) {
                spaces.push(space);
            }
        }
        Ok(spaces)
    }

    async fn discover_space_channels(
        &self,
        spaces_jid: &BareJid,
        space_id: &SpaceNode,
    ) -> ClientResult<Vec<DiscoveredChannel>> {
        let iq = build_pubsub_items_iq(spaces_jid, space_id);
        let result = self.send_iq(iq).await?;
        let mut channels =
            parse_space_channels_result(&result, space_id).ok_or_else(parse_error)?;
        for channel in &mut channels {
            if let Ok(info) = self
                .discover_info(&channel.room_jid.to_string(), None)
                .await
            {
                if let Some(channel_type) = info
                    .form_value(WADDLE_ROOM_METADATA_FORM_TYPE, "waddle#channel_type")
                    .and_then(DiscoveredChannelType::from_metadata)
                {
                    channel.channel_type = channel_type;
                }
                channel.description = info
                    .form_value(
                        "http://jabber.org/protocol/muc#roominfo",
                        "muc#roominfo_description",
                    )
                    .filter(|description| !description.trim().is_empty())
                    .map(str::to_string);
            }
        }
        Ok(channels)
    }

    async fn discover_topology(&self, spaces_jid: &BareJid) -> ClientResult<DiscoveredTopology> {
        let spaces = self.discover_spaces(spaces_jid).await?;
        let mut channels = Vec::new();
        for space in &spaces {
            channels.extend(self.discover_space_channels(spaces_jid, &space.id).await?);
        }
        Ok(DiscoveredTopology { spaces, channels })
    }
}
