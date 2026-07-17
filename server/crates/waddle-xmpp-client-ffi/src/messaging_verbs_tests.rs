//! Per-XEP FFI test suites for the messaging verbs.
//!
//! Each module pins one XEP's wire shape as produced by the exact
//! FFI construction path (`verb_message_type` / `chat_state_wire_name`
//! mapping + shared builder) plus the exported methods' typed failure
//! behavior (not-connected, invalid JID) through a recording listener,
//! mirroring the wasm crate's fixture style.

use std::sync::{Arc, Mutex as StdMutex};

use jid::{BareJid, Jid};
use minidom::Element;

use crate::messaging_verbs::{
    account_bare_jid, chat_state_wire_name, correction_stanza, mds_publish_iq, moderation_iq,
    pin_direct_stanza, pin_room_stanza, reaction_stanza, unpin_direct_stanza, unpin_room_stanza,
    verb_message_type,
};
use crate::{
    WaddleChatState, WaddleClient, WaddleConfig, WaddleConnectionGeneration,
    WaddleDeliveryAttemptId, WaddleDeliveryAttemptRef, WaddleError, WaddleSendMessageOutcome,
};

// XEP-0334 hint namespace; the client crate keeps its constant
// pub(crate), so the expected wire value is pinned here literally.
const NS_HINTS: &str = "urn:xmpp:hints";
const NS_ORIGIN_ID: &str = "urn:xmpp:sid:0";

#[derive(Clone, Default)]
struct RecordingListener {
    errors: Arc<StdMutex<Vec<String>>>,
}

impl RecordingListener {
    fn errors(&self) -> Vec<String> {
        self.errors.lock().expect("test mutex poisoned").clone()
    }
}

fn test_client(listener: RecordingListener) -> Arc<WaddleClient> {
    WaddleClient::new_for_test(
        WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            delivery_attempt: WaddleDeliveryAttemptRef {
                attempt_id: WaddleDeliveryAttemptId {
                    value: "00000000-0000-4000-8000-000000000001".to_string(),
                },
                connection_generation: WaddleConnectionGeneration { value: 0 },
            },
            resume_state: None,
        },
        listener.errors,
    )
}

fn jid(value: &str) -> Jid {
    value.parse().expect("test JID parses")
}

fn bare(value: &str) -> BareJid {
    value.parse().expect("test bare JID parses")
}

fn stanza_id(value: &str) -> waddle_xmpp_client::request::StanzaId {
    waddle_xmpp_client::request::StanzaId::new(value).expect("test stanza id")
}

/// The origin-id every chat/groupchat verb stanza must carry
/// (XEP-0359 §5, stamped by the shared builders).
fn assert_origin_id(stanza: &Element) {
    let origin = stanza
        .get_child("origin-id", NS_ORIGIN_ID)
        .expect("origin-id present");
    assert_eq!(origin.attr("id"), stanza.attr("id"));
}

// ── XEP-0444: Message Reactions ──────────────────────────────────────────────

mod xep0444 {
    use super::*;
    use waddle_xmpp_client::NS_REACTIONS;

    #[test]
    fn reaction_stanza_carries_full_emoji_set_for_dm() {
        let stanza = reaction_stanza(
            &jid("bob@waddle.test"),
            "target-1",
            &["👍".to_string(), "❤️".to_string()],
            false,
        );
        assert_eq!(stanza.name(), "message");
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
        assert_eq!(stanza.attr("type"), Some("chat"));
        let reactions = stanza
            .get_child("reactions", NS_REACTIONS)
            .expect("reactions payload");
        assert_eq!(reactions.attr("id"), Some("target-1"));
        let emojis: Vec<String> = reactions
            .children()
            .filter(|child| child.is("reaction", NS_REACTIONS))
            .map(|child| child.text())
            .collect();
        assert_eq!(emojis, vec!["👍".to_string(), "❤️".to_string()]);
        // XEP-0444 §4: reactions SHOULD be stored in the archive.
        assert!(stanza.get_child("store", NS_HINTS).is_some());
        assert_origin_id(&stanza);
    }

