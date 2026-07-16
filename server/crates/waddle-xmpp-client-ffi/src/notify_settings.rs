//! XEP-0492 chat notification settings over the UniFFI boundary.
//!
//! Fetch verbs surface the user's XEP-0402 bookmark items (MUC
//! carrier) and Waddle DM-bookmark items (`urn:waddle:dm-bookmarks:0`,
//! issue #720) with their XEP-0492 fallback notification mode; set
//! verbs delegate to the shared read/modify/write flows in
//! `waddle_xmpp_client::notification_settings`, driving them with the
//! connected [`ClientHandle`] behind a timeout-bounding
//! [`CommandDriver`] adapter. The wire
//! semantics — §3-preserving merges, sparse DM retract, publish-option
//! preconditions — live entirely in the core crate; this module only
//! parses untyped FFI inputs into typed values and maps the typed core
//! outcomes onto the FFI shapes.

use jid::BareJid;
use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::error::STANZA_CONDITION_ITEM_NOT_FOUND;
use waddle_xmpp_client::notification_settings::{
    parse_dm_bookmark_payloads, set_dm_notification_mode, set_room_notification_mode,
    SetDmNotificationMode, SetDmNotificationModeOutcome, SetRoomNotificationMode,
    SetRoomNotificationModeOutcome,
};
use waddle_xmpp_client::push::CommandDriver;
use waddle_xmpp_client::xep::xep0402::{
    build_fetch_bookmarks_iq, parse_bookmarks_response, BookmarkItem,
};
use waddle_xmpp_client::xep::xep0492::{
    build_fetch_dm_bookmarks_iq, read_dm_bookmark_notify, surface_bookmark_notify,
    surface_dm_notify, NotifyMode,
};
use waddle_xmpp_client::{ClientError, ClientHandle, ClientResult};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError};

/// [`CommandDriver`] adapter that bounds every IQ round-trip of the
/// core read/modify/write flows with the crate's fetch-style timeout.
/// `ClientHandle::send_iq` has no request-level timeout, and the set
/// verbs issue two-to-three sequential IQs — a server that accepts
/// but never answers would otherwise suspend the caller forever
/// (same invariant as `messaging_verbs::send_iq_with_timeout`).
struct TimeoutCommandDriver {
    handle: ClientHandle,
}

impl CommandDriver for TimeoutCommandDriver {
    async fn send_iq(&self, iq: Element) -> ClientResult<Element> {
        send_iq_with_timeout(&self.handle, iq).await
    }
}

/// XEP-0492 §2.1 fallback notification setting (no `identity-*`
/// attrs). FFI mirror of [`NotifyMode`].
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleNotifyMode {
    /// `<always/>` — notify for every message.
    Always,
    /// `<on-mention/>` — only notify on a XEP-0461/0372 mention.
    OnMention,
    /// `<never/>` — never notify (muted).
    Never,
}

impl From<NotifyMode> for WaddleNotifyMode {
    fn from(mode: NotifyMode) -> Self {
        match mode {
            NotifyMode::Always => Self::Always,
            NotifyMode::OnMention => Self::OnMention,
            NotifyMode::Never => Self::Never,
        }
    }
}

impl From<WaddleNotifyMode> for NotifyMode {
    fn from(mode: WaddleNotifyMode) -> Self {
        match mode {
            WaddleNotifyMode::Always => Self::Always,
            WaddleNotifyMode::OnMention => Self::OnMention,
            WaddleNotifyMode::Never => Self::Never,
        }
    }
}

/// One XEP-0402 bookmark item surfaced with its XEP-0492 fallback
/// notification mode. `notify_mode == None` means the bookmark has no
/// fallback `<notify/>` setting — the caller resolves against the
/// XEP-0492 §3 conversation-kind default.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WaddleBookmarkItem {
    /// The bookmarked room's bare JID (XEP-0402 §2 item id).
    pub jid: String,
    /// `<conference name='…'>` attribute, when present.
    pub name: Option<String>,
    /// `<conference autojoin='true|false'>`; defaults to `false`.
    pub autojoin: bool,
    /// XEP-0492 fallback setting read from the bookmark's
    /// `<extensions/>`, when present.
    pub notify_mode: Option<WaddleNotifyMode>,
    /// Waddle rich XEP-0357 push-summary opt-in carried in the
    /// XEP-0492 §2.3 `<advanced/>` block (#719). `false` is the
    /// default / absence-of-marker case.
    pub rich_payload_opt_in: bool,
}

