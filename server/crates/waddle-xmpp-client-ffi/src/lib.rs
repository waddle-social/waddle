//! UniFFI FFI bridge exposing the Waddle XMPP client to Swift.

uniffi::setup_scaffolding!("waddle_xmpp_client");

use std::sync::Arc;

use jid::{BareJid, FullJid};
use minidom::Element;
use tokio::sync::Mutex;
use url::Url;

mod boundary_convert;
mod calls;
mod community_admin;
mod consistent_color;
mod convert;
mod error;
mod extension_commands;
mod group_dm;
mod inbox_verbs;
mod jid_parts;
mod messaging;
mod messaging_verbs;
mod muji;
mod notify_settings;
mod profile_verbs;
mod push;
mod room_admin;
mod send_outcome;
mod stanza;
mod sticker_verbs;
mod types;

#[cfg(test)]
mod calls_tests;
#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod community_admin_tests;
#[cfg(test)]
mod extension_commands_tests;
#[cfg(test)]
mod group_dm_tests;
#[cfg(test)]
mod inbox_tests;
#[cfg(test)]
mod messaging_verbs_tests;
#[cfg(test)]
mod muji_tests;
#[cfg(test)]
mod notify_settings_tests;
#[cfg(test)]
mod profile_verbs_tests;
#[cfg(test)]
mod room_admin_tests;
#[cfg(test)]
mod sticker_verbs_tests;

pub use community_admin::{
    WaddleAdminChannelAffiliationEntry, WaddleAdminChannelListEntry,
    WaddleAdminChannelOccupantEntry, WaddleAdminChannelRef, WaddleAdminChannelsAffiliationsArgs,
    WaddleAdminChannelsAffiliationsPage, WaddleAdminChannelsCreateArgs,
    WaddleAdminChannelsKickResult, WaddleAdminChannelsListArgs, WaddleAdminChannelsListPage,
    WaddleAdminChannelsOccupantsArgs, WaddleAdminChannelsOccupantsPage,
    WaddleAdminChannelsSetAffiliationResult, WaddleAdminChannelsUpdateArgs, WaddleAdminSpaceEntry,
    WaddleAdminSpaceMemberEntry, WaddleAdminSpaceRef, WaddleAdminSpacesCreateArgs,
    WaddleAdminSpacesListArgs, WaddleAdminSpacesListPage, WaddleAdminSpacesMembersArgs,
    WaddleAdminSpacesMembersPage, WaddleAdminSpacesSetRoleResult, WaddleAdminSpacesUpdateArgs,
    WaddleAdminUserEntry, WaddleAdminUsersPage, WaddleSpaceRole,
};
pub use error::WaddleError;
pub use notify_settings::{
    WaddleBookmarkItem, WaddleDmBookmarkItem, WaddleNotifyMode, WaddleSetDmNotificationModeOutcome,
    WaddleSetRoomNotificationModeOutcome,
};
pub use profile_verbs::{WaddleActivity, WaddleMood, WaddlePepProfile, WaddleTune, WaddleVCard4};
pub use room_admin::{
    WaddlePinPermission, WaddleRoomConfig, WaddleRoomConfigPatch, WaddleRoomMemberEntry,
    WaddleUserSearchEntry,
};
pub use types::*;

use convert::dispatch_event;
use waddle_xmpp_client::{
    messaging::SessionId, AccessToken, ClientConfig, ClientHandle, ConnectionConfig,
    OAuthBearerConfig, WebSocketConfig,
};
use waddle_xmpp_client::{ClientResource, XmppClient};

// ── Main client object ───────────────────────────────────────────────────────

#[derive(uniffi::Object)]
pub struct WaddleClient {
    config: WaddleConfig,
    // Stored as Arc<Box<...>> so the constructor can take a Box (UniFFI callback_interface
    // generates FfiConverter for Box<dyn Trait>, not Arc<dyn Trait>).
    listener: Arc<Box<dyn WaddleEventListener>>,
    handle: Mutex<Option<ClientHandle>>,
    /// Serializes XEP-0430 inbox queries: `messages='false'`
    /// responses carry no MAM `queryid`, so entry correlation relies
    /// on a single in-flight query — the invariant the wasm driver
    /// enforces by deferring overlapping `SendInboxQuery` commands.
    inbox_query_gate: Mutex<()>,
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    #[uniffi::constructor]
    pub fn new(config: WaddleConfig, listener: Box<dyn WaddleEventListener>) -> Arc<Self> {
        Arc::new(Self {
            config,
            listener: Arc::new(listener),
            handle: Mutex::new(None),
            inbox_query_gate: Mutex::new(()),
        })
    }

