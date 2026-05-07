//! XEP-0313: Message Archive Management — direct (1:1) archival.
//!
//! Locality-aware:
//!
//! - **Sender pass** (`Locality::Sender` / `Locality::Both`): emit
//!   [`OutboundEvent::ArchiveDirect`] keyed under the sender's bare
//!   JID — the sender's own MAM archive captures their outgoing copy.
//! - **Recipient pass** (`Locality::Recipient` / `Locality::Both`):
//!   emit [`OutboundEvent::ArchiveDirect`] keyed under the recipient's
//!   bare JID — the recipient's MAM archive captures the incoming copy.
//!   This is what gives a local-to-local message *two* archive entries
//!   (sender-side + recipient-side), each with its own XEP-0359
//!   stanza-id `by=` attribution.
//! - **Groupchat**: skipped — the room handler chain (PR5) owns the
//!   single room-side archive write per XEP-0313 §5.1.3.
//! - **Error / Headline / non-Chat-or-Normal**: skipped per XEP-0313
//!   §5.1.3 archive-eligibility rules and XEP-0334 hint precedence.
//!
//! XEP-0313 §5.1.3 archive-eligibility rules implemented here:
//!
//! - Body-less and subject-only messages are not archived (heuristic:
//!   require a non-empty body or a payload that constitutes archivable
//!   content like a XEP-0424 retraction or XEP-0308 correction).
//! - XEP-0334 `<no-store>` / `<no-permanent-store>` hints suppress
//!   archive (with `<store>` overriding back on per §3 of that XEP).
//!
//! The handler does not perform the archive write — it emits the
//! typed event and the interpreter persists. Multiple `ArchiveDirect`
//! events from a single dispatch (sender-side and recipient-side
//! when locality is `Both`) flow through the same interpreter arm.

use crate::protocol::event::OutboundEvent;
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use crate::xep::xep0334::HintCarrier;
use xmpp_parsers::message::{Body, Message, MessageType};

/// XEP-0313 archive handler for the user-side message pipeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct ArchiveHandler;

impl MessageHandler for ArchiveHandler {
    fn name(&self) -> &'static str {
        "xep-0313-archive"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        if !is_archivable(message) {
            return HandlerOutcome::Continue(Vec::new());
        }

        let mut events: Vec<OutboundEvent> = Vec::new();
        let local_bare = ctx.full_jid.to_bare();
        let from_bare = message.from.as_ref().map(|j| j.to_bare());
        let to_bare = message.to.as_ref().map(|j| j.to_bare());

        // Sender-side write — keyed under the local user's archive when
        // the local user is the sender. Use the message's `from`/`to`
        // for the canonical (from, to) tuple; fall back to `local_bare`
        // if the message lacks an explicit address. Also covers the
        // `Locality::Both` true self-loop (alice/web -> alice/web), in
        // which case the recipient-side branch below is skipped to
        // avoid double-stamping the same archive.
        if ctx.locality.is_sender() {
            if let Some(to) = to_bare.clone() {
                let from = from_bare.clone().unwrap_or_else(|| local_bare.clone());
                events.push(OutboundEvent::ArchiveDirect {
                    archive_jid: from.clone(),
                    from,
                    to,
                    message: Box::new(message.clone()),
                });
            }
        }

        // Recipient-side write — fires on `Locality::Recipient` (which
        // includes cross-resource self-chat like alice/web -> alice/phone
        // on alice/phone's connection, where alice's archive captures the
        // incoming copy distinct from the sender-side outgoing copy).
        // Suppressed for `Locality::Both` because the sender branch
        // above already wrote the single event for that degenerate
        // self-loop case.
        if matches!(ctx.locality, Locality::Recipient) {
            if let Some(from) = from_bare {
                let to = to_bare.unwrap_or_else(|| local_bare.clone());
                events.push(OutboundEvent::ArchiveDirect {
                    archive_jid: to.clone(),
                    from,
                    to,
                    message: Box::new(message.clone()),
                });
            }
        }

        HandlerOutcome::Continue(events)
    }
}

