//! Messaging verb exports: reactions (XEP-0444), corrections
//! (XEP-0308), retractions (XEP-0424), moderation (XEP-0425), chat
//! states (XEP-0085), displayed markers (XEP-0333), Message Displayed
//! Synchronization (XEP-0490), and `urn:waddle:pin:0` pins.
//!
//! Every method parses its untyped FFI inputs (JID strings, stanza
//! ids) into typed values immediately, then delegates to the shared
//! `waddle-xmpp-client` builders / extension traits — the same code
//! paths the wasm chat client drives. Send-style methods keep the
//! crate's `bool` / [`WaddleSendMessageOutcome`] shapes; the two
//! fetch-style methods return `Result<_, WaddleError>`.

use jid::{BareJid, Jid};
use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::discovery::DiscoveryExt;
use waddle_xmpp_client::error::STANZA_CONDITION_ITEM_NOT_FOUND;
use waddle_xmpp_client::mds::{
    build_mds_catchup_iq, build_mds_publish_iq, build_mds_subscribe_iq, parse_mds_catchup_result,
    NS_PUBSUB_PUBLISH_OPTIONS_FEATURE,
};
use waddle_xmpp_client::messaging::{
    build_correction_message, build_moderation_message, build_pinned_chat_message,
    build_pinned_message, build_reaction_message, build_unpinned_chat_message,
    build_unpinned_message, MessagingExt, SendMessageOptions,
};
use waddle_xmpp_client::pin::{build_pin_list_iq, parse_pin_list_response};
use waddle_xmpp_client::request::StanzaId;
use waddle_xmpp_client::{ClientError, ClientResult};

use crate::boundary_convert::jid_domain;
use crate::convert::{mds_catchup_entry_to_ffi, pin_entry_to_ffi, send_options_from_ffi};
use crate::error::client_error_to_waddle;
use crate::send_outcome::send_failure_outcome;
use crate::{
    WaddleChatState, WaddleClient, WaddleError, WaddleMdsDisplayedEntry, WaddlePinEntry,
    WaddleSendMessageOutcome, WaddleSendOptions,
};

// ── FFI → wire mappings and stanza factories ────────────────────────────────
//
// Pure functions between the FFI argument shapes and the shared
// builders. The exported methods call these; the per-XEP test suite
// asserts their output against the XEP wire shapes, so the exact
// stanzas the FFI emits are pinned without a live connection.

/// RFC 6121 message `type` for the DM/MUC split every verb shares.
pub(crate) fn verb_message_type(is_muc: bool) -> &'static str {
    if is_muc {
        "groupchat"
    } else {
        "chat"
    }
}

/// XEP-0085 wire element name of a typed chat state. Inverse of the
/// inbound `chat_state_to_ffi` mapping in `convert.rs`.
pub(crate) fn chat_state_wire_name(state: WaddleChatState) -> &'static str {
    match state {
        WaddleChatState::Active => "active",
        WaddleChatState::Composing => "composing",
        WaddleChatState::Paused => "paused",
        WaddleChatState::Inactive => "inactive",
        WaddleChatState::Gone => "gone",
    }
}

/// XEP-0444 §3 reactions message targeting `target_stanza_id`.
pub(crate) fn reaction_stanza(
    to: &Jid,
    target_stanza_id: &str,
    emojis: &[String],
    is_muc: bool,
) -> Element {
    build_reaction_message(
        to.as_str(),
        verb_message_type(is_muc),
        target_stanza_id,
        emojis,
        None,
    )
}

/// XEP-0308 correction: a fresh message body carrying
/// `<replace id='…'/>` for the message being corrected.
pub(crate) fn correction_stanza(
    to: &Jid,
    new_body: &str,
    target_id: &str,
    is_muc: bool,
    options: &SendMessageOptions,
) -> ClientResult<(StanzaId, Element)> {
    build_correction_message(
        to.as_str(),
        verb_message_type(is_muc),
        new_body,
        target_id,
        options,
    )
}

/// XEP-0425 §4 moderation IQ addressed to the room. Moderation is
/// MUC-only by definition, so there is no DM variant.
pub(crate) fn moderation_iq(
    room: &BareJid,
    target_stanza_id: &str,
    reason: Option<&str>,
) -> Element {
    build_moderation_message(room.as_str(), "groupchat", target_stanza_id, reason)
}

