use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use futures::channel::{mpsc, oneshot};
use futures::{pin_mut, select, FutureExt, SinkExt, StreamExt};
use jid::{BareJid, Jid};
use js_sys::{Function, Promise};
use minidom::Element;
use serde::{Deserialize, Serialize};
use waddle_xmpp_client::avatar::{request_avatar_with_iq, AvatarRequestFailure};
use waddle_xmpp_client::discovery::{
    self, build_disable_push_iq, build_disco_info_iq, build_disco_items_iq, build_enable_push_iq,
    build_muc_admin_affiliation_list_iq, build_muc_admin_affiliation_set_iq, build_roster_get_iq,
    build_upload_slot_iq, build_user_search_iq, build_waddle_inbox_mark_read_iq,
    build_waddle_inbox_query_iq, parse_muc_admin_affiliation_query, parse_roster_result,
    parse_user_search_result, parse_waddle_inbox_result, MucAdminAffiliationItem, UserSearchQuery,
    WaddleInboxMarkRead, WaddleInboxQuery,
};
use waddle_xmpp_client::error::parse_stanza_error;
use waddle_xmpp_client::mam::{self, build_mam_iq, build_mam_iq_extended};
use waddle_xmpp_client::messaging::{
    self, build_chat_state_message, build_correction_message, build_displayed_message,
    build_moderation_message, build_outbound_message, build_pinned_message, build_reaction_message,
    build_retraction_message, build_unpinned_message, InboundMessage, InboundPresence,
    MarkupSpanData, MarkupSpanType, MucAffiliation, MucRole, ReferenceData, SendMessageOptions,
    SharedFileDisposition,
};
use waddle_xmpp_client::pep::{
    build_pep_items_iq, build_publish_activity_iq, build_publish_mood_iq, build_publish_tune_iq,
    build_retract_activity_iq, build_retract_mood_iq, build_retract_tune_iq, parse_pep_activity,
    parse_pep_mood, parse_pep_tune,
};
use waddle_xmpp_client::pin::{
    build_pin_list_iq, parse_pin_list_response, PinEntry, PinEvent, PinEventAction, PinPreview,
};
use waddle_xmpp_client::transport::{
    StreamClose, TransportEvent, TransportMessage, TransportState,
};
use waddle_xmpp_client::xep::{
    reply::{FallbackRange, ReplyMarker},
    thread::ThreadRef,
};
use waddle_xmpp_client::{
    AccessToken, ArchivedMessage, ClientConfig, ClientError, ClientEvent, ClientRequest,
    ClientResource, ConnectionConfig, ConnectionEvent, LifecycleEvent, MessageDeliveryEvent,
    OAuthBearerConfig, StanzaId, StreamManagementEvent, WasmTransportEvent, WasmWebSocket,
    WebSocketConfig, XmppRuntime,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

mod client_account;
mod client_core;
mod client_extensions;
mod client_history;
mod client_messaging;
mod client_rooms;
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
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_ADHOC_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_WADDLE_EXTENSION_1: &str = "urn:waddle:extension:1";
const EXTENSION_ROUTE_FORM_TYPE: &str = "urn:waddle:extension:1:routes";
const EXTENSION_ROUTE_ITEM_LIMIT: u32 = 100;
