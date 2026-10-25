use crate::server::routes::websocket::ActiveCallThread;
use crate::server::routes::websocket::DmCallThreadKey;
use crate::server::routes::websocket::PendingDmCallOffer;
use crate::server::routes::websocket::WebSocketState;
#[path = "direct_archive_plan.rs"]
mod plan;
use super::effects::direct::{external, DurableDirectEffect, ExternalDirectEffect};
use super::{
    apply_retraction_tombstone, build_direct_archived_message, debug, effects, warn,
    ArchiveIdRewrite, BareJid, Deps, Element, Message,
};
use waddle_xmpp::ingress::{IngressEffectIntent, RetractionTombstoneMutation};
use waddle_xmpp::mam::StoreOutcome;
use waddle_xmpp_core::xep0359::{extract_stanza_id_by, StanzaId};

#[cfg(test)]
#[path = "direct_archive_correction_plan_tests.rs"]
mod correction_plan_tests;

mod implementation;
pub(super) use implementation::archive_direct;
use implementation::format_call_thread_duration;
use implementation::jmi_propose;
use implementation::jmi_sid;
pub(super) use implementation::prune_dm_call_thread_state;
pub(super) use implementation::resolve_direct_correction_target_message_id;
use implementation::DM_CALL_ACTIVE_TTL_SECS;
use implementation::DM_CALL_PENDING_TTL_SECS;
pub(super) use implementation::DM_CALL_STATE_MAX_KEYS;
