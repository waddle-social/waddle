use jid::BareJid;

use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};

use super::iq::{
    build_disco_info_iq, build_disco_items_iq, build_pubsub_items_iq, build_upload_slot_iq,
};
use super::parsing::{
    parse_disco_info_result, parse_disco_items_result, parse_space_channels_result,
    parse_upload_slot, resolve_component_services, space_from_disco_item,
};
use super::types::{
    DiscoInfoResult, DiscoItem, DiscoveredChannel, DiscoveredChannelType,
    DiscoveredComponentServices, DiscoveredSpace, DiscoveredTopology, SpaceNode, UploadSlot,
};
use super::{
    STANDALONE_SPACE_ID, UPLOAD_NS, WADDLE_GROUP_DM_FEATURE_NS, WADDLE_ROOM_METADATA_FORM_TYPE,
};

// ── Private helpers ───────────────────────────────────────────────────────────

#[cfg(feature = "native")]
fn parse_error() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "bad-request".to_string(),
        text: Some("response could not be parsed".to_string()),
        application_condition: None,
    })
}

// ── Extension trait ──────────────────────────────────────────────────────────

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait DiscoveryExt {
    /// Query `disco#info` for a JID, optionally scoped to a node.
    fn discover_info<'a>(
        &'a self,
        jid: &'a str,
        node: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<DiscoInfoResult>> + Send + 'a;

    /// Query `disco#items` for a JID.
    fn discover_items<'a>(
        &'a self,
        jid: &'a str,
        node: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<Vec<DiscoItem>>> + Send + 'a;

    /// Discover the HTTP upload service under `server_domain`.
    ///
    /// Queries `disco#items` on the domain and then `disco#info` on each item
    /// until one advertises `urn:xmpp:http:upload:0`.  Returns its JID, or
    /// `None` if no matching component is found.
    fn discover_upload_service<'a>(
        &'a self,
        server_domain: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<Option<String>>> + Send + 'a;

    /// Request an HTTP upload slot from `service_jid` (XEP-0363).
    fn request_upload_slot<'a>(
        &'a self,
        service_jid: &'a str,
        filename: &'a str,
        size: u64,
        content_type: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<UploadSlot>> + Send + 'a;

    /// Enable push notifications via a push service (XEP-0357 §5).
    ///
    /// `publish_options` carries an optional XEP-0004 data form keyed
    /// on `http://jabber.org/protocol/pubsub#publish-options` — the
    /// user server passes it through verbatim. Provider credentials
    /// MUST NOT be carried here; they live behind the Push Service
    /// component (registered separately via XEP-0050).
    fn enable_push_notifications<'a>(
        &'a self,
        push_service_jid: &'a str,
        node: &'a str,
        publish_options: Option<xmpp_parsers::data_forms::DataForm>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;

    /// Disable push notifications. A `None` `node` disables ALL nodes
    /// at the service for this user (XEP-0357 §6.1).
    fn disable_push_notifications<'a>(
        &'a self,
        push_service_jid: &'a str,
        node: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;

    /// Discover XEP-0503 spaces from the spaces service.
    fn discover_spaces<'a>(
        &'a self,
        spaces_jid: &'a BareJid,
    ) -> impl std::future::Future<Output = ClientResult<Vec<DiscoveredSpace>>> + Send + 'a;

    /// Discover bookmark-backed MUC channels within one XEP-0503 space.
    fn discover_space_channels<'a>(
        &'a self,
        spaces_jid: &'a BareJid,
        space_id: &'a SpaceNode,
    ) -> impl std::future::Future<Output = ClientResult<Vec<DiscoveredChannel>>> + Send + 'a;

    /// Resolve the MUC and Spaces service JIDs against `server_domain`
    /// via `disco#items` + `disco#info`. Falls back to the conventional
    /// `muc.<domain>` and `spaces.<domain>` subdomains when the
    /// server does not explicitly advertise the relevant features
    /// (XEP-0045 `http://jabber.org/protocol/muc` for MUC, the
    /// `urn:xmpp:spaces:0` feature for Spaces).
    fn discover_component_services<'a>(
        &'a self,
        server_domain: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<DiscoveredComponentServices>> + Send + 'a;

    /// Discover the native spaces + channels topology against the
    /// server domain. Resolves both the MUC and Spaces service JIDs
    /// (via [`discover_component_services`](Self::discover_component_services)),
    /// reads XEP-0503 space bookmarks, **and** enumerates the MUC
    /// component directly so rooms without a space bookmark still
    /// surface to the UI. Rooms not bookmarked into any space are
    /// attached to a synthetic space with id [`STANDALONE_SPACE_ID`]
    /// so a single channel list can be rendered without
    /// special-casing.
    fn discover_topology<'a>(
        &'a self,
        server_domain: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<DiscoveredTopology>> + Send + 'a;
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

    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        publish_options: Option<xmpp_parsers::data_forms::DataForm>,
    ) -> ClientResult<()> {
        let iq =
            crate::xep::xep0357::build_xep0357_enable_iq(push_service_jid, node, publish_options);
        self.send_iq(iq).await.map(|_| ())
    }

    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: Option<&str>,
    ) -> ClientResult<()> {
        let iq = crate::xep::xep0357::build_xep0357_disable_iq(push_service_jid, node);
        self.send_iq(iq).await.map(|_| ())
    }

    async fn discover_spaces(&self, spaces_jid: &BareJid) -> ClientResult<Vec<DiscoveredSpace>> {
        // XEP-0503 lists spaces as node-backed disco#items on the Spaces
        // service, with per-node PubSub metadata declaring
        // `pubsub#type=urn:xmpp:spaces:0`. Skip entries whose node metadata
        // cannot be verified as an XEP-0503 Space.
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
                channel.is_group_dm = info.has_feature(WADDLE_GROUP_DM_FEATURE_NS);
            }
        }
        Ok(channels)
    }

    async fn discover_component_services(
        &self,
        server_domain: &str,
    ) -> ClientResult<DiscoveredComponentServices> {
        // Conventional defaults: many waddle deployments don't advertise
        // their components via server disco (or the fallback is plenty).
        // Parse failures here would be a misconfigured server domain
        // and would have already failed connect; bubble up the error.
        let fallback_muc: BareJid =
            format!("muc.{server_domain}")
                .parse()
                .map_err(|e: jid::Error| {
                    ClientError::StanzaError(StanzaError {
                        error_type: StanzaErrorType::Cancel,
                        condition: "bad-request".to_string(),
                        text: Some(format!("invalid MUC fallback JID: {e}")),
                        application_condition: None,
                    })
                })?;
        let fallback_spaces: BareJid =
            format!("spaces.{server_domain}")
                .parse()
                .map_err(|e: jid::Error| {
                    ClientError::StanzaError(StanzaError {
                        error_type: StanzaErrorType::Cancel,
                        condition: "bad-request".to_string(),
                        text: Some(format!("invalid Spaces fallback JID: {e}")),
                        application_condition: None,
                    })
                })?;

        // Best-effort: if disco#items fails on the server domain (some
        // small servers reject it), fall back to the conventional
        // subdomains rather than refusing to load the topology entirely.
        let Ok(items) = self.discover_items(server_domain, None).await else {
            return Ok(DiscoveredComponentServices {
                muc: fallback_muc,
                spaces: fallback_spaces,
            });
        };

        // Hydrate each disco#items entry with its disco#info so the
        // service-selection logic can dispatch on advertised features.
        // Disco#info failures are tolerated: they downgrade the entry
        // to "feature unknown", which never wins against the fallback.
        let mut hydrated: Vec<(BareJid, Option<DiscoInfoResult>)> = Vec::new();
        for item in items {
            let Ok(jid) = item.jid.parse::<BareJid>() else {
                continue;
            };
            let info = self.discover_info(&item.jid, None).await.ok();
            hydrated.push((jid, info));
        }
        Ok(resolve_component_services(
            &hydrated,
            fallback_muc,
            fallback_spaces,
        ))
    }

    async fn discover_topology(&self, server_domain: &str) -> ClientResult<DiscoveredTopology> {
        let services = self.discover_component_services(server_domain).await?;

        // (1) XEP-0503 space bookmarks via the Spaces component.
        // discover_spaces returns an empty list when the user has no
        // spaces; that's fine — we still want to surface raw MUC rooms.
        let spaces = self
            .discover_spaces(&services.spaces)
            .await
            .unwrap_or_default();
        let mut channels: Vec<DiscoveredChannel> = Vec::new();
        for space in &spaces {
            if let Ok(space_channels) = self
                .discover_space_channels(&services.spaces, &space.id)
                .await
            {
                channels.extend(space_channels);
            }
        }

        // (2) Direct MUC component enumeration. Any room not already
        // surfaced as a space bookmark is attached to a synthetic
        // "standalone" space so the UI's space-keyed channel list can
        // render every room a user might want to join without
        // requiring server-side bookmark administration first.
        let bookmarked: std::collections::HashSet<BareJid> = channels
            .iter()
            .map(|channel| channel.room_jid.clone())
            .collect();

        let mut spaces = spaces;
        let standalone_space_id = SpaceNode::new(STANDALONE_SPACE_ID).ok_or_else(parse_error)?;
        let mut standalone_count = 0usize;
        if let Ok(rooms) = self.discover_items(&services.muc.to_string(), None).await {
            for (offset, room) in rooms.into_iter().enumerate() {
                let Ok(room_jid) = room.jid.parse::<BareJid>() else {
                    continue;
                };
                if bookmarked.contains(&room_jid) {
                    continue;
                }
                let mut channel_type = DiscoveredChannelType::Text;
                let mut description = None;
                let mut is_group_dm = false;
                if let Ok(info) = self.discover_info(&room.jid, None).await {
                    if let Some(parsed_type) = info
                        .form_value(WADDLE_ROOM_METADATA_FORM_TYPE, "waddle#channel_type")
                        .and_then(DiscoveredChannelType::from_metadata)
                    {
                        channel_type = parsed_type;
                    }
                    description = info
                        .form_value(
                            "http://jabber.org/protocol/muc#roominfo",
                            "muc#roominfo_description",
                        )
                        .filter(|description| !description.trim().is_empty())
                        .map(str::to_string);
                    is_group_dm = info.has_feature(WADDLE_GROUP_DM_FEATURE_NS);
                }
                let name = room
                    .name
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| {
                        room_jid
                            .node()
                            .map(|node| node.as_str().to_string())
                            .unwrap_or_else(|| room_jid.to_string())
                    });
                let id = format!("{}::{}", STANDALONE_SPACE_ID, room_jid);
                channels.push(DiscoveredChannel {
                    id,
                    room_jid,
                    name,
                    description,
                    channel_type,
                    position: offset as i32,
                    space_id: standalone_space_id.clone(),
                    // Non-bookmarked catalog rooms default to
                    // autojoin (web parity); the user's XEP-0402
                    // bookmark below can override this.
                    autojoin: true,
                    bookmark_name: None,
                    is_group_dm,
                });
                standalone_count += 1;
            }
        }

        // (3) XEP-0402: the user's own-PEP bookmarks stamp
        // autojoin/name onto catalog rooms and append bookmark-only
        // rooms (kept only when disco#info confirms MUC). A failed
        // bookmarks fetch yields an empty list, degrading to the
        // catalog-only result (web parity).
        let bookmarks = super::bookmarks::fetch_user_bookmarks(self).await;
        let appended = super::bookmarks::merge_bookmarks_into_channels(
            self,
            &mut channels,
            bookmarks,
            &standalone_space_id,
        )
        .await;

        if standalone_count + appended > 0 {
            spaces.push(DiscoveredSpace {
                id: standalone_space_id,
                service_jid: services.muc.clone(),
                name: "Rooms".to_string(),
                description: None,
            });
        }

        Ok(DiscoveredTopology { spaces, channels })
    }
}