    pub async fn connect(&self) {
        let jid: BareJid = match self.config.jid.parse() {
            Ok(j) => j,
            Err(e) => {
                self.emit_error(format!("Invalid JID: {e}"));
                return;
            }
        };

        let domain: BareJid = match jid.domain().to_string().parse() {
            Ok(d) => d,
            Err(e) => {
                self.emit_error(format!("Invalid domain: {e}"));
                return;
            }
        };

        let url: Url = match self.config.server_url.parse() {
            Ok(u) => u,
            Err(e) => {
                self.emit_error(format!("Invalid server URL: {e}"));
                return;
            }
        };

        let transport = match WebSocketConfig::new(url) {
            Ok(t) => t,
            Err(e) => {
                self.emit_error(format!("Invalid WebSocket config: {e}"));
                return;
            }
        };

        let resource = match ClientResource::new(&self.config.resource) {
            Ok(r) => r,
            Err(e) => {
                self.emit_error(format!("Invalid resource: {e}"));
                return;
            }
        };

        let auth =
            OAuthBearerConfig::new(jid, resource, AccessToken::new(&self.config.access_token));
        let auth = match auth {
            Ok(a) => a,
            Err(e) => {
                self.emit_error(format!("Invalid auth config: {e}"));
                return;
            }
        };

        let client_config = match ClientConfig::new(ConnectionConfig::new(domain), transport, auth)
        {
            Ok(c) => c,
            Err(e) => {
                self.emit_error(format!("Invalid client config: {e}"));
                return;
            }
        };

        let xmpp_client = match XmppClient::new(client_config) {
            Ok(c) => c,
            Err(e) => {
                self.emit_error(format!("Failed to create XMPP client: {e}"));
                return;
            }
        };

        let driver = match xmpp_client.driver() {
            Ok(d) => d,
            Err(e) => {
                self.emit_error(format!("Failed to create driver: {e}"));
                return;
            }
        };

        let client_handle = match driver.connect().await {
            Ok(h) => h,
            Err(e) => {
                self.emit_error(format!("Failed to connect: {e}"));
                return;
            }
        };

        let mut events = client_handle.events();
        let account_bare_jid = self.config.jid.split('/').next().unwrap_or("").to_string();
        let listener = Arc::clone(&self.listener);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => dispatch_event(event, &account_bare_jid, &**listener),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        listener.on_event(WaddleClientEvent::Disconnected);
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });

        *self.handle.lock().await = Some(client_handle);
    }

    pub async fn disconnect(&self) {
        let mut guard = self.handle.lock().await;
        if let Some(h) = guard.take() {
            let _ = h.disconnect().await;
        }
    }
}

// ── Send helpers ────────────────────────────────────────────────────────────

impl WaddleClient {
    /// Emit a human-readable diagnostic through the single-event
    /// listener. Every internal failure path funnels through here so
    /// diagnostics stay out of the typed payload variants.
    pub(crate) fn emit_error(&self, description: String) {
        self.listener
            .on_event(WaddleClientEvent::Error { description });
    }

    /// Send a built stanza fire-and-forget. Returns `true` on success.
    /// On failure (not connected, transport closed) emits an
    /// `Error` event and returns `false` so callers can short-circuit.
    ///
    /// Holds the client mutex only long enough to clone the
    /// [`ClientHandle`] (cheap — it is `Arc`-backed) and releases
    /// before awaiting the underlying send. Holding across the
    /// `send_stanza` await would be safe today (it returns once the
    /// stanza hits the command channel) but matches the same shape
    /// used by `send_iq_or_error`, which *must* release because
    /// `send_iq` awaits the correlated server reply.
    async fn send_stanza_or_error(&self, stanza: Element, op: &'static str) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle.send_stanza(stanza).await {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("{op} failed: {e}"));
                false
            }
        }
    }

    /// Send a built IQ and await its correlated result. Returns
    /// `true` on `<iq type='result'>`, `false` on `<iq type='error'>`
    /// or transport failure (with an `Error` event for both).
    ///
    /// Clones the [`ClientHandle`] under the mutex and **drops the
    /// guard before awaiting** the IQ response. `send_iq` blocks on
    /// a `oneshot::Receiver` until the server replies — holding the
    /// mutex across that wait would serialise every concurrent
    /// FFI call against the slowest in-flight IQ.
    async fn send_iq_or_error(&self, stanza: Element, op: &'static str) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        // Bounded like the fetch-style methods: `send_iq` itself never
        // times out, so a server that accepts but never answers would
        // otherwise suspend the caller's `await` forever.
        match crate::messaging_verbs::send_iq_with_timeout(&handle, stanza).await {
            Ok(_) => true,
            Err(e) => {
                self.emit_error(format!("{op} failed: {e}"));
                false
            }
        }
    }

    /// Take a cheap [`ClientHandle`] clone under the mutex, dropping
    /// the guard before returning so callers can await freely.
    async fn clone_handle(&self) -> Option<ClientHandle> {
        let guard = self.handle.lock().await;
        match guard.as_ref() {
            None => {
                drop(guard);
                self.emit_error("Not connected".to_string());
                None
            }
            Some(h) => {
                let handle = h.clone();
                drop(guard);
                Some(handle)
            }
        }
    }

    fn parse_full_jid(&self, value: &str, op: &'static str) -> Option<FullJid> {
        match value.parse::<FullJid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.emit_error(format!("{op} failed: invalid full JID '{value}': {e}"));
                None
            }
        }
    }

    fn parse_bare_jid(&self, value: &str, op: &'static str) -> Option<BareJid> {
        match value.parse::<BareJid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.emit_error(format!("{op} failed: invalid bare JID '{value}': {e}"));
                None
            }
        }
    }

    fn parse_jid(&self, value: &str, op: &'static str) -> Option<jid::Jid> {
        match value.parse::<jid::Jid>() {
            Ok(j) => Some(j),
            Err(e) => {
                self.emit_error(format!("{op} failed: invalid JID '{value}': {e}"));
                None
            }
        }
    }

    /// Wrap a caller-supplied id in [`SessionId`] after rejecting
    /// empty or whitespace-only inputs. XEP-0166 §7 uses `sid` as
    /// the correlation key for every later stanza in the call; a
    /// whitespace-only string passes the empty check but never
    /// matches anything on the wire, leaving the call UI in a
    /// stuck state. `SessionId(String)` is a thin newtype with no
    /// runtime check, so this method is the gate.
    fn parse_session_id(&self, value: String, op: &'static str) -> Option<SessionId> {
        if value.trim().is_empty() {
            self.emit_error(format!(
                "{op} failed: session id must be a non-empty, non-whitespace string"
            ));
            return None;
        }
        Some(SessionId(value))
    }
}
