//! The flagship traffic metric (#1320): message stanzas delivered to
//! a local recipient's connection queue, by kind.
//!
//! Classification happens once, at the delivery choke points
//! (`ConnectionRegistry` sends and `UserActor::try_deliver`), from
//! the stanza itself:
//!
//! - `type='groupchat'` → `muc`
//! - a XEP-0045 §7.5 private message (delivered PMs carry exactly one
//!   `muc#user` `<x/>` marker) → `muc_pm`
//! - anything else with a body → `dm`
//!
//! Only messages with a body count — chat states, receipts, and
//! markers are message stanzas but not messages a user would count.

use super::attributes::MessageKind;
use crate::Stanza;
use xmpp_parsers::message::MessageType;

const FANOUT_MESSAGE_ID_MAX_BYTES: usize = 64;

/// Bound a client-controlled message id before attaching it to a
/// per-recipient fan-out span. The returned slice is at most 64 bytes and
/// always ends on a UTF-8 boundary.
pub fn fanout_span_message_id(message_id: &str) -> &str {
    if message_id.len() <= FANOUT_MESSAGE_ID_MAX_BYTES {
        return message_id;
    }

    let mut end = FANOUT_MESSAGE_ID_MAX_BYTES;
    while !message_id.is_char_boundary(end) {
        end -= 1;
    }
    &message_id[..end]
}

/// Classify a stanza about to be queued for local delivery. `None`
/// for non-messages and body-less message stanzas.
pub fn delivered_message_kind(stanza: &Stanza) -> Option<MessageKind> {
    let Stanza::Message(message) = stanza else {
        return None;
    };
    if message.bodies.is_empty() {
        return None;
    }
    if message.type_ == MessageType::Groupchat {
        return Some(MessageKind::Muc);
    }
    let is_muc_pm = message
        .payloads
        .iter()
        .any(|payload| payload.is("x", xmpp_parsers::ns::MUC_USER));
    if is_muc_pm {
        Some(MessageKind::MucPm)
    } else {
        Some(MessageKind::Dm)
    }
}

/// Count one delivered message on the kind-labeled flagship counter.
/// The retired `waddle_messages_total` / `waddle_messages_per_second`
/// text names keep answering via the Mimir alias recording rules
/// (#1330 contract phase).
pub fn record_delivered_message(kind: MessageKind) {
    crate::counter_add!(
        "waddle.messages.delivered",
        "{message}",
        "Message stanzas with a body queued to a local recipient's connection, by kind \
         (dm | muc | muc_pm).",
        1,
        kind,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Lang, Message};

    fn message_with_body(type_: MessageType) -> Message {
        let mut message = Message::new(Some("peer@example.com".parse().expect("valid jid")));
        message.type_ = type_;
        message.bodies.insert(Lang::new(), "hello".to_string());
        message
    }

    #[test]
    fn groupchat_with_body_is_muc() {
        let stanza = Stanza::Message(message_with_body(MessageType::Groupchat));
        assert_eq!(delivered_message_kind(&stanza), Some(MessageKind::Muc));
    }

    #[test]
    fn chat_with_muc_user_marker_is_muc_pm() {
        let mut message = message_with_body(MessageType::Chat);
        message
            .payloads
            .push(minidom::Element::builder("x", xmpp_parsers::ns::MUC_USER).build());
        let stanza = Stanza::Message(message);
        assert_eq!(delivered_message_kind(&stanza), Some(MessageKind::MucPm));
    }

    #[test]
    fn plain_chat_with_body_is_dm() {
        let stanza = Stanza::Message(message_with_body(MessageType::Chat));
        assert_eq!(delivered_message_kind(&stanza), Some(MessageKind::Dm));
    }

    #[test]
    fn bodyless_messages_and_non_messages_do_not_count() {
        let mut chat_state_only =
            Message::new(Some("peer@example.com".parse().expect("valid jid")));
        chat_state_only.type_ = MessageType::Chat;
        assert_eq!(
            delivered_message_kind(&Stanza::Message(chat_state_only)),
            None
        );
        let presence = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));
        assert_eq!(delivered_message_kind(&presence), None);
    }

    #[test]
    fn fanout_span_message_id_caps_bytes_without_splitting_utf8() {
        let ascii = "a".repeat(FANOUT_MESSAGE_ID_MAX_BYTES + 1);
        assert_eq!(
            fanout_span_message_id(&ascii),
            "a".repeat(FANOUT_MESSAGE_ID_MAX_BYTES)
        );

        let multibyte = format!("{}é", "a".repeat(FANOUT_MESSAGE_ID_MAX_BYTES - 1));
        assert_eq!(
            fanout_span_message_id(&multibyte),
            "a".repeat(FANOUT_MESSAGE_ID_MAX_BYTES - 1)
        );
        assert!(fanout_span_message_id(&multibyte)
            .is_char_boundary(fanout_span_message_id(&multibyte).len()));
    }

    #[tokio::test]
    async fn record_delivered_message_counts_by_kind() {
        let guard = crate::telemetry::test_support::acquire().await;

        record_delivered_message(MessageKind::Dm);
        record_delivered_message(MessageKind::Muc);
        record_delivered_message(MessageKind::Muc);

        assert_eq!(
            guard.counter_sum("waddle.messages.delivered", &[("kind", "dm")]),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum("waddle.messages.delivered", &[("kind", "muc")]),
            Some(2)
        );
    }
}