    #[test]
    fn reaction_stanza_uses_groupchat_type_for_muc() {
        let stanza = reaction_stanza(
            &jid("room@muc.waddle.test"),
            "target-2",
            &["🎉".to_string()],
            true,
        );
        assert_eq!(stanza.attr("type"), Some("groupchat"));
        assert_eq!(stanza.attr("to"), Some("room@muc.waddle.test"));
    }

    #[test]
    fn empty_emoji_set_clears_reactions() {
        // XEP-0444 §3.2: an empty <reactions/> retracts all of the
        // sender's reactions on the target message.
        let stanza = reaction_stanza(&jid("bob@waddle.test"), "target-3", &[], false);
        let reactions = stanza
            .get_child("reactions", NS_REACTIONS)
            .expect("reactions payload");
        assert_eq!(reactions.children().count(), 0);
    }

    #[tokio::test]
    async fn send_reaction_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_reaction(
                "bob@waddle.test".to_string(),
                "target-1".to_string(),
                vec!["👍".to_string()],
                false,
            )
            .await;
        assert!(!sent);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }

    #[tokio::test]
    async fn send_reaction_rejects_invalid_room_jid() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_reaction(
                "not a jid".to_string(),
                "target-1".to_string(),
                vec!["👍".to_string()],
                true,
            )
            .await;
        assert!(!sent);
        assert!(
            listener
                .errors()
                .first()
                .is_some_and(|error| error.contains("invalid bare JID")),
            "expected invalid JID diagnostic"
        );
    }
}

// ── XEP-0308: Last Message Correction ────────────────────────────────────────

mod xep0308 {
    use super::*;
    use waddle_xmpp_client::messaging::SendMessageOptions;
    use waddle_xmpp_client::NS_MESSAGE_CORRECT;

    #[test]
    fn correction_stanza_carries_replace_and_new_body() {
        let (stanza_id, stanza) = correction_stanza(
            &jid("bob@waddle.test"),
            "fixed typo",
            "orig-1",
            false,
            &SendMessageOptions::default(),
        )
        .expect("correction builds");
        assert_eq!(stanza.attr("type"), Some("chat"));
        assert_eq!(stanza.attr("id"), Some(stanza_id.as_str()));
        let replace = stanza
            .get_child("replace", NS_MESSAGE_CORRECT)
            .expect("replace payload");
        assert_eq!(replace.attr("id"), Some("orig-1"));
        let body = stanza
            .get_child("body", "jabber:client")
            .expect("body present");
        assert_eq!(body.text(), "fixed typo");
        assert_origin_id(&stanza);
    }

    #[test]
    fn correction_stanza_uses_groupchat_type_for_muc() {
        let (_, stanza) = correction_stanza(
            &jid("room@muc.waddle.test"),
            "fixed",
            "orig-2",
            true,
            &SendMessageOptions::default(),
        )
        .expect("correction builds");
        assert_eq!(stanza.attr("type"), Some("groupchat"));
        assert_eq!(stanza.attr("to"), Some("room@muc.waddle.test"));
    }

    #[tokio::test]
    async fn send_correction_reports_not_connected_outcome() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let outcome = client
            .send_correction(
                "bob@waddle.test".to_string(),
                "orig-1".to_string(),
                "fixed".to_string(),
                false,
                None,
            )
            .await;
        assert_eq!(outcome, WaddleSendMessageOutcome::NotConnected);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }

    #[tokio::test]
    async fn send_correction_reports_invalid_recipient_outcome() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let outcome = client
            .send_correction(
                "not a jid".to_string(),
                "orig-1".to_string(),
                "fixed".to_string(),
                true,
                None,
            )
            .await;
        assert_eq!(outcome, WaddleSendMessageOutcome::InvalidRecipient);
    }
}

// ── XEP-0424: Message Retraction ─────────────────────────────────────────────

mod xep0424 {
    use super::*;
    use waddle_xmpp_client::messaging::build_retraction_message;
    use waddle_xmpp_client::xep::fallback::NS_FALLBACK;
    use waddle_xmpp_client::NS_MESSAGE_RETRACT;