/// One Waddle DM-bookmark item (issue #720): the `urn:waddle:dm-bookmarks:0`
/// PEP node's item id is the contact's bare JID and its payload hosts
/// one official XEP-0492 `<notify/>`. The node is sparse /
/// override-only — an item exists ONLY when the DM has an override —
/// so `notify_mode == None` means the hosted `<notify/>` carries only
/// identity-scoped siblings; resolve against the §3 direct-chat
/// default (`always`).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WaddleDmBookmarkItem {
    /// The contact's bare JID (PEP item id).
    pub jid: String,
    /// XEP-0492 fallback setting read from the hosted `<notify/>`.
    pub notify_mode: Option<WaddleNotifyMode>,
    /// Waddle rich XEP-0357 push-summary opt-in (#719).
    pub rich_payload_opt_in: bool,
}

/// Typed outcome of [`WaddleClient::set_room_notification_mode`].
/// Mirrors the core `SetRoomNotificationModeOutcome` (and the WASM
/// boundary's tagged shape) so callers branch without parsing
/// diagnostics.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WaddleSetRoomNotificationModeOutcome {
    /// The merged bookmark was published. Carries the surfaced item so
    /// the caller reconciles its store without a follow-up fetch.
    Ok { item: WaddleBookmarkItem },
    /// XEP-0060 `precondition-not-met` on the publish — typically a
    /// pre-existing PEP node with a divergent `access_model`.
    NodeConfigMismatch,
    /// Any other stanza-level error. The specific RFC 6120 §8.3
    /// condition stays on the Rust side (emitted via `tracing::warn`)
    /// rather than crossing the boundary as a stringly-typed payload.
    Error,
}

/// Typed outcome of [`WaddleClient::set_dm_notification_mode`] (issue
/// #720). Mirrors [`WaddleSetRoomNotificationModeOutcome`] with one
/// extra variant: the DM node is sparse / override-only, so returning
/// a DM to the XEP-0492 §3 direct-chat default RETRACTS the item.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WaddleSetDmNotificationModeOutcome {
    /// The DM carried an override; the item was published.
    Ok { item: WaddleDmBookmarkItem },
    /// The merged `<notify/>` collapsed to the §3 direct-chat default,
    /// so the item was retracted. `jid` echoes the contact so the
    /// caller can drop its override entry.
    Removed { jid: String },
    /// XEP-0060 `precondition-not-met` on the publish.
    NodeConfigMismatch,
    /// Any other stanza-level error (condition stays on the Rust side).
    Error,
}

// ── Pure mapping helpers (pinned by the XEP-0492 FFI test suite) ────────────

/// Surface one parsed [`BookmarkItem`] into the FFI shape via the
/// shared core sibling-scanning rules.
pub(crate) fn bookmark_item_to_ffi(item: BookmarkItem) -> WaddleBookmarkItem {
    let surfaced = surface_bookmark_notify(&item.extensions);
    WaddleBookmarkItem {
        jid: item.jid.to_string(),
        name: item.name,
        autojoin: item.autojoin,
        notify_mode: surfaced.fallback_mode.map(WaddleNotifyMode::from),
        rich_payload_opt_in: surfaced.rich_payload_opt_in,
    }
}

/// Map a bookmarks-fetch IQ round-trip result onto the FFI item list.
/// An `item-not-found` reply (the user's PEP `urn:xmpp:bookmarks:1`
/// node is absent because no bookmark was ever published) is the
/// normal first-run state and yields the empty list, per the WASM
/// boundary's behavior; the caller resolves absent items against the
/// XEP-0492 §3 defaults.
pub(crate) fn bookmarks_from_fetch_result(
    result: ClientResult<Element>,
) -> Result<Vec<WaddleBookmarkItem>, WaddleError> {
    match result {
        Ok(response) => Ok(parse_bookmarks_response(&response)
            .into_iter()
            .map(bookmark_item_to_ffi)
            .collect()),
        Err(ClientError::StanzaError(err)) if err.condition == STANZA_CONDITION_ITEM_NOT_FOUND => {
            Ok(Vec::new())
        }
        Err(e) => Err(client_error_to_waddle(&e)),
    }
}

