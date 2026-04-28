//! Waddle inbox projection — emits [`OutboundEvent::ProjectInbox`].
//!
//! Inbox is a Waddle product surface (not a finalized XEP — the
//! closest precedent is the deferred ProtoXEP "Inbox" used by Movim
//! and Conversations). It tracks the most recent message per
//! conversation per user, keyed by `(owner, peer)`.
//!
//! Locality-aware:
//!
//! - **Sender pass**: `owner = ctx.full_jid.bare()`, `peer = to.bare()`.
//! - **Recipient pass**: `owner = ctx.full_jid.bare()`, `peer = from.bare()`.
//! - **Both** (true self-loop): `owner = peer = local_bare`. One projection.
//! - **Neither**: no-op.
//!
//! The handler runs **after** [`super::canonicalize::CanonicalizeHandler`]
//! per the locked Q2(a) ordering, so the message already carries the
//! XEP-0359 `<stanza-id by='local-bare'/>` stamp; inbox uses it as
//! the typed `archive_ref` so clients can pivot from inbox → MAM
//! through the same id space.
//!
//! Eligibility (avoid noisy projections):
//!
//! - `Chat` or `Normal` only; groupchat inbox writes are owned by the
//!   room handler chain (PR5), error/headline are not conversational.
//! - Substantive body required — pure rich-target requests
//!   (correction, retraction, reply) project the rewritten target,
//!   not the request itself; we keep this handler simple by requiring
//!   a body and revisit when XEP-0424/0308 reach the inbox UI.
//! - XEP-0334 `<no-store/>` (without `<store/>` override) suppresses
//!   inbox just as it suppresses archive — the same privacy hint
//!   applies.

use crate::inbox::runtime::should_project_message;
use crate::protocol::event::{OutboundEvent, StanzaIdRef, StanzaIdValue};
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use crate::xep::xep0359::extract_stanza_id_by;
use jid::BareJid;
use xmpp_parsers::message::{Message, MessageType};

/// Inbox projection handler.
#[derive(Debug, Default, Clone, Copy)]
pub struct InboxHandler;

impl MessageHandler for InboxHandler {
    fn name(&self) -> &'static str {
        "waddle-inbox-projection"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        if !is_eligible(message) {
            return HandlerOutcome::Continue(Vec::new());
        }

        let owner = ctx.full_jid.to_bare();
        // Skip projection if CanonicalizeHandler hasn't stamped under
        // the local archive — an empty XEP-0359 stanza-id is not a
        // meaningful reference and would create inbox rows clients
        // cannot pivot to via MAM. The Q2(a) order guarantees
        // CanonicalizeHandler runs before InboxHandler for chat/normal,
        // but a misconfigured dispatcher (e.g. tests building a custom
        // chain) shouldn't silently produce malformed projections.
        let Some(local_archive_id) = extract_stanza_id_by(message, &owner.to_string()) else {
            return HandlerOutcome::Continue(Vec::new());
        };
        let from_bare = message.from.as_ref().map(|j| j.to_bare());
        let to_bare = message.to.as_ref().map(|j| j.to_bare());

        let mut events: Vec<OutboundEvent> = Vec::new();
        match ctx.locality {
            Locality::Sender => {
                if let Some(peer) = to_bare {
                    // Sender pass: this owner is the message author, so
                    // their unread count must NOT bump for their own
                    // outgoing copy.
                    events.push(build(owner, peer, message, &local_archive_id, false));
                }
            }
            Locality::Recipient => {
                if let Some(peer) = from_bare {
                    // Recipient pass: a brand-new message arriving for
                    // this owner — bump their unread count.
                    events.push(build(owner, peer, message, &local_archive_id, true));
                }
            }
            Locality::Both => {
                // Self-loop alice/web -> alice/web — one inbox entry,
                // peer = owner. The owner is both author and recipient;
                // legacy parity says do not bump unread for self-sent
                // messages.
                events.push(build(
                    owner.clone(),
                    owner,
                    message,
                    &local_archive_id,
                    false,
                ));
            }
            Locality::Neither => {}
        }
        HandlerOutcome::Continue(events)
    }
}