    /// The exported `send_retraction` delegates to
    /// `MessagingExt::retract_message`, which builds through this exact
    /// builder + `verb_message_type` mapping.
    #[test]
    fn retraction_stanza_carries_retract_and_marked_fallback() {
        let stanza = build_retraction_message(
            jid("room@muc.waddle.test").as_str(),
            verb_message_type(true),
            "target-sid",
            None,
        );
        assert_eq!(stanza.attr("type"), Some("groupchat"));
        let retract = stanza
            .get_child("retract", NS_MESSAGE_RETRACT)
            .expect("retract payload");
        assert_eq!(retract.attr("id"), Some("target-sid"));
        // XEP-0424 §4.2: fallback body for non-supporting clients,
        // marked with XEP-0428 so supporting receivers drop it.
        assert!(stanza.get_child("body", "jabber:client").is_some());
        let fallback = stanza
            .get_child("fallback", NS_FALLBACK)
            .expect("fallback marker");
        assert_eq!(fallback.attr("for"), Some(NS_MESSAGE_RETRACT));
        assert!(stanza.get_child("store", NS_HINTS).is_some());
        assert_origin_id(&stanza);
    }

    #[test]
    fn retraction_stanza_uses_chat_type_for_dm() {
        let stanza = build_retraction_message(
            jid("bob@waddle.test").as_str(),
            verb_message_type(false),
            "target-sid",
            None,
        );
        assert_eq!(stanza.attr("type"), Some("chat"));
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
    }

    #[tokio::test]
    async fn send_retraction_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_retraction("bob@waddle.test".to_string(), "sid-1".to_string(), false)
            .await;
        assert!(!sent);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }
}

// ── XEP-0425: Moderated Message Retraction ───────────────────────────────────

mod xep0425 {
    use super::*;
    use waddle_xmpp_client::{NS_MESSAGE_MODERATE, NS_MESSAGE_RETRACT};

    #[test]
    fn moderation_iq_wraps_retract_with_reason() {
        let iq = moderation_iq(&bare("room@muc.waddle.test"), "target-sid", Some("spam"));
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), Some("room@muc.waddle.test"));
        assert!(iq.attr("id").is_some());
        let moderate = iq
            .get_child("moderate", NS_MESSAGE_MODERATE)
            .expect("moderate payload");
        assert_eq!(moderate.attr("id"), Some("target-sid"));
        assert!(moderate.get_child("retract", NS_MESSAGE_RETRACT).is_some());
        let reason = moderate
            .get_child("reason", NS_MESSAGE_MODERATE)
            .expect("reason present");
        assert_eq!(reason.text(), "spam");
    }

    #[test]
    fn moderation_iq_omits_reason_when_absent() {
        let iq = moderation_iq(&bare("room@muc.waddle.test"), "target-sid", None);
        let moderate = iq
            .get_child("moderate", NS_MESSAGE_MODERATE)
            .expect("moderate payload");
        assert!(moderate.get_child("reason", NS_MESSAGE_MODERATE).is_none());
    }

    #[tokio::test]
    async fn send_moderation_rejects_invalid_room_jid() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_moderation("not a room".to_string(), "sid-1".to_string(), None)
            .await;
        assert!(!sent);
        assert!(
            listener
                .errors()
                .first()
                .is_some_and(|error| error.contains("invalid bare JID")),
            "expected invalid JID diagnostic"
        );
    }

    #[tokio::test]
    async fn send_moderation_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_moderation(
                "room@muc.waddle.test".to_string(),
                "sid-1".to_string(),
                Some("spam".to_string()),
            )
            .await;
        assert!(!sent);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }
}

// ── XEP-0085: Chat State Notifications ───────────────────────────────────────

mod xep0085 {
    use super::*;
    use waddle_xmpp_client::messaging::build_chat_state_message;
    use waddle_xmpp_client::NS_CHAT_STATES;