/// Parse a DM-bookmark items response into the FFI surface (issue
/// #720). A payload missing its `<notify/>` yields `notify_mode = None`
/// (the caller resolves it against the §3 direct-chat default).
pub(crate) fn parse_dm_bookmarks_response(iq: &Element) -> Vec<WaddleDmBookmarkItem> {
    parse_dm_bookmark_payloads(iq)
        .into_iter()
        .map(|(jid, payload)| {
            let surfaced = read_dm_bookmark_notify(&payload).map(surface_dm_notify);
            WaddleDmBookmarkItem {
                jid: jid.to_string(),
                notify_mode: surfaced
                    .and_then(|s| s.fallback_mode)
                    .map(WaddleNotifyMode::from),
                rich_payload_opt_in: surfaced.map(|s| s.rich_payload_opt_in).unwrap_or(false),
            }
        })
        .collect()
}

/// Map a DM-bookmarks-fetch IQ round-trip result onto the FFI item
/// list, treating an absent node (`item-not-found` — no DM has an
/// override yet) as the empty list.
pub(crate) fn dm_bookmarks_from_fetch_result(
    result: ClientResult<Element>,
) -> Result<Vec<WaddleDmBookmarkItem>, WaddleError> {
    match result {
        Ok(response) => Ok(parse_dm_bookmarks_response(&response)),
        Err(ClientError::StanzaError(err)) if err.condition == STANZA_CONDITION_ITEM_NOT_FOUND => {
            Ok(Vec::new())
        }
        Err(e) => Err(client_error_to_waddle(&e)),
    }
}

/// Map the core room outcome onto the FFI shape.
pub(crate) fn room_outcome_to_ffi(
    outcome: SetRoomNotificationModeOutcome,
) -> WaddleSetRoomNotificationModeOutcome {
    match outcome {
        SetRoomNotificationModeOutcome::Ok { item } => WaddleSetRoomNotificationModeOutcome::Ok {
            item: bookmark_item_to_ffi(item),
        },
        SetRoomNotificationModeOutcome::NodeConfigMismatch => {
            WaddleSetRoomNotificationModeOutcome::NodeConfigMismatch
        }
        SetRoomNotificationModeOutcome::Error => WaddleSetRoomNotificationModeOutcome::Error,
    }
}

/// Map the core DM outcome onto the FFI shape.
pub(crate) fn dm_outcome_to_ffi(
    outcome: SetDmNotificationModeOutcome,
) -> WaddleSetDmNotificationModeOutcome {
    match outcome {
        SetDmNotificationModeOutcome::Ok {
            jid,
            mode,
            rich_payload_opt_in,
        } => WaddleSetDmNotificationModeOutcome::Ok {
            item: WaddleDmBookmarkItem {
                jid: jid.to_string(),
                notify_mode: Some(mode.into()),
                rich_payload_opt_in,
            },
        },
        SetDmNotificationModeOutcome::Removed { jid } => {
            WaddleSetDmNotificationModeOutcome::Removed {
                jid: jid.to_string(),
            }
        }
        SetDmNotificationModeOutcome::NodeConfigMismatch => {
            WaddleSetDmNotificationModeOutcome::NodeConfigMismatch
        }
        SetDmNotificationModeOutcome::Error => WaddleSetDmNotificationModeOutcome::Error,
    }
}

/// Parse and validate a bookmark-carrier target JID: it must be a bare
/// JID **with a localpart**. XEP-0402 §2.2 bookmark ids are room bare
/// JIDs (`<localpart>@<muc-service>`) and the Waddle DM-bookmark item
/// id is the contact's bare JID — a domain-only JID is invalid for
/// both carriers and the PEP service would reject the publish, so it
/// is rejected early as a typed error (WASM-boundary parity).
pub(crate) fn parse_bookmark_target_jid(value: &str) -> Result<BareJid, WaddleError> {
    let jid: BareJid = value.parse().map_err(|_| WaddleError::InvalidJid)?;
    if jid.node().is_none() {
        return Err(WaddleError::InvalidJid);
    }
    Ok(jid)
}

