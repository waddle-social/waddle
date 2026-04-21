//! Service discovery (XEP-0030), HTTP upload (XEP-0363), inbox (XEP-0430),
//! push notifications, and custom Waddle channel creation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use minidom::Element;

use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
use crate::event::ClientEvent;

// ── Namespace constants ───────────────────────────────────────────────────────

pub const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";
pub const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";
pub const UPLOAD_NS: &str = "urn:xmpp:http:upload:0";
pub const INBOX_NS: &str = "erlang-solutions.com:xmpp:inbox:0";
pub const PUSH_NS: &str = "urn:xmpp:push:0";
pub const ADHOC_NS: &str = "http://jabber.org/protocol/commands";
pub const CLIENT_NS: &str = "jabber:client";
pub const DATA_FORMS_NS: &str = "jabber:x:data";
pub const RSM_NS: &str = "http://jabber.org/protocol/rsm";

// ── ID generation ────────────────────────────────────────────────────────────

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoFeature(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoIdentity {
    pub category: String,
    pub identity_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoInfoResult {
    pub jid: String,
    pub node: Option<String>,
    pub identities: Vec<DiscoIdentity>,
    pub features: Vec<String>,
}

impl DiscoInfoResult {
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoItem {
    pub jid: String,
    pub name: Option<String>,
    pub node: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadSlot {
    pub put_url: String,
    pub get_url: String,
    pub put_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboxEntry {
    pub jid: String,
    pub unread_count: u32,
    pub last_message_body: Option<String>,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredWaddle {
    pub id: String,
    pub name: String,
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredChannel {
    pub id: String,
    pub name: String,
    pub channel_type: String,
    pub position: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateChannelResult {
    pub room_jid: String,
    pub waddle_id: String,
    pub channel_id: String,
}

// ── Parse helpers ─────────────────────────────────────────────────────────────

/// Parse a disco#info result IQ into a [`DiscoInfoResult`].
pub fn parse_disco_info_result(iq: &Element, queried_jid: &str) -> Option<DiscoInfoResult> {
    let query = iq.get_child("query", DISCO_INFO_NS)?;
    let node = query.attr("node").map(str::to_string);

    let identities = query
        .children()
        .filter(|c| c.name() == "identity" && c.ns() == DISCO_INFO_NS)
        .map(|c| DiscoIdentity {
            category: c.attr("category").unwrap_or("").to_string(),
            identity_type: c.attr("type").unwrap_or("").to_string(),
            name: c.attr("name").map(str::to_string),
        })
        .collect();

    let features = query
        .children()
        .filter(|c| c.name() == "feature" && c.ns() == DISCO_INFO_NS)
        .filter_map(|c| c.attr("var").map(str::to_string))
        .collect();

    Some(DiscoInfoResult {
        jid: queried_jid.to_string(),
        node,
        identities,
        features,
    })
}

/// Parse a disco#items result IQ into a list of [`DiscoItem`]s.
pub fn parse_disco_items_result(iq: &Element) -> Option<Vec<DiscoItem>> {
    let query = iq.get_child("query", DISCO_ITEMS_NS)?;

    let items = query
        .children()
        .filter(|c| c.name() == "item" && c.ns() == DISCO_ITEMS_NS)
        .filter_map(|c| {
            let jid = c.attr("jid")?.to_string();
            Some(DiscoItem {
                jid,
                name: c.attr("name").map(str::to_string),
                node: c.attr("node").map(str::to_string),
            })
        })
        .collect();

    Some(items)
}

/// Parse an HTTP upload slot result IQ into an [`UploadSlot`].
pub fn parse_upload_slot(iq: &Element) -> Option<UploadSlot> {
    let slot = iq.get_child("slot", UPLOAD_NS)?;
    let put_el = slot.get_child("put", UPLOAD_NS)?;
    let get_el = slot.get_child("get", UPLOAD_NS)?;

    let put_url = put_el.attr("url")?.to_string();
    let get_url = get_el.attr("url")?.to_string();

    let put_headers = put_el
        .children()
        .filter(|c| c.name() == "header" && c.ns() == UPLOAD_NS)
        .filter_map(|c| {
            let name = c.attr("name")?.to_string();
            let value = c.text();
            Some((name, value))
        })
        .collect();

    Some(UploadSlot {
        put_url,
        get_url,
        put_headers,
    })
}

/// Parse an inbox result element (XEP-0430 / erlang-solutions inbox).
///
/// Accepts either a bare `<result xmlns='...inbox:0'>` element or a wrapping
/// stanza (e.g. `<message>`) that contains one as a child.  Returns `None` for
/// plain stanzas that carry no inbox result.
pub fn parse_inbox_result(element: &Element) -> Option<InboxEntry> {
    let (result_el, jid) = if element.name() == "result" && element.ns() == INBOX_NS {
        let jid = element
            .attr("from")
            .or_else(|| element.attr("to"))
            .map(str::to_string)
            .unwrap_or_default();
        (element, jid)
    } else {
        let result_el = element.get_child("result", INBOX_NS)?;
        let jid = element
            .attr("from")
            .or_else(|| element.attr("to"))
            .map(str::to_string)
            .unwrap_or_default();
        (result_el, jid)
    };

    let unread_count = result_el
        .get_child("unread", INBOX_NS)
        .and_then(|e| e.text().parse::<u32>().ok())
        .unwrap_or(0);

    let last_message_body = extract_forwarded_body(result_el);

    Some(InboxEntry {
        jid,
        unread_count,
        last_message_body,
        timestamp: None,
    })
}

fn extract_forwarded_body(result_el: &Element) -> Option<String> {
    const FORWARD_NS: &str = "urn:xmpp:forward:0";
    let forwarded = result_el.get_child("forwarded", FORWARD_NS)?;
    let message = forwarded.get_child("message", CLIENT_NS)?;
    let body = message.get_child("body", CLIENT_NS)?;
    Some(body.text())
}

// ── IQ builders ──────────────────────────────────────────────────────────────

fn build_disco_info_iq(to: &str, node: Option<&str>) -> Element {
    let id = format!("disco-info-{}", next_id());
    let mut query_builder = Element::builder("query", DISCO_INFO_NS);
    if let Some(n) = node {
        query_builder = query_builder.attr("node", n);
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", to)
        .attr("id", id)
        .append(query_builder.build())
        .build()
}

fn build_disco_items_iq(to: &str, node: Option<&str>) -> Element {
    let id = format!("disco-items-{}", next_id());
    let mut query_builder = Element::builder("query", DISCO_ITEMS_NS);
    if let Some(n) = node {
        query_builder = query_builder.attr("node", n);
    }
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", to)
        .attr("id", id)
        .append(query_builder.build())
        .build()
}

fn build_upload_slot_iq(
    service_jid: &str,
    filename: &str,
    size: u64,
    content_type: &str,
) -> Element {
    let id = format!("upload-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "get")
        .attr("to", service_jid)
        .attr("id", id)
        .append(
            Element::builder("request", UPLOAD_NS)
                .attr("filename", filename)
                .attr("size", size.to_string())
                .attr("content-type", content_type)
                .build(),
        )
        .build()
}

fn build_inbox_iq(bare_jid: &str, query_id: &str, max: u32) -> Element {
    let id = format!("inbox-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("to", bare_jid)
        .attr("id", id)
        .append(
            Element::builder("inbox", INBOX_NS)
                .attr("queryid", query_id)
                .append(
                    Element::builder("max", RSM_NS)
                        .append(max.to_string())
                        .build(),
                )
                .build(),
        )
        .build()
}

fn build_enable_push_iq(push_service_jid: &str, node: &str, token: &str) -> Element {
    let id = format!("push-enable-{}", next_id());
    let form = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "submit")
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "FORM_TYPE")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append("http://jabber.org/protocol/pubsub#publish-options")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "secret")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(token)
                        .build(),
                )
                .build(),
        )
        .build();
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("id", id)
        .append(
            Element::builder("enable", PUSH_NS)
                .attr("jid", push_service_jid)
                .attr("node", node)
                .append(form)
                .build(),
        )
        .build()
}

fn build_disable_push_iq(push_service_jid: &str, node: &str) -> Element {
    let id = format!("push-disable-{}", next_id());
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("id", id)
        .append(
            Element::builder("disable", PUSH_NS)
                .attr("jid", push_service_jid)
                .attr("node", node)
                .build(),
        )
        .build()
}

fn build_create_channel_iq(
    service_jid: &str,
    waddle_id: &str,
    name: &str,
    channel_type: &str,
) -> Element {
    let id = format!("create-channel-{}", next_id());
    let form = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "submit")
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "name")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(name)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "type")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(channel_type)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "waddle_id")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(waddle_id)
                        .build(),
                )
                .build(),
        )
        .build();
    Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("to", service_jid)
        .attr("id", id)
        .append(
            Element::builder("command", ADHOC_NS)
                .attr("node", "create-channel")
                .attr("action", "execute")
                .append(form)
                .build(),
        )
        .build()
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Parse a channel node attribute in the format `{type}_{position}_{id}`.
fn parse_channel_node(node: Option<&str>, jid: &str) -> (String, i32, String) {
    if let Some(n) = node {
        let parts: Vec<&str> = n.splitn(3, '_').collect();
        if parts.len() == 3 {
            let channel_type = parts[0].to_string();
            let position = parts[1].parse::<i32>().unwrap_or(0);
            let id = parts[2].to_string();
            return (channel_type, position, id);
        }
    }
    let id = jid.split('@').next().unwrap_or(jid).to_string();
    ("text".to_string(), 0, id)
}

/// Parse the result of a `create-channel` ad-hoc command IQ.
fn parse_create_channel_result(iq: &Element) -> Option<CreateChannelResult> {
    let command = iq.get_child("command", ADHOC_NS)?;
    let form = command.get_child("x", DATA_FORMS_NS)?;

    let room_jid = form
        .children()
        .filter(|c| c.name() == "field" && c.ns() == DATA_FORMS_NS)
        .find(|c| c.attr("var") == Some("room_jid"))
        .and_then(|c| c.get_child("value", DATA_FORMS_NS))
        .map(|v| v.text())?;

    let (waddle_id, channel_id) = parse_room_jid_parts(&room_jid);

    Some(CreateChannelResult {
        room_jid,
        waddle_id,
        channel_id,
    })
}

/// Extract `waddle_id` and `channel_id` from `{waddleID}_{channelID}@muc.domain`.
fn parse_room_jid_parts(room_jid: &str) -> (String, String) {
    let local = room_jid.split('@').next().unwrap_or(room_jid);
    let mut parts = local.splitn(2, '_');
    let waddle_id = parts.next().unwrap_or("").to_string();
    let channel_id = parts.next().unwrap_or("").to_string();
    (waddle_id, channel_id)
}

fn parse_error() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "bad-request".to_string(),
        text: Some("response could not be parsed".to_string()),
    })
}