    /// Every typed state maps to a wire name the shared builder's
    /// validator accepts and emits as the XEP-0085 child element.
    #[test]
    fn every_chat_state_maps_to_a_valid_wire_element() {
        let cases = [
            (WaddleChatState::Active, "active"),
            (WaddleChatState::Composing, "composing"),
            (WaddleChatState::Paused, "paused"),
            (WaddleChatState::Inactive, "inactive"),
            (WaddleChatState::Gone, "gone"),
        ];
        for (state, wire) in cases {
            assert_eq!(chat_state_wire_name(state), wire);
            let stanza = build_chat_state_message(
                jid("bob@waddle.test").as_str(),
                chat_state_wire_name(state),
                verb_message_type(false),
                None,
            )
            .expect("mapped state passes the builder's validator");
            assert!(
                stanza.get_child(wire, NS_CHAT_STATES).is_some(),
                "state child <{wire}/> present"
            );
        }
    }

    #[test]
    fn chat_state_stanza_uses_groupchat_type_for_muc() {
        let stanza = build_chat_state_message(
            jid("room@muc.waddle.test").as_str(),
            chat_state_wire_name(WaddleChatState::Composing),
            verb_message_type(true),
            None,
        )
        .expect("state builds");
        assert_eq!(stanza.attr("type"), Some("groupchat"));
        assert!(stanza.get_child("composing", NS_CHAT_STATES).is_some());
    }

    #[tokio::test]
    async fn send_chat_state_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_chat_state(
                "bob@waddle.test".to_string(),
                WaddleChatState::Composing,
                false,
            )
            .await;
        assert!(!sent);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }
}

// ── XEP-0333: Displayed Markers ──────────────────────────────────────────────

mod xep0333 {
    use super::*;
    use waddle_xmpp_client::messaging::build_displayed_message;
    use waddle_xmpp_client::NS_CHAT_MARKERS;

    #[test]
    fn displayed_marker_targets_message_id_with_no_store_hints() {
        let stanza = build_displayed_message(
            jid("bob@waddle.test").as_str(),
            "msg-42",
            verb_message_type(false),
            None,
        );
        assert_eq!(stanza.attr("type"), Some("chat"));
        let displayed = stanza
            .get_child("displayed", NS_CHAT_MARKERS)
            .expect("displayed marker");
        assert_eq!(displayed.attr("id"), Some("msg-42"));
        // Markers are ephemeral: hint the archive away from storing.
        assert!(stanza.get_child("no-store", NS_HINTS).is_some());
        assert!(stanza.get_child("no-permanent-store", NS_HINTS).is_some());
        assert_origin_id(&stanza);
    }

    #[test]
    fn displayed_marker_uses_groupchat_type_for_muc() {
        let stanza = build_displayed_message(
            jid("room@muc.waddle.test").as_str(),
            "msg-43",
            verb_message_type(true),
            None,
        );
        assert_eq!(stanza.attr("type"), Some("groupchat"));
    }

    #[tokio::test]
    async fn send_displayed_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_displayed("bob@waddle.test".to_string(), "msg-42".to_string(), false)
            .await;
        assert!(!sent);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }
}

// ── XEP-0490: Message Displayed Synchronization ──────────────────────────────

mod xep0490 {
    use super::*;
    use crate::convert::mds_catchup_entry_to_ffi;
    use waddle_xmpp_client::mds::{build_mds_subscribe_iq, MdsCatchupEntry, MDS_NODE};

    const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
    const NS_MDS: &str = "urn:xmpp:mds:displayed:0";

    #[test]
    fn publish_iq_carries_chat_item_and_spec_publish_options() {
        let iq = mds_publish_iq(
            &jid("room@muc.waddle.test"),
            &stanza_id("sid-42"),
            &bare("muc.waddle.test"),
        );
        assert_eq!(iq.attr("type"), Some("set"));
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub payload");
        let publish = pubsub.get_child("publish", NS_PUBSUB).expect("publish");
        assert_eq!(publish.attr("node"), Some(MDS_NODE));
        let item = publish.get_child("item", NS_PUBSUB).expect("item");
        // XEP-0490 §3: the item id is the chat JID.
        assert_eq!(item.attr("id"), Some("room@muc.waddle.test"));
        let displayed = item.get_child("displayed", NS_MDS).expect("displayed");
        let sid = displayed
            .get_child("stanza-id", "urn:xmpp:sid:0")
            .expect("stanza-id");
        assert_eq!(sid.attr("id"), Some("sid-42"));
        assert_eq!(sid.attr("by"), Some("muc.waddle.test"));
        // Spec-mandated publish-options travel with every publish.
        let xml = String::from(&iq);
        assert!(xml.contains("pubsub#persist_items"), "{xml}");
        assert!(xml.contains(">whitelist<"), "{xml}");
    }

