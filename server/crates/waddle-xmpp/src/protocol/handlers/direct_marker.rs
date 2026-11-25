//! XEP-0333 chat marker → inbox read position for one-to-one chats.
//!
//! A client that has read a one-to-one message sends the peer a
//! `<message type='chat'>` carrying `<displayed xmlns='urn:xmpp:chat-markers:0'
//! id='…'/>` (or `<acknowledged/>`). XEP-0430 §"Unread Messages" makes that
//! marker cause the message — and every message before it — to be read, so
//! this handler emits [`OutboundEvent::MarkInboxReadFromMarker`] on the
//! **sender pass** only: the marker reports the sender's own read state, and a
//! recipient observing it must not clear their own inbox.
//!
//! The marker's `id` is the `id` attribute of the peer's message (XEP-0333
//! §"Sending Displayed Markers"); the interpreter resolves it against the
//! sender's personal archive, by origin-id first and wire id second, and
//! ignores a marker whose message is not in the conversation.
//!
//! Group chat markers are owned by the room handler chain
//! ([`crate::protocol::room`]); a marker addressed to the sender's own bare
//! JID refers to no peer and is ignored.

use crate::protocol::event::{MarkerConversation, OutboundEvent};
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use crate::xep::xep0333::extract_marker_from_message;
use xmpp_parsers::message::{Message, MessageType};

/// Direct-message chat-marker handler.
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectMarkerHandler;

impl MessageHandler for DirectMarkerHandler {
    fn name(&self) -> &'static str {
        "xep-0333-direct-marker"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        if !matches!(message.type_, MessageType::Chat | MessageType::Normal)
            || ctx.locality != Locality::Sender
        {
            return HandlerOutcome::Continue(Vec::new());
        }
        let Some(marked_id) = extract_marker_from_message(message)
            .and_then(|marker| marker.read_through_id().map(str::to_owned))
        else {
            return HandlerOutcome::Continue(Vec::new());
        };
        let owner = ctx.full_jid.to_bare();
        let Some(peer) = message.to.as_ref().map(|to| to.to_bare()) else {
            return HandlerOutcome::Continue(Vec::new());
        };
        if peer == owner {
            return HandlerOutcome::Continue(Vec::new());
        }
        HandlerOutcome::Continue(vec![OutboundEvent::MarkInboxReadFromMarker {
            owner,
            conversation: MarkerConversation::Direct(peer),
            marked_message_id: marked_id,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use crate::xep::xep0333::{build_displayed_element, NS_CHAT_MARKERS};
    use jid::{BareJid, FullJid};
    use minidom::Element;

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn marker_message(from: &str, to: &str, element: Element) -> Message {
        let mut msg = Message::new(Some(to.parse().expect("jid")));
        msg.from = Some(from.parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        msg.payloads.push(element);
        msg
    }

    fn acknowledged(id: &str) -> Element {
        Element::builder("acknowledged", NS_CHAT_MARKERS)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .build()
    }

    fn run(local: &FullJid, msg: &mut Message) -> Vec<OutboundEvent> {
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("test".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: &blocklist,
            carbons: CarbonsState::Disabled,
            muc_occupancy: &occupancy,
            has_live_transport: true,
            delivery_fanout: &[],
            id_gen: &id_gen,
        };
        let ctx = MessageContext::derive(env, msg);
        match DirectMarkerHandler.handle(msg, &ctx) {
            HandlerOutcome::Continue(events) => events,
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[test]
    fn sender_pass_displayed_marker_marks_the_peer_conversation() {
        let local = full("alice@example.com/web");
        let mut msg = marker_message(
            "alice@example.com/web",
            "bob@example.com",
            build_displayed_element("bob-msg-2"),
        );

        let events = run(&local, &mut msg);

        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::MarkInboxReadFromMarker {
                owner,
                conversation,
                marked_message_id,
            } => {
                assert_eq!(owner, &bare("alice@example.com"));
                assert_eq!(
                    conversation,
                    &MarkerConversation::Direct(bare("bob@example.com"))
                );
                assert_eq!(marked_message_id, "bob-msg-2");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn sender_pass_acknowledged_marker_marks_the_peer_conversation() {
        let local = full("alice@example.com/web");
        let mut msg = marker_message(
            "alice@example.com/web",
            "bob@example.com/desk",
            acknowledged("bob-msg-3"),
        );

        let events = run(&local, &mut msg);

        assert!(matches!(
            events.as_slice(),
            [OutboundEvent::MarkInboxReadFromMarker { marked_message_id, .. }]
                if marked_message_id == "bob-msg-3"
        ));
    }

    #[test]
    fn recipient_pass_never_marks_the_recipient_inbox() {
        let local = full("bob@example.com/desk");
        let mut msg = marker_message(
            "alice@example.com/web",
            "bob@example.com",
            build_displayed_element("bob-msg-2"),
        );

        assert!(run(&local, &mut msg).is_empty());
    }

    #[test]
    fn markable_requests_and_self_addressed_markers_emit_nothing() {
        let local = full("alice@example.com/web");
        let mut markable = marker_message(
            "alice@example.com/web",
            "bob@example.com",
            crate::xep::xep0333::build_markable_element(),
        );
        markable.id = Some(xmpp_parsers::message::Id("m-1".into()));
        assert!(run(&local, &mut markable).is_empty());

        let mut to_self = marker_message(
            "alice@example.com/web",
            "alice@example.com",
            build_displayed_element("m-2"),
        );
        assert!(run(&local, &mut to_self).is_empty());
    }

    #[test]
    fn groupchat_markers_are_left_to_the_room_chain() {
        let local = full("alice@example.com/web");
        let mut msg = marker_message(
            "alice@example.com/web",
            "team@conf.example.com",
            build_displayed_element("m-3"),
        );
        msg.type_ = MessageType::Groupchat;

        assert!(run(&local, &mut msg).is_empty());
    }
}