// ── Extension trait ──────────────────────────────────────────────────────────

#[allow(async_fn_in_trait)]
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

    /// Discover top-level Waddle communities from the server.
    async fn discover_waddles(&self, server_domain: &str) -> ClientResult<Vec<DiscoveredWaddle>>;

    /// Discover channels within a waddle.
    async fn discover_channels(&self, waddle_jid: &str) -> ClientResult<Vec<DiscoveredChannel>>;

    /// Create a new channel via the Waddle ad-hoc command.
    async fn create_channel(
        &self,
        service_jid: &str,
        waddle_id: &str,
        name: &str,
        channel_type: &str,
    ) -> ClientResult<CreateChannelResult>;
}

// ── Implementation ────────────────────────────────────────────────────────────

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

    async fn discover_waddles(&self, server_domain: &str) -> ClientResult<Vec<DiscoveredWaddle>> {
        let items = self.discover_items(server_domain, None).await?;
        let waddles = items
            .into_iter()
            .map(|item| DiscoveredWaddle {
                id: item.node.unwrap_or_else(|| item.jid.clone()),
                name: item.name.unwrap_or_default(),
                is_public: true,
            })
            .collect();
        Ok(waddles)
    }

    async fn discover_channels(&self, waddle_jid: &str) -> ClientResult<Vec<DiscoveredChannel>> {
        let items = self.discover_items(waddle_jid, None).await?;
        let channels = items
            .into_iter()
            .map(|item| {
                let (channel_type, position, id) =
                    parse_channel_node(item.node.as_deref(), &item.jid);
                DiscoveredChannel {
                    id,
                    name: item.name.unwrap_or_default(),
                    channel_type,
                    position,
                }
            })
            .collect();
        Ok(channels)
    }

    async fn create_channel(
        &self,
        service_jid: &str,
        waddle_id: &str,
        name: &str,
        channel_type: &str,
    ) -> ClientResult<CreateChannelResult> {
        let iq = build_create_channel_iq(service_jid, waddle_id, name, channel_type);
        let result = self.send_iq(iq).await?;
        parse_create_channel_result(&result).ok_or_else(parse_error)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_disco_info_result_extracts_features() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .append(
                        Element::builder("feature", DISCO_INFO_NS)
                            .attr("var", UPLOAD_NS)
                            .build(),
                    )
                    .append(
                        Element::builder("feature", DISCO_INFO_NS)
                            .attr("var", "jabber:iq:version")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let result = parse_disco_info_result(&iq, "upload.example.com").unwrap();
        assert_eq!(result.jid, "upload.example.com");
        assert!(result.has_feature(UPLOAD_NS));
        assert!(result.has_feature("jabber:iq:version"));
        assert!(!result.has_feature("urn:xmpp:nonexistent"));
    }

    #[test]
    fn parse_disco_info_result_extracts_identities() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", DISCO_INFO_NS)
                    .append(
                        Element::builder("identity", DISCO_INFO_NS)
                            .attr("category", "store")
                            .attr("type", "file")
                            .attr("name", "HTTP File Upload")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let result = parse_disco_info_result(&iq, "upload.example.com").unwrap();
        assert_eq!(result.identities.len(), 1);
        let id = &result.identities[0];
        assert_eq!(id.category, "store");
        assert_eq!(id.identity_type, "file");
        assert_eq!(id.name.as_deref(), Some("HTTP File Upload"));
    }

    #[test]
    fn parse_disco_items_result_extracts_items() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("query", DISCO_ITEMS_NS)
                    .append(
                        Element::builder("item", DISCO_ITEMS_NS)
                            .attr("jid", "upload.example.com")
                            .attr("name", "Upload Service")
                            .build(),
                    )
                    .append(
                        Element::builder("item", DISCO_ITEMS_NS)
                            .attr("jid", "muc.example.com")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let items = parse_disco_items_result(&iq).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].jid, "upload.example.com");
        assert_eq!(items[0].name.as_deref(), Some("Upload Service"));
        assert_eq!(items[1].jid, "muc.example.com");
        assert!(items[1].name.is_none());
    }

    #[test]
    fn parse_upload_slot_extracts_urls_and_headers() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("slot", UPLOAD_NS)
                    .append(
                        Element::builder("put", UPLOAD_NS)
                            .attr("url", "https://example.com/upload/file.jpg")
                            .append(
                                Element::builder("header", UPLOAD_NS)
                                    .attr("name", "Authorization")
                                    .append("Bearer token123")
                                    .build(),
                            )
                            .append(
                                Element::builder("header", UPLOAD_NS)
                                    .attr("name", "Cookie")
                                    .append("session=abc")
                                    .build(),
                            )
                            .build(),
                    )
                    .append(
                        Element::builder("get", UPLOAD_NS)
                            .attr("url", "https://cdn.example.com/file.jpg")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let slot = parse_upload_slot(&iq).unwrap();
        assert_eq!(slot.put_url, "https://example.com/upload/file.jpg");
        assert_eq!(slot.get_url, "https://cdn.example.com/file.jpg");
        assert_eq!(slot.put_headers.len(), 2);
        assert_eq!(
            slot.put_headers[0],
            ("Authorization".to_string(), "Bearer token123".to_string())
        );
        assert_eq!(
            slot.put_headers[1],
            ("Cookie".to_string(), "session=abc".to_string())
        );
    }

    #[test]
    fn parse_upload_slot_no_headers_ok() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr("type", "result")
            .append(
                Element::builder("slot", UPLOAD_NS)
                    .append(
                        Element::builder("put", UPLOAD_NS)
                            .attr("url", "https://example.com/upload/file.jpg")
                            .build(),
                    )
                    .append(
                        Element::builder("get", UPLOAD_NS)
                            .attr("url", "https://cdn.example.com/file.jpg")
                            .build(),
                    )
                    .build(),
            )
            .build();

        let slot = parse_upload_slot(&iq).unwrap();
        assert_eq!(slot.put_url, "https://example.com/upload/file.jpg");
        assert_eq!(slot.get_url, "https://cdn.example.com/file.jpg");
        assert!(slot.put_headers.is_empty());
    }

    #[test]
    fn disco_info_result_has_feature_check() {
        let result = DiscoInfoResult {
            jid: "example.com".to_string(),
            node: None,
            identities: vec![],
            features: vec![UPLOAD_NS.to_string(), "jabber:iq:ping".to_string()],
        };
        assert!(result.has_feature(UPLOAD_NS));
        assert!(result.has_feature("jabber:iq:ping"));
        assert!(!result.has_feature("urn:xmpp:nonexistent"));
    }

    #[test]
    fn parse_inbox_result_returns_none_for_plain_message() {
        let message = Element::builder("message", CLIENT_NS)
            .attr("from", "alice@example.com")
            .attr("to", "me@example.com")
            .append(Element::builder("body", CLIENT_NS).append("Hello!").build())
            .build();

        assert!(parse_inbox_result(&message).is_none());
    }
}