    #[test]
    fn subscribe_iq_uses_bare_account_jid_as_subscriber() {
        let bare_account = account_bare_jid("alice@waddle.test/phone").expect("account JID parses");
        assert_eq!(bare_account.to_string(), "alice@waddle.test");
        let iq = build_mds_subscribe_iq("iq-1", bare_account.as_str());
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub payload");
        let subscribe = pubsub.get_child("subscribe", NS_PUBSUB).expect("subscribe");
        assert_eq!(subscribe.attr("node"), Some(MDS_NODE));
        assert_eq!(subscribe.attr("jid"), Some("alice@waddle.test"));
    }

    #[test]
    fn catchup_entry_converts_to_ffi_record() {
        let entry = MdsCatchupEntry {
            chat_id: jid("bob@waddle.test"),
            stanza_id: stanza_id("sid-7"),
            stanza_id_by: bare("waddle.test"),
        };
        let ffi = mds_catchup_entry_to_ffi(entry);
        assert_eq!(ffi.chat_id, "bob@waddle.test");
        assert_eq!(ffi.stanza_id, "sid-7");
        assert_eq!(ffi.stanza_id_by, "waddle.test");
    }

    #[tokio::test]
    async fn fetch_mds_displayed_throws_not_connected() {
        let client = test_client(RecordingListener::default());
        let result = client.fetch_mds_displayed().await;
        assert_eq!(result, Err(WaddleError::NotConnected));
    }

    #[tokio::test]
    async fn publish_mds_displayed_rejects_empty_stanza_id() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .publish_mds_displayed(
                "bob@waddle.test".to_string(),
                String::new(),
                "waddle.test".to_string(),
            )
            .await;
        assert!(!sent);
        assert!(
            listener
                .errors()
                .first()
                .is_some_and(|error| error.contains("invalid stanza id")),
            "expected invalid stanza id diagnostic"
        );
    }

    #[tokio::test]
    async fn subscribe_mds_displayed_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        assert!(!client.subscribe_mds_displayed().await);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }

    #[tokio::test]
    async fn supports_mds_publish_options_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        assert!(!client.supports_mds_publish_options().await);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }
}

// ── urn:waddle:pin:0: message pins ───────────────────────────────────────────

mod waddle_pin {
    use super::*;
    use crate::convert::pin_entry_to_ffi;
    use waddle_xmpp_client::messaging::NS_WADDLE_PIN_V0;
    use waddle_xmpp_client::pin::parse_pin_list_response;

    #[test]
    fn pin_room_stanza_targets_stanza_id_as_groupchat() {
        let stanza = pin_room_stanza(&bare("room@muc.waddle.test"), "sid-1");
        assert_eq!(stanza.attr("type"), Some("groupchat"));
        assert_eq!(stanza.attr("to"), Some("room@muc.waddle.test"));
        let pinned = stanza
            .get_child("pinned", NS_WADDLE_PIN_V0)
            .expect("pinned payload");
        assert_eq!(pinned.attr("target"), Some("sid-1"));
        assert_origin_id(&stanza);
    }

    #[test]
    fn unpin_room_stanza_targets_stanza_id_as_groupchat() {
        let stanza = unpin_room_stanza(&bare("room@muc.waddle.test"), "sid-1");
        assert_eq!(stanza.attr("type"), Some("groupchat"));
        let unpinned = stanza
            .get_child("unpinned", NS_WADDLE_PIN_V0)
            .expect("unpinned payload");
        assert_eq!(unpinned.attr("target"), Some("sid-1"));
    }