/// Normalize the optional `<conference name='…'>` label: an empty or
/// whitespace-only name would publish `<conference name=""/>`, which
/// the parser round-trips as `None` — normalize here so the wire shape
/// is consistent (WASM-boundary parity).
pub(crate) fn normalize_bookmark_name(name: Option<String>) -> Option<String> {
    name.as_deref()
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_string)
}

// ── Exported verbs ───────────────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Fetch the user's XEP-0402 bookmark items from PEP, surfaced
    /// with the XEP-0492 fallback notification mode (when present) per
    /// room. Resolves to the empty list when the PEP node is absent
    /// (first publish hasn't happened — `item-not-found`).
    ///
    /// **Deferred** (WASM parity): no XEP-0163 §4.4 `+notify`
    /// self-subscription on `urn:xmpp:bookmarks:1` yet, so callers
    /// re-fetch on every fresh session-ready; a setting changed on
    /// another device reaches this one on the next reconnect.
    pub async fn fetch_user_bookmarks(&self) -> Result<Vec<WaddleBookmarkItem>, WaddleError> {
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = build_fetch_bookmarks_iq(&Uuid::new_v4().to_string());
        bookmarks_from_fetch_result(send_iq_with_timeout(&handle, iq).await)
    }

    /// Fetch the user's Waddle DM-bookmark items (issue #720) from
    /// PEP, surfaced with the XEP-0492 fallback mode + rich-payload
    /// opt-in (#719) per direct-chat contact. The node is sparse /
    /// override-only; an absent node (`item-not-found`) resolves to
    /// the empty list.
    pub async fn fetch_dm_bookmarks(&self) -> Result<Vec<WaddleDmBookmarkItem>, WaddleError> {
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = build_fetch_dm_bookmarks_iq(&Uuid::new_v4().to_string());
        dm_bookmarks_from_fetch_result(send_iq_with_timeout(&handle, iq).await)
    }

    /// Set the per-chat XEP-0492 notification mode for one room by
    /// merging into the user's XEP-0402 bookmark for that room
    /// (creating one with `autojoin=false` when absent, so the call
    /// never changes join behavior). Foreign `<advanced/>` children
    /// and identity-scoped siblings written by other clients are
    /// preserved verbatim (XEP-0492 §3); `rich_payload_opt_in`
    /// toggles the Waddle rich XEP-0357 push-summary opt-in (#719).
    pub async fn set_room_notification_mode(
        &self,
        room_jid: String,
        mode: WaddleNotifyMode,
        name: Option<String>,
        rich_payload_opt_in: bool,
    ) -> Result<WaddleSetRoomNotificationModeOutcome, WaddleError> {
        let room_jid = parse_bookmark_target_jid(&room_jid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let options = SetRoomNotificationMode {
            room_jid,
            name: normalize_bookmark_name(name),
            mode: mode.into(),
            rich_payload_opt_in,
        };
        let driver = TimeoutCommandDriver { handle };
        match set_room_notification_mode(&driver, options).await {
            Ok(outcome) => Ok(room_outcome_to_ffi(outcome)),
            Err(e) => Err(client_error_to_waddle(&e)),
        }
    }

    /// Set the per-DM XEP-0492 notification mode for one direct-chat
    /// contact via the Waddle DM-bookmark carrier (issue #720). The
    /// node is sparse / override-only: returning the DM to the §3
    /// direct-chat default retracts the item (`Removed` outcome)
    /// instead of publishing.
    pub async fn set_dm_notification_mode(
        &self,
        dm_jid: String,
        mode: WaddleNotifyMode,
        rich_payload_opt_in: bool,
    ) -> Result<WaddleSetDmNotificationModeOutcome, WaddleError> {
        let dm_jid = parse_bookmark_target_jid(&dm_jid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let options = SetDmNotificationMode {
            dm_jid,
            mode: mode.into(),
            rich_payload_opt_in,
        };
        let driver = TimeoutCommandDriver { handle };
        match set_dm_notification_mode(&driver, options).await {
            Ok(outcome) => Ok(dm_outcome_to_ffi(outcome)),
            Err(e) => Err(client_error_to_waddle(&e)),
        }
    }
}
