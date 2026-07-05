//! XEP-0160 offline-message intake handler (issue #209).
//!
//! Runs the locked Q3 = A typed [`classify_dm_intake`] classifier and,
//! on the **headless recipient pass** (where `has_live_transport ==
//! false` — see [`crate::protocol::HEADLESS_RECIPIENT_RESOURCE`]),
//! emits an [`OutboundEvent::QueueOfflineDelivery`] for stanzas the
//! classifier marks as `pending != None`.
//!
//! Position in the locked Q2(a) chain: AFTER
//! [`super::archive::ArchiveHandler`] so that `Archived` payloads can
//! reference the just-stamped XEP-0359 `<stanza-id by='recipient'/>`.
//! BEFORE [`super::route::RouteHandler`] — though Route is a no-op on
//! the headless pass anyway.
//!
//! On the live recipient pass (`has_live_transport == true`), this
//! handler is a no-op — by definition the recipient has at least one
//! online resource (this one) so XEP-0160 §3 step 2 does not trigger.
//! Carbons fanout to other resources is owned by
//! [`super::carbons_message::CarbonsMessageHandler`]; offline catch-up
//! for resources that join later is handled by client-driven MAM
//! catch-up per locked Q10a.

use crate::pending_delivery::PendingPayload;
use crate::protocol::dm_routing::{
    classify_dm_intake, DmRouting, OnlineResources, PendingDecision,
};
use crate::protocol::event::OutboundEvent;
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use chrono::Utc;
use jid::Jid;
use waddle_xmpp_core::xep0359::{extract_stanza_id_by, StanzaId};
use xmpp_parsers::message::Message;

/// Intake handler for XEP-0160 offline storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct OfflineDeliveryHandler;

impl MessageHandler for OfflineDeliveryHandler {
    fn name(&self) -> &'static str {
        "xep0160-offline-delivery"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        // Only the recipient pass writes pending_delivery rows. Sender /
        // Both / Neither passes have no recipient-side offline obligation.
        if !matches!(ctx.locality, Locality::Recipient) {
            return HandlerOutcome::Continue(Vec::new());
        }

        // Live recipient pass means at least one resource of the
        // recipient is online (this one) — XEP-0160 §3 step 2 does not
        // trigger. The headless pass is the only one that should ever
        // queue for offline delivery.
        if ctx.has_live_transport {
            return HandlerOutcome::Continue(Vec::new());
        }

        // Headless pass ⇒ recipient has zero online resources.
        // Pass an empty `OnlineResources` to the classifier so the
        // priority-trigger branch fires.
        let online = OnlineResources::empty();
        let routing: DmRouting = classify_dm_intake(message, &online, ctx.blocklist);