    #[test]
    fn direct_pin_stanzas_use_chat_type() {
        let pin = pin_direct_stanza(&jid("bob@waddle.test"), "sid-2");
        assert_eq!(pin.attr("type"), Some("chat"));
        assert_eq!(pin.attr("to"), Some("bob@waddle.test"));
        assert!(pin.get_child("pinned", NS_WADDLE_PIN_V0).is_some());

        let unpin = unpin_direct_stanza(&jid("bob@waddle.test"), "sid-2");
        assert_eq!(unpin.attr("type"), Some("chat"));
        assert!(unpin.get_child("unpinned", NS_WADDLE_PIN_V0).is_some());
    }

    #[test]
    fn pin_list_entries_convert_to_ffi_records() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='p1'>\
          <query xmlns='urn:waddle:pin:0'>\
            <pin id='stanza-1' by='admin@waddle.test' at='2026-05-08T12:00:00+00:00'>\
              <preview>\
                <author jid='alice@waddle.test' nick='alice'/>\
                <text>important update</text>\
                <ts>2026-05-08T11:55:00+00:00</ts>\
              </preview>\
            </pin>\
          </query>\
        </iq>"
            .parse()
            .expect("fixture parses");
        let entries: Vec<_> = parse_pin_list_response(&iq)
            .into_iter()
            .map(pin_entry_to_ffi)
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target_stanza_id, "stanza-1");
        assert_eq!(entries[0].pinner_jid, "admin@waddle.test");
        assert_eq!(entries[0].pinned_at, "2026-05-08T12:00:00+00:00");
        assert_eq!(entries[0].preview.author_jid, "alice@waddle.test");
        assert_eq!(entries[0].preview.author_nick.as_deref(), Some("alice"));
        assert_eq!(entries[0].preview.text, "important update");
        assert_eq!(
            entries[0].preview.message_timestamp,
            "2026-05-08T11:55:00+00:00"
        );
    }

    #[tokio::test]
    async fn fetch_room_pins_throws_invalid_jid() {
        let client = test_client(RecordingListener::default());
        let result = client.fetch_room_pins("not a room".to_string()).await;
        assert_eq!(result, Err(WaddleError::InvalidJid));
    }

    #[tokio::test]
    async fn fetch_room_pins_throws_not_connected() {
        let client = test_client(RecordingListener::default());
        let result = client
            .fetch_room_pins("room@muc.waddle.test".to_string())
            .await;
        assert_eq!(result, Err(WaddleError::NotConnected));
    }

    #[tokio::test]
    async fn pin_message_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .pin_message("room@muc.waddle.test".to_string(), "sid-1".to_string())
            .await;
        assert!(!sent);
        assert_eq!(listener.errors(), vec!["Not connected"]);
    }
}

// ── WaddleError mapping ──────────────────────────────────────────────────────

mod waddle_error {
    use super::*;
    use crate::error::client_error_to_waddle;
    use std::time::Duration;
    use waddle_xmpp_client::{ClientError, StanzaError, StanzaErrorType};

    #[test]
    fn client_errors_collapse_into_typed_ffi_variants() {
        assert_eq!(
            client_error_to_waddle(&ClientError::Disconnected),
            WaddleError::NotConnected
        );
        assert_eq!(
            client_error_to_waddle(&ClientError::StanzaError(StanzaError {
                error_type: StanzaErrorType::Auth,
                condition: "forbidden".to_string(),
                text: Some("not an admin".to_string()),
                application_condition: None,
            })),
            WaddleError::Stanza {
                condition: "forbidden".to_string(),
                text: Some("not an admin".to_string()),
            }
        );
        assert_eq!(
            client_error_to_waddle(&ClientError::WebSocketConnectTimeout {
                timeout: Duration::from_secs(5),
            }),
            WaddleError::Timeout
        );
        assert_eq!(
            client_error_to_waddle(&ClientError::TransportClosed),
            WaddleError::Transport
        );
        // Anything unclassified folds into the Transport bucket rather
        // than leaking a stringly-typed diagnostic.
        assert_eq!(
            client_error_to_waddle(&ClientError::RequestCancelled),
            WaddleError::Transport
        );
    }
}