/// XEP-0313 §5.1.3 archive-eligibility heuristic.
///
/// Returns `true` when the message should be archived.
pub fn is_archivable(message: &Message) -> bool {
    // Skip non-archivable types.
    match message.type_ {
        MessageType::Chat | MessageType::Normal => {}
        // Groupchat is the room chain's job; error / headline are
        // never archived.
        MessageType::Groupchat | MessageType::Error | MessageType::Headline => return false,
    }
    // XEP-0334 hint precedence (`<store>` overrides `<no-store>`).
    if message.should_skip_archive() {
        return false;
    }
    // An explicit `<store/>` hint forces archival even for body-less
    // stanzas (XEP-0334 §5.4 SHOULD store). Without it, require either
    // a non-empty body or a substantive archivable payload.
    if message.has_store() {
        return true;
    }
    let has_body = has_substantive_body(message);
    // Skip subject-only messages — XEP-0313 §5.1.3 lists these as
    // non-archivable since they're typically MUC subject changes that
    // the room would handle.
    if !message.subjects.is_empty() && !has_body {
        return false;
    }
    has_body || has_archivable_payload(message)
}

/// True when the message carries content that justifies a MAM archive
/// row even without an explicit `<store/>` hint (a non-empty body or a
/// substantive XEP-0308 / XEP-0424 / XEP-0444 payload).
///
/// Public so the DM-intake classifier shares the exact same matrix as
/// [`is_archivable`] — the two must never disagree, or the classifier
/// will route stanzas to `pending_delivery` as `Archived` whose MAM row
/// the archive handler refuses to write (issue #209 finding #1).
pub fn has_archivable_content(message: &Message) -> bool {
    has_substantive_body(message) || has_archivable_payload(message)
}

fn has_substantive_body(message: &Message) -> bool {
    message
        .bodies
        .values()
        .any(|Body(text)| !text.trim().is_empty())
}