        let recipient_bare = ctx.full_jid.to_bare();
        match routing.pending {
            PendingDecision::None => HandlerOutcome::Continue(Vec::new()),
            PendingDecision::Archived => {
                // The MAM writer has stamped a XEP-0359 <stanza-id> by
                // the recipient archive — read it back so the
                // pending_delivery row points at the same MAM entry.
                let owner_jid = Jid::from(recipient_bare.clone());
                let Some(stanza_id_str) = extract_stanza_id_by(message, &owner_jid) else {
                    // The classifier produced PendingDecision::Archived
                    // (so it expects the stanza to be in MAM), but the
                    // recipient-archive XEP-0359 stamp is missing. The
                    // locked Q2(a) handler order puts ArchiveHandler
                    // *before* this handler precisely so the stamp is
                    // present here, so a missing stamp signals one of:
                    //   - chain misconfiguration (test fixture / custom
                    //     dispatcher with handlers reordered),
                    //   - ArchiveHandler skipped this stanza despite
                    //     classifier eligibility (silent disagreement),
                    //   - canonicalize stage was bypassed.
                    //
                    // Emit a typed Log so production deployments surface
                    // the bug, then drop the stanza rather than write a
                    // pending_delivery row pointing at a non-existent
                    // MAM id (which would loop forever as a poison
                    // pill on every flush). Falling back to Transient
                    // would discard the message-vs-archive consistency
                    // guarantee the rest of the stack relies on; it's
                    // safer to surface than to paper over.
                    return HandlerOutcome::Continue(vec![OutboundEvent::Log {
                        level: tracing::Level::WARN,
                        message: format!(
                            "OfflineDeliveryHandler: classifier said Archived for \
                             recipient={recipient_bare} but no <stanza-id by='{recipient_bare}'/> \
                             stamp present — chain misconfiguration suspected; dropping \
                             pending_delivery row to avoid dangling MAM reference"
                        ),
                    }]);
                };
                let payload =
                    PendingPayload::Archived(StanzaId::new(stanza_id_str, owner_jid.clone()));
                if message.from.is_none() {
                    return HandlerOutcome::Continue(Vec::new());
                }
                HandlerOutcome::Continue(vec![OutboundEvent::QueueOfflineDelivery {
                    recipient: recipient_bare,
                    payload,
                    original_receipt_at: Utc::now(),
                    original_message: Box::new(message.clone()),
                }])
            }
            PendingDecision::Transient => {
                // <no-permanent-store/>: no MAM row exists. Carry the
                // stanza inline so the flush handler can replay it
                // verbatim.
                let payload = PendingPayload::Transient(Box::new(message.clone()));
                if message.from.is_none() {
                    return HandlerOutcome::Continue(Vec::new());
                }
                HandlerOutcome::Continue(vec![OutboundEvent::QueueOfflineDelivery {
                    recipient: recipient_bare,
                    payload,
                    original_receipt_at: Utc::now(),
                    original_message: Box::new(message.clone()),
                }])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use crate::xep::xep0334::{add_hint, Hint};
    use jid::{BareJid, FullJid};
    use waddle_xmpp_core::xep0359::build_stanza_id_element;
    use xmpp_parsers::message::MessageType;

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn dm_chat(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse::<Jid>().expect("jid")));
        m.from = Some(from.parse::<Jid>().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_string());
        m
    }

    fn stamp_stanza_id(message: &mut Message, by: &BareJid, id: &str) {
        let by_jid = Jid::from(by.clone());
        message.payloads.push(build_stanza_id_element(id, &by_jid));
    }