/// XEP-0490 §3 publish IQ with the spec-mandated publish-options.
pub(crate) fn mds_publish_iq(chat: &Jid, stanza_id: &StanzaId, stanza_id_by: &BareJid) -> Element {
    build_mds_publish_iq(&Uuid::new_v4().to_string(), chat, stanza_id, stanza_id_by)
}

/// Bare account JID used as the XEP-0060 subscriber for the MDS node.
/// The stored config JID is bare by contract, but a resource-carrying
/// value must still subscribe as the bare account, so this parses the
/// general form and strips any resource.
pub(crate) fn account_bare_jid(config_jid: &str) -> Result<BareJid, jid::Error> {
    config_jid.parse::<Jid>().map(|jid| jid.to_bare())
}

/// `urn:waddle:pin:0` room pin request message.
pub(crate) fn pin_room_stanza(room: &BareJid, target_stanza_id: &str) -> Element {
    build_pinned_message(room.as_str(), target_stanza_id)
}

/// `urn:waddle:pin:0` room unpin request message.
pub(crate) fn unpin_room_stanza(room: &BareJid, target_stanza_id: &str) -> Element {
    build_unpinned_message(room.as_str(), target_stanza_id)
}

/// `urn:waddle:pin:0` 1:1 DM pin request message.
pub(crate) fn pin_direct_stanza(peer: &Jid, target_stanza_id: &str) -> Element {
    build_pinned_chat_message(peer.as_str(), target_stanza_id)
}

/// `urn:waddle:pin:0` 1:1 DM unpin request message.
pub(crate) fn unpin_direct_stanza(peer: &Jid, target_stanza_id: &str) -> Element {
    build_unpinned_chat_message(peer.as_str(), target_stanza_id)
}

impl WaddleClient {
    /// Parse the recipient of a DM/MUC verb: rooms must be bare JIDs,
    /// DM peers may be bare or full. Emits a diagnostic and returns
    /// `None` on parse failure, mirroring the other JID helpers.
    fn parse_verb_recipient(&self, value: &str, is_muc: bool, op: &'static str) -> Option<Jid> {
        if is_muc {
            self.parse_bare_jid(value, op).map(Jid::from)
        } else {
            self.parse_jid(value, op)
        }
    }
}

