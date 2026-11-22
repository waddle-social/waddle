//! Native client service-discovery and roster request operations.

use futures::stream::{self, StreamExt};
use jid::BareJid;
use std::collections::HashSet;

use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};

use super::iq::{
    build_disco_info_iq, build_disco_items_iq, build_pubsub_items_iq, build_upload_slot_iq,
};
use super::parsing::{
    parse_disco_info_result, parse_disco_items_result, parse_space_channels_result,
    parse_upload_slot, resolve_component_services, room_bots_from_disco_items,
    space_from_disco_item,
};
use super::types::{
    DiscoInfoResult, DiscoItem, DiscoveredChannel, DiscoveredChannelType,
    DiscoveredComponentServices, DiscoveredSpace, DiscoveredTopology, RoomBot, SpaceNode,
    UploadSlot,
};
use super::{STANDALONE_SPACE_ID, UPLOAD_NS, WADDLE_ROOM_METADATA_FORM_TYPE};

const DISCO_CONCURRENCY: usize = 8;

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
/// Native client operations for discovery, upload slots, push settings, and channel topology.
pub trait DiscoveryExt {
    /// Query `disco#info` for a JID, optionally scoped to a node.
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
    fn discover_info<'a>(
        &'a self,
        jid: &'a str,
        node: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<DiscoInfoResult>> + Send + 'a;

    /// Query `disco#items` for a JID.
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
    fn discover_items<'a>(
        &'a self,
        jid: &'a str,
        node: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<Vec<DiscoItem>>> + Send + 'a;