    fn ctx_for<'a>(
        full_jid: &'a FullJid,
        blocklist: &'a Blocklist,
        muc_occupancy: &'a MucOccupancy,
        id_gen: &'a FixedIdGenerator,
        has_live_transport: bool,
        message: &Message,
    ) -> MessageContext<'a> {
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid,
            blocklist,
            carbons: CarbonsState::Disabled,
            muc_occupancy,
            has_live_transport,
            delivery_fanout: &[],
            id_gen,
        };
        MessageContext::derive(env, message)
    }

    fn first_offline_event(events: &[OutboundEvent]) -> Option<&OutboundEvent> {
        events
            .iter()
            .find(|e| matches!(e, OutboundEvent::QueueOfflineDelivery { .. }))
    }

    #[test]
    fn headless_recipient_pass_chat_emits_archived_pending() {
        let alice = full("alice@example.com/offline-recipient-pass");
        let alice_bare = alice.to_bare();
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("ignored".to_string());

        let mut msg = dm_chat("bob@elsewhere/x", "alice@example.com", "hi");
        stamp_stanza_id(&mut msg, &alice_bare, "mam-id-1");

        let ctx = ctx_for(&alice, &blocklist, &occupancy, &id_gen, false, &msg);
        let outcome = OfflineDeliveryHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            _ => panic!("expected Continue"),
        };
        let event = first_offline_event(&events).expect("offline event emitted");
        match event {
            OutboundEvent::QueueOfflineDelivery {
                recipient, payload, ..
            } => {
                assert_eq!(*recipient, alice_bare);
                match payload {
                    PendingPayload::Archived(stanza_id_ref) => {
                        assert_eq!(stanza_id_ref.by, alice_bare);
                        assert_eq!(stanza_id_ref.id.as_str(), "mam-id-1");
                    }
                    _ => panic!("expected Archived payload"),
                }
            }
            _ => panic!("wrong event variant"),
        }
    }

    #[test]
    fn live_recipient_pass_does_not_emit_offline_event() {
        let alice = full("alice@example.com/web");
        let alice_bare = alice.to_bare();
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("ignored".to_string());

        let mut msg = dm_chat("bob@elsewhere/x", "alice@example.com", "hi");
        stamp_stanza_id(&mut msg, &alice_bare, "mam-id-2");

        // has_live_transport=true → live pass, recipient is online.
        let ctx = ctx_for(&alice, &blocklist, &occupancy, &id_gen, true, &msg);
        let outcome = OfflineDeliveryHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            _ => panic!("expected Continue"),
        };
        assert!(first_offline_event(&events).is_none());
    }

    #[test]
    fn sender_pass_does_not_emit_offline_event() {
        let alice = full("alice@example.com/web");
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("ignored".to_string());

        // Sender pass: from = local user.
        let mut msg = dm_chat("alice@example.com/web", "bob@elsewhere", "hi");
        let ctx = ctx_for(&alice, &blocklist, &occupancy, &id_gen, false, &msg);
        let outcome = OfflineDeliveryHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            _ => panic!("expected Continue"),
        };
        assert!(first_offline_event(&events).is_none());
    }

    #[test]
    fn no_permanent_store_emits_transient_pending() {
        let alice = full("alice@example.com/offline-recipient-pass");
        let alice_bare = alice.to_bare();
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("ignored".to_string());

        let mut msg = dm_chat("bob@elsewhere/x", "alice@example.com", "off the record");
        add_hint(&mut msg, Hint::NoPermanentStore);
        // Importantly: no <stanza-id> stamp because ArchiveHandler
        // does not archive <no-permanent-store/>.

        let ctx = ctx_for(&alice, &blocklist, &occupancy, &id_gen, false, &msg);
        let outcome = OfflineDeliveryHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            _ => panic!("expected Continue"),
        };
        let event = first_offline_event(&events).expect("offline event emitted");
        match event {
            OutboundEvent::QueueOfflineDelivery {
                recipient, payload, ..
            } => {
                assert_eq!(*recipient, alice_bare);
                assert!(payload.is_transient());
            }
            _ => panic!("wrong event variant"),
        }
    }

    #[test]
    fn no_store_hint_skips_event_entirely() {
        let alice = full("alice@example.com/offline-recipient-pass");
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("ignored".to_string());

        let mut msg = dm_chat("bob@elsewhere/x", "alice@example.com", "ephemeral");
        add_hint(&mut msg, Hint::NoStore);

        let ctx = ctx_for(&alice, &blocklist, &occupancy, &id_gen, false, &msg);
        let outcome = OfflineDeliveryHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            _ => panic!("expected Continue"),
        };
        assert!(first_offline_event(&events).is_none());
    }

    #[test]
    fn blocked_sender_skips_event_entirely() {
        let alice = full("alice@example.com/offline-recipient-pass");
        let blocklist = Blocklist::new([bare("blocked@elsewhere")]);
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("ignored".to_string());

        let mut msg = dm_chat("blocked@elsewhere/x", "alice@example.com", "spam");
        stamp_stanza_id(&mut msg, &alice.to_bare(), "mam-id-3");

        let ctx = ctx_for(&alice, &blocklist, &occupancy, &id_gen, false, &msg);
        let outcome = OfflineDeliveryHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            _ => panic!("expected Continue"),
        };
        assert!(first_offline_event(&events).is_none());
    }

    #[test]
    fn archived_decision_without_stanza_id_stamp_is_skipped() {
        // Defensive: if the chain is misconfigured and ArchiveHandler
        // was bypassed but classifier still said Archived, we should
        // not emit a dangling reference to a non-existent MAM row.
        let alice = full("alice@example.com/offline-recipient-pass");
        let blocklist = Blocklist::empty();
        let occupancy = MucOccupancy::empty();
        let id_gen = FixedIdGenerator("ignored".to_string());

        let mut msg = dm_chat("bob@elsewhere/x", "alice@example.com", "hi");
        // No stamp — would normally have been added by ArchiveHandler.

        let ctx = ctx_for(&alice, &blocklist, &occupancy, &id_gen, false, &msg);
        let outcome = OfflineDeliveryHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            HandlerOutcome::Continue(events) => events,
            _ => panic!("expected Continue"),
        };
        assert!(first_offline_event(&events).is_none());
    }
}