// ── Exported verbs ───────────────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0444: react to the message identified by
    /// `target_stanza_id` with the full desired emoji set. Per §3 the
    /// stanza carries the *complete* reaction state of this user for
    /// that message — send an empty `emojis` to clear all reactions.
    pub async fn send_reaction(
        &self,
        target_jid: String,
        target_stanza_id: String,
        emojis: Vec<String>,
        is_muc: bool,
    ) -> bool {
        let Some(to) = self.parse_verb_recipient(&target_jid, is_muc, "send_reaction") else {
            return false;
        };
        let stanza = reaction_stanza(&to, &target_stanza_id, &emojis, is_muc);
        self.send_stanza_or_error(stanza, "send_reaction").await
    }

    /// XEP-0308: replace the body of the message identified by
    /// `target_id` with `new_body`. Returns the outcome of the fresh
    /// send; on success `stanza_id` is the id of the *correction*
    /// message (the wasm client's `send_correction` return value).
    pub async fn send_correction(
        &self,
        peer_jid: String,
        target_id: String,
        new_body: String,
        is_muc: bool,
        options: Option<WaddleSendOptions>,
    ) -> WaddleSendMessageOutcome {
        let Some(to) = self.parse_verb_recipient(&peer_jid, is_muc, "send_correction") else {
            return WaddleSendMessageOutcome::InvalidRecipient;
        };
        let opts = match options.map(send_options_from_ffi).transpose() {
            Ok(o) => o.unwrap_or_default(),
            Err(e) => {
                self.emit_error(format!("send_correction failed: {e}"));
                return WaddleSendMessageOutcome::InvalidOptions;
            }
        };
        let (stanza_id, stanza) = match correction_stanza(&to, &new_body, &target_id, is_muc, &opts)
        {
            Ok(built) => built,
            Err(e) => {
                self.emit_error(format!("send_correction failed: {e}"));
                return WaddleSendMessageOutcome::Error;
            }
        };
        let Some(handle) = self.clone_handle().await else {
            return WaddleSendMessageOutcome::NotConnected;
        };
        match handle.send_stanza(stanza).await {
            Ok(()) => WaddleSendMessageOutcome::Sent {
                stanza_id: stanza_id.to_string(),
            },
            Err(e) => {
                self.emit_error(format!("send_correction failed: {e}"));
                send_failure_outcome(&e)
            }
        }
    }

    /// XEP-0424: retract the sender's own message identified by
    /// `target_stanza_id`. The stanza carries the §4.2 fallback body
    /// marked with a XEP-0428 `<fallback/>` so supporting receivers
    /// never render it.
    pub async fn send_retraction(
        &self,
        peer_jid: String,
        target_stanza_id: String,
        is_muc: bool,
    ) -> bool {
        let Some(to) = self.parse_verb_recipient(&peer_jid, is_muc, "send_retraction") else {
            return false;
        };
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle
            .retract_message(
                to.as_str(),
                &target_stanza_id,
                verb_message_type(is_muc),
                None,
            )
            .await
        {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("send_retraction failed: {e}"));
                false
            }
        }
    }

    /// XEP-0425: ask the room to moderate (retract) the message
    /// identified by `target_stanza_id`. Sent as an IQ to the room;
    /// the server gates on moderator privileges and answers
    /// `<forbidden/>` otherwise, which surfaces here as `false` plus
    /// an `Error` event.
    pub async fn send_moderation(
        &self,
        room_jid: String,
        target_stanza_id: String,
        reason: Option<String>,
    ) -> bool {
        let Some(room) = self.parse_bare_jid(&room_jid, "send_moderation") else {
            return false;
        };
        let iq = moderation_iq(&room, &target_stanza_id, reason.as_deref());
        self.send_iq_or_error(iq, "send_moderation").await
    }

    /// XEP-0085: send a standalone chat state notification
    /// (typing indicator) to a DM peer or room.
    pub async fn send_chat_state(
        &self,
        peer_jid: String,
        state: WaddleChatState,
        is_muc: bool,
    ) -> bool {
        let Some(to) = self.parse_verb_recipient(&peer_jid, is_muc, "send_chat_state") else {
            return false;
        };
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle
            .send_chat_state(
                to.as_str(),
                chat_state_wire_name(state),
                verb_message_type(is_muc),
                None,
            )
            .await
        {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("send_chat_state failed: {e}"));
                false
            }
        }
    }

    /// XEP-0333: send a `<displayed id='…'/>` marker acknowledging
    /// the message identified by `stanza_id` as read.
    pub async fn send_displayed(&self, peer_jid: String, stanza_id: String, is_muc: bool) -> bool {
        let Some(to) = self.parse_verb_recipient(&peer_jid, is_muc, "send_displayed") else {
            return false;
        };
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        match handle
            .send_displayed_marker(to.as_str(), &stanza_id, verb_message_type(is_muc), None)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("send_displayed failed: {e}"));
                false
            }
        }
    }

    /// XEP-0490 §3: publish the latest displayed marker for `chat_jid`
    /// to the user's own MDS PEP node. `chat_jid` (the PEP item id) is
    /// the chat's JID — bare DM contact, bare MUC room, or full MUC
    /// occupant for a private message. `stanza_id` is the XEP-0359 id
    /// of the displayed message and `stanza_id_by` its resource-less
    /// assigning authority. Carries the spec-mandated publish-options
    /// as preconditions.
    pub async fn publish_mds_displayed(
        &self,
        chat_jid: String,
        stanza_id: String,
        stanza_id_by: String,
    ) -> bool {
        let Some(chat) = self.parse_jid(&chat_jid, "publish_mds_displayed") else {
            return false;
        };
        let stanza_id = match StanzaId::new(stanza_id) {
            Ok(id) => id,
            Err(e) => {
                self.emit_error(format!(
                    "publish_mds_displayed failed: invalid stanza id: {e}"
                ));
                return false;
            }
        };
        let Some(by) = self.parse_bare_jid(&stanza_id_by, "publish_mds_displayed") else {
            return false;
        };
        let iq = mds_publish_iq(&chat, &stanza_id, &by);
        self.send_iq_or_error(iq, "publish_mds_displayed").await
    }

    /// XEP-0490 §3.1 catch-up: retrieve every item from the user's own
    /// `urn:xmpp:mds:displayed:0` PEP node. An `item-not-found` reply
    /// (no node published yet) is the normal first-run state and
    /// returns an empty list rather than an error.
    pub async fn fetch_mds_displayed(&self) -> Result<Vec<WaddleMdsDisplayedEntry>, WaddleError> {
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = build_mds_catchup_iq(&Uuid::new_v4().to_string());
        match handle.send_iq(iq).await {
            Ok(result) => Ok(parse_mds_catchup_result(&result)
                .into_iter()
                .map(mds_catchup_entry_to_ffi)
                .collect()),
            Err(ClientError::StanzaError(err))
                if err.condition == STANZA_CONDITION_ITEM_NOT_FOUND =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(client_error_to_waddle(&e)),
        }
    }

    /// XEP-0060 explicit subscribe to the user's own MDS node — the
    /// fallback path for receiving `+notify` events when this client's
    /// presence does not yet carry XEP-0115 caps. The subscriber JID
    /// is the bare account JID.
    pub async fn subscribe_mds_displayed(&self) -> bool {
        let bare = match account_bare_jid(&self.config.jid) {
            Ok(jid) => jid,
            Err(e) => {
                self.emit_error(format!(
                    "subscribe_mds_displayed failed: invalid account JID: {e}"
                ));
                return false;
            }
        };
        let iq = build_mds_subscribe_iq(&Uuid::new_v4().to_string(), bare.as_str());
        self.send_iq_or_error(iq, "subscribe_mds_displayed").await
    }

    /// XEP-0490 §3 precondition probe: whether the account server
    /// advertises `http://jabber.org/protocol/pubsub#publish-options`
    /// on disco#info. Publishing MDS markers without it risks a node
    /// with wrong access-model defaults; callers should check once per
    /// session before the first publish (wasm client parity).
    pub async fn supports_mds_publish_options(&self) -> bool {
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        let domain = jid_domain(&self.config.jid);
        match handle.discover_info(domain, None).await {
            Ok(info) => info.has_feature(NS_PUBSUB_PUBLISH_OPTIONS_FEATURE),
            Err(e) => {
                self.emit_error(format!("supports_mds_publish_options failed: {e}"));
                false
            }
        }
    }

    /// `urn:waddle:pin:0`: fetch the current pinned-messages list for
    /// a MUC room. Empty when the room has no pins. The server gates
    /// on room occupancy; a non-occupant caller receives a
    /// `forbidden` stanza error.
    pub async fn fetch_room_pins(
        &self,
        room_jid: String,
    ) -> Result<Vec<WaddlePinEntry>, WaddleError> {
        let room = room_jid
            .parse::<BareJid>()
            .map_err(|_| WaddleError::InvalidJid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = build_pin_list_iq(room.as_str());
        match handle.send_iq(iq).await {
            Ok(result) => Ok(parse_pin_list_response(&result)
                .into_iter()
                .map(pin_entry_to_ffi)
                .collect()),
            Err(e) => Err(client_error_to_waddle(&e)),
        }
    }

    /// `urn:waddle:pin:0`: pin the room message identified by the
    /// XEP-0359 `by=room` stanza-id. The server gates on Owner/Admin
    /// affiliation; a non-admin sender receives a `forbidden` error
    /// via the inbound message stream.
    pub async fn pin_message(&self, room_jid: String, target_stanza_id: String) -> bool {
        let Some(room) = self.parse_bare_jid(&room_jid, "pin_message") else {
            return false;
        };
        let stanza = pin_room_stanza(&room, &target_stanza_id);
        self.send_stanza_or_error(stanza, "pin_message").await
    }

    /// `urn:waddle:pin:0`: unpin a room message. Same authorization
    /// rules as [`Self::pin_message`].
    pub async fn unpin_message(&self, room_jid: String, target_stanza_id: String) -> bool {
        let Some(room) = self.parse_bare_jid(&room_jid, "unpin_message") else {
            return false;
        };
        let stanza = unpin_room_stanza(&room, &target_stanza_id);
        self.send_stanza_or_error(stanza, "unpin_message").await
    }

    /// `urn:waddle:pin:0`: pin a message in a 1:1 DM conversation.
    pub async fn pin_direct_message(&self, peer_jid: String, target_stanza_id: String) -> bool {
        let Some(peer) = self.parse_jid(&peer_jid, "pin_direct_message") else {
            return false;
        };
        let stanza = pin_direct_stanza(&peer, &target_stanza_id);
        self.send_stanza_or_error(stanza, "pin_direct_message")
            .await
    }

    /// `urn:waddle:pin:0`: unpin a message in a 1:1 DM conversation.
    pub async fn unpin_direct_message(&self, peer_jid: String, target_stanza_id: String) -> bool {
        let Some(peer) = self.parse_jid(&peer_jid, "unpin_direct_message") else {
            return false;
        };
        let stanza = unpin_direct_stanza(&peer, &target_stanza_id);
        self.send_stanza_or_error(stanza, "unpin_direct_message")
            .await
    }
}