fn has_archivable_payload(message: &Message) -> bool {
    use crate::xep::{xep0308, xep0424, xep0444};
    xep0308::is_correction_message(message)
        || matches!(
            xep0424::extract_retraction_from_message(message),
            Some(xep0424::RetractionKind::Request(_))
        )
        || xep0444::extract_reactions_from_message(message).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use crate::xep::xep0334::Hint;
    use jid::{BareJid, FullJid};
    use minidom::Element;
    use xmpp_parsers::message::{Body, Message, MessageType, Subject};

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
            has_live_transport: true,
            id_gen: &gen,
        };
        let ctx = MessageContext::derive(env, msg);
        ArchiveHandler.handle(msg, &ctx)
    }

    fn extract_archive_events(outcome: &HandlerOutcome) -> Vec<(BareJid, BareJid)> {
        match outcome {
            HandlerOutcome::Continue(events) => events
                .iter()
                .filter_map(|e| match e {
                    OutboundEvent::ArchiveDirect { from, to, .. } => {
                        Some((from.clone(), to.clone()))
                    }
                    _ => None,
                })
                .collect(),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    fn extract_archive_jids(outcome: &HandlerOutcome) -> Vec<BareJid> {
        match outcome {
            HandlerOutcome::Continue(events) => events
                .iter()
                .filter_map(|e| match e {
                    OutboundEvent::ArchiveDirect { archive_jid, .. } => Some(archive_jid.clone()),
                    _ => None,
                })
                .collect(),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Locality + (from, to) shape
    // -----------------------------------------------------------------

    #[test]
    fn xep_0313_sender_pass_emits_one_archive_direct_keyed_for_local_user() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &mut msg);
        let archives = extract_archive_events(&outcome);
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].0, bare("alice@example.com"));
        assert_eq!(archives[0].1, bare("bob@example.com"));
    }

    #[test]
    fn xep_0313_recipient_pass_emits_one_archive_direct_for_recipient_archive() {
        let local = full("bob@example.com/desk");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &mut msg);
        let archives = extract_archive_events(&outcome);
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].0, bare("alice@example.com"));
        assert_eq!(archives[0].1, bare("bob@example.com"));
    }

    #[test]
    fn xep_0313_neither_locality_emits_nothing() {
        let local = full("eve@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &mut msg);
        assert_eq!(extract_archive_events(&outcome).len(), 0);
    }

    #[test]
    fn xep_0313_true_self_loop_same_resource_emits_single_archive_not_double() {
        // alice/web -> alice/web. Locality::Both; sender-side fires;
        // recipient-side branch skipped because Both already covered
        // by sender-side. One archive entry, not two.
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "alice@example.com/web", "echo");
        let outcome = run(&local, &mut msg);
        let archives = extract_archive_events(&outcome);
        assert_eq!(archives.len(), 1);
    }

    #[test]
    fn xep_0424_tombstones_are_not_archived_as_inbound_messages() {
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some(jid::Jid::from(bare("bob@example.com"))));
        msg.from = Some(jid::Jid::from(local.clone()));
        msg.type_ = MessageType::Chat;
        msg.payloads.push(
            Element::builder("retracted", crate::xep::xep0424::NS_MESSAGE_RETRACT)
                .attr("id", "retract-1")
                .attr("stamp", "2024-06-01T09:00:00Z")
                .build(),
        );

        let outcome = run(&local, &mut msg);
        assert!(
            extract_archive_events(&outcome).is_empty(),
            "archive-side tombstones must not be treated as inbound archivable messages"
        );
    }

    #[test]
    fn xep_0313_sender_pass_archive_jid_is_local_user_bare() {
        // The archive_jid the interpreter writes to MUST be the local
        // user's archive on the sender pass (so the interpreter does
        // not need to know which side it's on).
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &mut msg);
        let archives = extract_archive_jids(&outcome);
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0], bare("alice@example.com"));
    }

    #[test]
    fn xep_0313_recipient_pass_archive_jid_is_local_user_bare() {
        // Same invariant on the recipient pass: archive_jid is the
        // local user's bare, not the message sender's.
        let local = full("bob@example.com/desk");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &mut msg);
        let archives = extract_archive_jids(&outcome);
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0], bare("bob@example.com"));
    }

    #[test]
    fn xep_0313_cross_resource_self_chat_emits_recipient_side_archive() {
        // alice/web -> alice/phone, observed from alice/phone's
        // connection. Locality::Recipient (the asymmetric derive in
        // PR1 ensures cross-resource self-chat is NOT classified as
        // Both). The recipient-side write must fire so alice's
        // archive captures the incoming copy on alice/phone — earlier
        // versions of this handler suppressed the write whenever the
        // bare JIDs matched, losing the entry.
        let local = full("alice@example.com/phone");
        let mut msg = chat_with_body(
            "alice@example.com/web",
            "alice@example.com/phone",
            "ping yourself",
        );
        let outcome = run(&local, &mut msg);
        let archives = extract_archive_events(&outcome);
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].0, bare("alice@example.com"));
        assert_eq!(archives[0].1, bare("alice@example.com"));
    }

    // -----------------------------------------------------------------
    // XEP-0313 §5.1.3 + XEP-0334 hint precedence
    // -----------------------------------------------------------------

    #[test]
    fn xep_0313_groupchat_is_skipped_user_side() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "room@conf.example.com", "shouted");
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0313_error_messages_are_not_archived() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "boom");
        msg.type_ = MessageType::Error;
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0313_body_less_message_is_not_archived() {
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0313_subject_only_message_is_not_archived() {
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        msg.subjects
            .insert(String::new(), Subject("New Topic".to_string()));
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0334_no_store_hint_suppresses_archive() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "ephemeral");
        msg.payloads.push(
            Element::builder(Hint::NoStore.element_name(), crate::xep::xep0334::NS_HINTS).build(),
        );
        let outcome = run(&local, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    #[test]
    fn xep_0334_store_hint_overrides_no_store() {
        let local = full("alice@example.com/web");
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "force store");
        msg.payloads.push(
            Element::builder(Hint::NoStore.element_name(), crate::xep::xep0334::NS_HINTS).build(),
        );
        msg.payloads.push(
            Element::builder(Hint::Store.element_name(), crate::xep::xep0334::NS_HINTS).build(),
        );
        let outcome = run(&local, &mut msg);
        assert_eq!(extract_archive_events(&outcome).len(), 1);
    }

    #[test]
    fn xep_0313_retraction_without_body_is_archivable_payload() {
        // A XEP-0424 retraction may have no body but still warrants
        // archival — clients query MAM to see the tombstone.
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        msg.payloads
            .push(crate::xep::xep0424::build_retract_element("stanza-X"));
        let outcome = run(&local, &mut msg);
        assert_eq!(extract_archive_events(&outcome).len(), 1);
    }

    #[test]
    fn xep_0313_reaction_without_body_is_archivable_payload() {
        // XEP-0444 reaction messages are bodyless, but clients reload
        // reaction state from MAM after reconnect/page refresh.
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        msg.payloads
            .push(crate::xep::xep0444::build_reactions_element(
                "stanza-X",
                &["🔥"],
            ));
        let outcome = run(&local, &mut msg);
        assert_eq!(extract_archive_events(&outcome).len(), 1);
    }

    #[test]
    fn xep_0313_malformed_reaction_without_target_id_is_not_archived() {
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("bob@example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Chat;
        msg.payloads.push(
            Element::builder("reactions", crate::xep::xep0444::NS_REACTIONS)
                .append(
                    Element::builder("reaction", crate::xep::xep0444::NS_REACTIONS)
                        .append("🔥")
                        .build(),
                )
                .build(),
        );
        let outcome = run(&local, &mut msg);
        assert!(extract_archive_events(&outcome).is_empty());
    }
}
