use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use futures::channel::{mpsc, oneshot};
use futures::{pin_mut, select, FutureExt, SinkExt, StreamExt};
use jid::{BareJid, FullJid, Jid};
use js_sys::{Function, Promise};
use minidom::Element;
use serde::{Deserialize, Serialize};
use waddle_xmpp_client::avatar::{request_avatar_with_iq, AvatarRequestFailure};
use waddle_xmpp_client::discovery::{
    self, build_disco_info_iq, build_disco_items_iq, build_muc_admin_affiliation_list_iq,
    build_muc_admin_affiliation_set_iq, build_roster_get_iq, build_upload_slot_iq,
    build_user_search_iq, build_waddle_inbox_mark_read_iq, parse_disco_info_result,
    parse_muc_admin_affiliation_query, parse_push_vapid_from_disco_info, parse_roster_result,
    parse_user_search_result, MucAdminAffiliationItem, PushVapidDiscoParseError, UserSearchQuery,
    WaddleInboxMarkRead,
};
use waddle_xmpp_client::error::parse_stanza_error;
use waddle_xmpp_client::mam::{
    self, build_dm_search_history_iq, build_mam_iq, build_room_search_history_iq, MamIqBuilder,
    MamResultCollector,
};
use waddle_xmpp_client::mds::{
    build_mds_catchup_iq, build_mds_publish_iq, build_mds_subscribe_iq, parse_mds_catchup_result,
    MdsCatchupEntry, NS_PUBSUB_PUBLISH_OPTIONS_FEATURE,
};
use waddle_xmpp_client::messaging::{
    self, build_chat_state_message, build_correction_message, build_displayed_message,
    build_in_call_reaction_message, build_moderation_message, build_outbound_message,
    build_pinned_message, build_reaction_message, build_retraction_message, build_unpinned_message,
    InCallReactionEmoji, InCallReactionRecipient, InCallSessionId, InCallSignal, InboundCallEvent,
    InboundMessage, InboundPresence, LinkPreviewToken, MarkupSpanData, MarkupSpanType,
    MucAffiliation, MucRole, ReferenceData, SendMessageOptions, SharedFileDisposition,
};
use waddle_xmpp_client::pep::{
    build_pep_items_iq, build_publish_activity_iq, build_publish_mood_iq, build_publish_tune_iq,
    build_retract_activity_iq, build_retract_mood_iq, build_retract_tune_iq, parse_pep_activity,
    parse_pep_mood, parse_pep_tune,
};
use waddle_xmpp_client::pin::{
    build_pin_list_iq, parse_pin_list_response, PinEntry, PinEvent, PinEventAction, PinPreview,
};
use waddle_xmpp_client::transport::{TransportEvent, TransportMessage, TransportState};
use waddle_xmpp_client::xep::{
    reply::{FallbackRange, ReplyMarker},
    thread::ThreadRef,
    threads::{
        build_fetch_threads_iq, parse_threads_response, FetchThreadsQuery, ThreadSort,
        ThreadStatusFilter,
    },
    xep0292::{build_fetch_vcard4_iq, build_publish_vcard4_iq, parse_pep_vcard4, VCard4},
    xep0402::{build_fetch_bookmarks_iq, parse_bookmarks_response, BookmarkItem},
    xep0492::{build_fetch_dm_bookmarks_iq, read_dm_bookmark_notify},
};
use waddle_xmpp_client::{
    AccessToken, ArchivedMessage, ClientConfig, ClientError, ClientEvent, ClientRequest,
    ClientResource, ConnectionConfig, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent,
    OAuthBearerConfig, SaslFailureCondition, StanzaId, StreamErrorCondition, StreamErrorDetail,
    StreamManagementEvent, WasmTransportEvent, WasmWebSocket, WebSocketConfig, XmppRuntime,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

mod client_account;
mod client_admin;
mod client_admin_v2;
mod client_calls;
mod client_core;
mod client_extensions;
mod client_history;
mod client_messaging;
mod client_rooms;
mod client_social_feed;
mod client_status_preference;
mod client_stories;
mod client_story_reads;
mod client_xcal;
mod commands;
mod conversions;
mod driver;
mod encrypted;
mod events;
mod extension_routes;
mod helpers;
mod options;
mod state;
mod types;

pub(crate) use commands::*;
pub(crate) use conversions::*;
pub(crate) use driver::*;
pub(crate) use encrypted::*;
pub(crate) use events::*;
pub(crate) use extension_routes::*;
pub(crate) use helpers::*;
pub(crate) use options::*;
pub(crate) use state::*;

pub use state::{WaddleClient, WaddleConfig};
pub use types::*;

const NS_CLIENT: &str = "jabber:client";
const NS_CHAT_STATES: &str = "http://jabber.org/protocol/chatstates";
const NS_CHAT_MARKERS: &str = "urn:xmpp:chat-markers:0";
const NS_REACTIONS: &str = "urn:xmpp:reactions:0";
const NS_RETRACT: &str = "urn:xmpp:message-retract:1";
const NS_REPLACE: &str = "urn:xmpp:message-correct:0";
const NS_MODERATE: &str = "urn:xmpp:message-moderate:1";
const NS_HINTS: &str = "urn:xmpp:hints";
const NS_ROSTER: &str = "jabber:iq:roster";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_VERSION: &str = "jabber:iq:version";
const NS_USER_SEARCH: &str = "jabber:iq:search";
const NS_ADHOC_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_WADDLE_EXTENSION_1: &str = "urn:waddle:extension:1";
const EXTENSION_ROUTE_FORM_TYPE: &str = "urn:waddle:extension:1:routes";
const EXTENSION_ROUTE_ITEM_LIMIT: u32 = 100;

// ── XEP-0392 Consistent Color Generation ─────────────────────────────
//
// The chat UI colors avatars/nicknames deterministically per-user. The
// spec algorithm (XEP-0392 §5.1) is: SHA-1(UTF-8 input) → first 2
// bytes as a big-endian u16 → hue = (value / 65536) * 360. Browsers
// expose SHA-1 only via async SubtleCrypto, which would force every
// chat-ui callsite onto an async render path; instead we expose the
// Rust SHA-1 impl synchronously through wasm so the existing sync
// `consistentColor(input)` callsite in chat-ui.ts can swap a single
// hash function and stay sync.

/// Compute the XEP-0392 consistent-color hue (0.0–360.0) for `input`.
///
/// Stateless free function — does not require a WaddleClient instance.
/// JS callers receive a Number.
#[wasm_bindgen]
pub fn xep0392_consistent_hue(input: &str) -> f64 {
    waddle_xmpp_core::xep0392::compute_hue(input)
}

/// Compute a CSS `hsl(...)` string for `input` using XEP-0392 with
/// custom saturation/lightness (percentages, 0.0–100.0). The CVD
/// correction is "none" — pass the hue back through `xep0392_consistent_hue`
/// and `apply_cvd_correction` in a future iteration if CVD modes
/// become a user preference.
#[wasm_bindgen]
pub fn xep0392_consistent_color(input: &str, saturation: f64, lightness: f64) -> String {
    waddle_xmpp_core::xep0392::generate_color_with_params(
        input,
        saturation,
        lightness,
        waddle_xmpp_core::xep0392::CvdCorrection::None,
    )
    .to_css()
}