fn build(
    owner: BareJid,
    peer: BareJid,
    message: &Message,
    local_archive_id: &str,
    increment_unread: bool,
) -> OutboundEvent {
    OutboundEvent::ProjectInbox {
        owner: owner.clone(),
        peer,
        message: Box::new(message.clone()),
        archive_ref: StanzaIdRef {
            by: owner,
            id: StanzaIdValue::new(local_archive_id),
        },
        increment_unread,
    }
}

/// Eligibility for inbox projection.
///
/// Mirrors [`crate::inbox::runtime::should_project_message`] so the
/// handler-driven projection matches the legacy runtime path bit-for-bit
/// — bodyless-but-meaningful messages (XEP-0444 reactions,
/// XEP-0424 retractions, XEP-0425 moderation, XEP-0447 file sharing,
/// XEP-0510 stickers, encrypted SFS) keep projecting and no
/// integration-test coverage regresses on cutover.
///
/// Type guards: skip groupchat (room chain owns it in PR6), skip
/// error, skip headline (pure notification, not conversational).
fn is_eligible(message: &Message) -> bool {
    match message.type_ {
        MessageType::Chat | MessageType::Normal => {}
        MessageType::Groupchat | MessageType::Error | MessageType::Headline => return false,
    }
    should_project_message(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use crate::xep::xep0334::Hint;
    use crate::xep::xep0359::build_stanza_id_element;
    use jid::FullJid;
    use minidom::Element;
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    fn run(local: &FullJid, msg: &mut Message) -> HandlerOutcome {
        let bl = Blocklist::empty();
        let occ = MucOccupancy::empty();
        let gen = FixedIdGenerator("test".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: &bl,
            carbons: CarbonsState::Disabled,
            muc_occupancy: &occ,
            id_gen: &gen,
        };
        let ctx = MessageContext::derive(env, msg);
        InboxHandler.handle(msg, &ctx)
    }

    fn extract_inbox(outcome: &HandlerOutcome) -> Vec<(BareJid, BareJid, String)> {
        match outcome {
            HandlerOutcome::Continue(events) => events
                .iter()
                .filter_map(|e| match e {
                    OutboundEvent::ProjectInbox {
                        owner,
                        peer,
                        archive_ref,
                        ..
                    } => Some((
                        owner.clone(),
                        peer.clone(),
                        archive_ref.id.as_str().to_string(),
                    )),
                    _ => None,
                })
                .collect(),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    fn extract_inbox_increments(outcome: &HandlerOutcome) -> Vec<bool> {
        match outcome {
            HandlerOutcome::Continue(events) => events
                .iter()
                .filter_map(|e| match e {
                    OutboundEvent::ProjectInbox {
                        increment_unread, ..
                    } => Some(*increment_unread),
                    _ => None,
                })
                .collect(),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Locality / peer attribution
    // -----------------------------------------------------------------

    #[test]
    fn inbox_sender_pass_keys_owner_local_peer_to() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        // Pretend canonicalize ran and stamped the local archive id.
        msg.payloads
            .push(build_stanza_id_element("alice-A1", "alice@example.com"));
        let outcome = run(&local, &mut msg);
        let inbox = extract_inbox(&outcome);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].0, bare("alice@example.com"));
        assert_eq!(inbox[0].1, bare("bob@example.com"));
        assert_eq!(inbox[0].2, "alice-A1");
    }

    #[test]
    fn inbox_recipient_pass_keys_owner_local_peer_from() {
        let local = full("bob@example.com/desk");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        msg.payloads
            .push(build_stanza_id_element("bob-B1", "bob@example.com"));
        let outcome = run(&local, &mut msg);
        let inbox = extract_inbox(&outcome);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].0, bare("bob@example.com"));
        assert_eq!(inbox[0].1, bare("alice@example.com"));
        assert_eq!(inbox[0].2, "bob-B1");
    }

    #[test]
    fn inbox_neither_locality_emits_no_event() {
        let local = full("eve@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &mut msg);
        assert!(extract_inbox(&outcome).is_empty());
    }

    #[test]
    fn inbox_sender_pass_does_not_increment_unread() {
        // Sender pass: the owner is the author, so unread must not
        // bump for their own outgoing copy.
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        msg.payloads
            .push(build_stanza_id_element("alice-A1", "alice@example.com"));
        let outcome = run(&local, &mut msg);
        assert_eq!(extract_inbox_increments(&outcome), vec![false]);
    }

    #[test]
    fn inbox_recipient_pass_increments_unread() {
        // Recipient pass: a new message arriving for this owner —
        // bump unread.
        let local = full("bob@example.com/desk");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        msg.payloads
            .push(build_stanza_id_element("bob-B1", "bob@example.com"));
        let outcome = run(&local, &mut msg);
        assert_eq!(extract_inbox_increments(&outcome), vec![true]);
    }

    #[test]
    fn inbox_self_loop_does_not_increment_unread() {
        // Self-loop alice→alice: one event, owner=peer, do not bump
        // unread (matches legacy parity for messages you sent yourself).
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "alice@example.com/web", "echo");
        msg.payloads
            .push(build_stanza_id_element("alice-self", "alice@example.com"));
        let outcome = run(&local, &mut msg);
        assert_eq!(extract_inbox_increments(&outcome), vec![false]);
    }

    #[test]
    fn inbox_self_loop_keys_owner_equals_peer() {
        // alice/web -> alice/web (Locality::Both). One inbox entry
        // with owner == peer.
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "alice@example.com/web", "echo");
        msg.payloads
            .push(build_stanza_id_element("alice-self", "alice@example.com"));
        let outcome = run(&local, &mut msg);
        let inbox = extract_inbox(&outcome);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].0, bare("alice@example.com"));
        assert_eq!(inbox[0].1, bare("alice@example.com"));
    }

    // -----------------------------------------------------------------
    // Eligibility — type, body, hints
    // -----------------------------------------------------------------

    #[test]
    fn inbox_groupchat_is_skipped() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "room@conf.example.com", "shouted");
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, &mut msg);
        assert!(extract_inbox(&outcome).is_empty());
    }

    #[test]
    fn inbox_error_message_is_skipped() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "boom");
        msg.type_ = MessageType::Error;
        let outcome = run(&local, &mut msg);
        assert!(extract_inbox(&outcome).is_empty());
    }

    #[test]
    fn inbox_body_less_message_is_skipped() {
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        let outcome = run(&local, &mut msg);
        assert!(extract_inbox(&outcome).is_empty());
    }

    #[test]
    fn inbox_no_store_hint_suppresses_projection() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "ephemeral");
        msg.payloads.push(
            Element::builder(Hint::NoStore.element_name(), crate::xep::xep0334::NS_HINTS).build(),
        );
        let outcome = run(&local, &mut msg);
        assert!(extract_inbox(&outcome).is_empty());
    }

    #[test]
    fn inbox_skips_projection_when_canonicalize_did_not_stamp() {
        // An empty XEP-0359 stanza-id is not a meaningful reference;
        // skip projection rather than emit a malformed event. The
        // dispatcher's Q2(a) order guarantees CanonicalizeHandler runs
        // first in production — this test guards against test
        // fixtures or misconfigured chains producing garbage rows.
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &mut msg);
        assert!(extract_inbox(&outcome).is_empty());
    }

    #[test]
    fn inbox_projects_bodyless_retraction_when_stamped() {
        // Bodyless-but-meaningful: a XEP-0424 retraction has no body
        // but updates the conversation summary. The legacy
        // `inbox::runtime::should_project_message` includes this
        // case; this handler must too.
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        msg.payloads
            .push(crate::xep::xep0424::build_retract_element("stanza-X"));
        msg.payloads
            .push(build_stanza_id_element("alice-A1", "alice@example.com"));
        let outcome = run(&local, &mut msg);
        let inbox = extract_inbox(&outcome);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].2, "alice-A1");
    }
}