    /// The extension bots that have posted in `room` (Waddle's
    /// [`NODE_WADDLE_ROOM_BOTS`](super::NODE_WADDLE_ROOM_BOTS) disco#items
    /// node). Empty for a requester who may not enter the room.
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
    fn discover_room_bots<'a>(
        &'a self,
        room: &'a BareJid,
    ) -> impl std::future::Future<Output = ClientResult<Vec<RoomBot>>> + Send + 'a;

    /// Discover the HTTP upload service under `server_domain`.
    ///
    /// Queries `disco#items` on the domain and then `disco#info` on each item
    /// until one advertises `urn:xmpp:http:upload:0`.  Returns its JID, or
    /// `None` if no matching component is found.
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
    fn discover_upload_service<'a>(
        &'a self,
        server_domain: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<Option<String>>> + Send + 'a;

    /// Request an HTTP upload slot from `service_jid` (XEP-0363).
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response. Returns a bad-request error if the response lacks a valid upload slot.
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
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
    fn enable_push_notifications<'a>(
        &'a self,
        push_service_jid: &'a str,
        node: &'a str,
        publish_options: Option<xmpp_parsers::data_forms::DataForm>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;

    /// Disable push notifications. A `None` `node` disables ALL nodes
    /// at the service for this user (XEP-0357 §6.1).
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
    fn disable_push_notifications<'a>(
        &'a self,
        push_service_jid: &'a str,
        node: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;

    /// Discover XEP-0503 spaces from the spaces service.
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
    fn discover_spaces<'a>(
        &'a self,
        spaces_jid: &'a BareJid,
    ) -> impl std::future::Future<Output = ClientResult<Vec<DiscoveredSpace>>> + Send + 'a;

    /// Discover bookmark-backed MUC channels within one XEP-0503 space.
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response.
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
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response. Invalid derived service JIDs also return a validation error.
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
    ///
    /// # Errors
    ///
    /// Propagates disconnection, IQ timeout, stanza-error, or transport failures while awaiting the response. Invalid derived service JIDs also return a validation error.
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

    async fn discover_room_bots(&self, room: &BareJid) -> ClientResult<Vec<RoomBot>> {
        self.discover_items(room.as_str(), Some(super::NODE_WADDLE_ROOM_BOTS))
            .await
            .map(room_bots_from_disco_items)
    }

    async fn discover_upload_service(&self, server_domain: &str) -> ClientResult<Option<String>> {
        let items = self.discover_items(server_domain, None).await?;
        let mut probes = std::pin::pin!(stream::iter(items)
            .map(|item| async move {
                let is_upload = self
                    .discover_info(&item.jid, None)
                    .await
                    .is_ok_and(|info| info.has_feature(UPLOAD_NS));
                (item.jid, is_upload)
            })
            .buffered(DISCO_CONCURRENCY));
        while let Some((jid, is_upload)) = probes.next().await {
            if is_upload {
                return Ok(Some(jid));
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
        let spaces_jid_str = spaces_jid.to_string();
        let items = self.discover_items(&spaces_jid_str, None).await?;
        let spaces: Vec<DiscoveredSpace> = stream::iter(items)
            .map(|item| {
                let spaces_jid_str = spaces_jid_str.as_str();
                async move {
                    let node = item.node.clone()?;
                    let info = self
                        .discover_info(spaces_jid_str, Some(node.as_str()))
                        .await
                        .ok()?;
                    space_from_disco_item(spaces_jid, item, &info)
                }
            })
            .buffered(DISCO_CONCURRENCY)
            .filter_map(|space| async move { space })
            .collect()
            .await;
        Ok(spaces)
    }

    async fn discover_space_channels(
        &self,
        spaces_jid: &BareJid,
        space_id: &SpaceNode,
    ) -> ClientResult<Vec<DiscoveredChannel>> {
        let iq = build_pubsub_items_iq(spaces_jid, space_id);
        let result = self.send_iq(iq).await?;
        let channels = parse_space_channels_result(&result, space_id).ok_or_else(parse_error)?;
        let hydrated = stream::iter(channels)
            .map(|mut channel| async move {
                let room_jid = channel.room_jid.to_string();
                if let Ok(info) = self.discover_info(&room_jid, None).await {
                    if let Some(channel_type) = info
                        .form_value(WADDLE_ROOM_METADATA_FORM_TYPE, "waddle#channel_type")
                        .and_then(DiscoveredChannelType::from_metadata)
                    {
                        channel.channel_type = channel_type;
                    }
                    // The space item keeps the name it was published with, so
                    // it goes stale when an owner renames the room. The room's
                    // own answer is current.
                    if let Some(name) = info.room_name() {
                        channel.name = name.to_string();
                    }
                    channel.description = info
                        .form_value(
                            "http://jabber.org/protocol/muc#roominfo",
                            "muc#roominfo_description",
                        )
                        .filter(|description| !description.trim().is_empty())
                        .map(str::to_string);
                    (channel.is_group_dm, channel.members_only) = info.room_flags();
                }
                channel
            })
            .buffered(DISCO_CONCURRENCY)
            .collect()
            .await;
        Ok(hydrated)
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
        let valid_items: Vec<(BareJid, String)> = items
            .into_iter()
            .filter_map(|item| item.jid.parse::<BareJid>().ok().map(|jid| (jid, item.jid)))
            .collect();
        let hydrated: Vec<(BareJid, Option<DiscoInfoResult>)> = stream::iter(valid_items)
            .map(|(jid, raw_jid)| async move {
                let info = self.discover_info(&raw_jid, None).await.ok();
                (jid, info)
            })
            .buffered(DISCO_CONCURRENCY)
            .collect()
            .await;
        Ok(resolve_component_services(
            &hydrated,
            fallback_muc,
            fallback_spaces,
        ))
    }

    async fn discover_topology(&self, server_domain: &str) -> ClientResult<DiscoveredTopology> {
        let services = self.discover_component_services(server_domain).await?;
        let muc_jid_str = services.muc.to_string();

        // Issue the three top-level topology queries concurrently:
        // (1) XEP-0503 spaces, (2) direct MUC catalog, and (3) XEP-0402 user bookmarks.
        let spaces_fut = async {
            self.discover_spaces(&services.spaces)
                .await
                .unwrap_or_default()
        };
        let muc_rooms_fut = self.discover_items(&muc_jid_str, None);
        let bookmarks_fut = super::bookmarks::fetch_user_bookmarks(self);
        let (mut spaces, muc_rooms_res, bookmarks) =
            tokio::join!(spaces_fut, muc_rooms_fut, bookmarks_fut);
        let bookmarks_loaded = bookmarks.is_some();
        let bookmarks = bookmarks.unwrap_or_default();

        // Hydrate channels across spaces concurrently while preserving space order.
        let space_ids: Vec<SpaceNode> = spaces.iter().map(|space| space.id.clone()).collect();
        let spaces_jid = &services.spaces;
        let space_channel_batches: Vec<Vec<DiscoveredChannel>> = stream::iter(space_ids)
            .map(|space_id| async move {
                self.discover_space_channels(spaces_jid, &space_id)
                    .await
                    .unwrap_or_default()
            })
            .buffered(DISCO_CONCURRENCY)
            .collect()
            .await;
        let mut channels: Vec<DiscoveredChannel> =
            space_channel_batches.into_iter().flatten().collect();

        // (2) Direct MUC component enumeration. Any room not already
        // surfaced as a space bookmark is attached to a synthetic
        // "standalone" space so the UI's space-keyed channel list can
        // render every room a user might want to join without
        // requiring server-side bookmark administration first.
        let bookmarked: HashSet<BareJid> = channels
            .iter()
            .map(|channel| channel.room_jid.clone())
            .collect();

        let standalone_space_id =
            SpaceNode::try_new(STANDALONE_SPACE_ID).ok_or_else(parse_error)?;
        let mut standalone_count = 0usize;
        if let Ok(rooms) = muc_rooms_res {
            let standalone_candidates: Vec<(usize, DiscoItem, BareJid)> = rooms
                .into_iter()
                .enumerate()
                .filter_map(|(offset, room)| {
                    let room_jid = room.jid.parse::<BareJid>().ok()?;
                    (!bookmarked.contains(&room_jid)).then_some((offset, room, room_jid))
                })
                .collect();
            let standalone_channels: Vec<DiscoveredChannel> = stream::iter(standalone_candidates)
                .map(|(offset, room, room_jid)| {
                    let standalone_space_id = standalone_space_id.clone();
                    async move {
                        let mut channel_type = DiscoveredChannelType::Text;
                        let mut description = None;
                        let mut is_group_dm = false;
                        let mut members_only = None;
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
                            (is_group_dm, members_only) = info.room_flags();
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
                        DiscoveredChannel {
                            id,
                            room_jid,
                            name,
                            description,
                            channel_type,
                            position: offset as i32,
                            space_id: standalone_space_id,
                            // Non-bookmarked catalog rooms default to
                            // autojoin (web parity); the user's XEP-0402
                            // bookmark below can override this.
                            autojoin: true,
                            user_bookmark: None,
                            bookmark_name: None,
                            is_group_dm,
                            members_only,
                        }
                    }
                })
                .buffered(DISCO_CONCURRENCY)
                .collect()
                .await;
            standalone_count = standalone_channels.len();
            channels.extend(standalone_channels);
        }

        // (3) XEP-0402: the user's own-PEP bookmarks stamp
        // autojoin/name onto catalog rooms and append bookmark-only
        // rooms (kept only when disco#info confirms MUC). A failed
        // bookmarks fetch degrades to the catalog-only result (web
        // parity) and is reported as `bookmarks_loaded: false`.
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

        Ok(DiscoveredTopology {
            spaces,
            channels,
            bookmarks_loaded,
        })
    }
}
