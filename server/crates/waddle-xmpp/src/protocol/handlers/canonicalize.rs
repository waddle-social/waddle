//! XEP-0359: Unique and Stable Stanza IDs — strip + stamp.
//!
//! Per [`super::super::traits::HandlerOutcome`] semantics this handler
//! mutates the in-flight message: it strips any `<stanza-id>` siblings
//! that claim the same `by=` attribute as the local archive (defending
//! against client spoofing) and then stamps a fresh
//! `<stanza-id by='ctx.full_jid.bare()' id='ctx.id_gen.fresh_stanza_id()'/>`.
//!
//! XEP-0359 §5 conformance, exact:
//!
//! > Before adding the new ID, the entity MUST first remove any other
//! > `<stanza-id/>` elements that contain the same `by` attribute as the
//! > one it intends to stamp.
//!
//! Cross-archive `<stanza-id>` siblings (any other `by=`) are
//! **preserved**, supporting the local-to-local flow where Alice's
//! archive stamp survives Bob's recipient-pass stamping.
//!
//! `<origin-id>` (XEP-0359 §3.2) is treated as **read-only** — never
//! stripped, never modified. The XEP forbids it.
//!
//! # Locality
//!
//! - Sender pass: stamp under `ctx.full_jid.bare()` (sender's archive).
//! - Recipient pass: stamp under `ctx.full_jid.bare()` (recipient's archive).
//! - Both: stamp once for the local user's archive (the same `by=` for
//!   both ends; collision strip applies).
//! - Neither: no-op (this connection is not the canonical archive for
//!   the message).
//!
//! Groupchat handling (`type='groupchat'`) is reserved for the room
//! handler chain in PR5; this handler skips groupchat to avoid stamping
//! the room's archive id under the user's bare JID.

use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use jid::{BareJid, Jid};
use waddle_xmpp_core::xep0359::{add_stanza_id, is_stanza_id_element, StanzaId};
use xmpp_parsers::message::{Message, MessageType};

/// XEP-0359 strip-and-stamp handler for the message pipeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct CanonicalizeHandler;

impl MessageHandler for CanonicalizeHandler {
    fn name(&self) -> &'static str {
        "xep-0359-canonicalize"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        // Groupchat reflections are stamped by the room (different `by=`,
        // owned by the MUC handler chain in PR5). The user-side pipeline
        // doesn't stamp here.
        if matches!(message.type_, MessageType::Groupchat) {
            return HandlerOutcome::Continue(Vec::new());
        }

        // Skip if the local user is not the relevant archive for this
        // message (third-party stanza arriving via routing).
        if matches!(ctx.locality, Locality::Neither) {
            return HandlerOutcome::Continue(Vec::new());
        }

        let local_archive = ctx.full_jid.to_bare();
        let by_jid = Jid::from(local_archive.clone());

        // Strip any pre-existing `<stanza-id>` siblings whose `by=`
        // matches this archive (XEP-0359 §5). Compare via typed
        // [`BareJid`] equality so semantically equivalent JIDs in
        // different string forms (case-folded localpart, IDN, …) still
        // strip — string equality on `by=` would let those slip
        // through and leak through into downstream archive lookups.
        message.payloads.retain(|p| {
            if !is_stanza_id_element(p) {
                return true;
            }
            match p.attr("by").and_then(|raw| raw.parse::<BareJid>().ok()) {
                Some(parsed) => parsed != local_archive,
                // Malformed `by=` — leave the element alone; the
                // server can't claim ownership of an unparseable bare.
                None => true,
            }
        });

        // Stamp a fresh stanza-id from the injected entropy source.
        let id = ctx.id_gen.fresh_stanza_id();
        add_stanza_id(message, &StanzaId::new(id, by_jid));

        HandlerOutcome::Continue(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
    use jid::FullJid;
    use waddle_xmpp_core::xep0359::{
        build_origin_id_element, build_stanza_id_element, extract_origin_id_str, extract_stanza_ids,
    };
    use xmpp_parsers::message::{Message, MessageType};

    fn jid(s: &str) -> Jid {
        s.parse().expect("valid jid")
    }

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn chat_msg(from: &str, to: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m
    }

    fn run_with_id(local: &FullJid, msg: &mut Message, fresh: &str) {
        let bl = Blocklist::empty();
        let occ = MucOccupancy::empty();
        let gen = FixedIdGenerator(fresh.to_string());
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
        let outcome = CanonicalizeHandler.handle(msg, &ctx);
        // Continue with no events — the rewrite is in the message itself.
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    // -----------------------------------------------------------------
    // XEP-0359 §5 — strip + stamp precision
    // -----------------------------------------------------------------

    #[test]
    fn xep_0359_strips_same_by_collision_and_stamps_fresh() {
        let local = full("alice@example.com/web");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        // Client-supplied claim with the same `by=` as the local archive.
        msg.payloads.push(build_stanza_id_element(
            "spoofed",
            &jid("alice@example.com"),
        ));

        run_with_id(&local, &mut msg, "fresh-id-1");

        let stamps = extract_stanza_ids(&msg);
        // Expect exactly one stanza-id with by=alice@example.com and id=fresh-id-1.
        let alice = jid("alice@example.com");
        let alice_stamps: Vec<_> = stamps.iter().filter(|s| s.by == alice).collect();
        assert_eq!(alice_stamps.len(), 1);
        assert_eq!(alice_stamps[0].id, "fresh-id-1");
    }

    #[test]
    fn xep_0359_preserves_cross_archive_stanza_ids() {
        // Recipient pass on Bob's machine. Alice's stamp (by=alice@…)
        // must be preserved; Bob's fresh stamp (by=bob@…) is added.
        let local = full("bob@example.com/desk");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        msg.payloads.push(build_stanza_id_element(
            "alice-A1",
            &jid("alice@example.com"),
        ));

        run_with_id(&local, &mut msg, "bob-B1");

        let stamps = extract_stanza_ids(&msg);
        assert_eq!(stamps.len(), 2);
        let alice = jid("alice@example.com");
        let bob = jid("bob@example.com");
        assert!(stamps.iter().any(|s| s.by == alice && s.id == "alice-A1"));
        assert!(stamps.iter().any(|s| s.by == bob && s.id == "bob-B1"));
    }

    #[test]
    fn xep_0359_preserves_origin_id() {
        let local = full("alice@example.com/web");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        msg.payloads.push(build_origin_id_element("client-X"));

        run_with_id(&local, &mut msg, "stamped-Y");

        // origin-id present and unchanged.
        assert_eq!(extract_origin_id_str(&msg), Some("client-X".to_string()));
        // stamp added.
        let stamps = extract_stanza_ids(&msg);
        assert!(stamps.iter().any(|s| s.id == "stamped-Y"));
    }

    #[test]
    fn xep_0359_preserves_multiple_cross_archive_stamps() {
        let local = full("bob@example.com/desk");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        msg.payloads.push(build_stanza_id_element(
            "alice-A1",
            &jid("alice@example.com"),
        ));
        msg.payloads.push(build_stanza_id_element(
            "charlie-C1",
            &jid("charlie@example.com"),
        ));

        run_with_id(&local, &mut msg, "bob-B1");

        let stamps = extract_stanza_ids(&msg);
        assert_eq!(stamps.len(), 3);
        let alice = jid("alice@example.com");
        let charlie = jid("charlie@example.com");
        let bob = jid("bob@example.com");
        assert!(stamps.iter().any(|s| s.by == alice && s.id == "alice-A1"));
        assert!(stamps
            .iter()
            .any(|s| s.by == charlie && s.id == "charlie-C1"));
        assert!(stamps.iter().any(|s| s.by == bob && s.id == "bob-B1"));
    }

    #[test]
    fn xep_0359_strips_same_archive_stamp_via_typed_jid_equality_not_string_equality() {
        // Client-supplied stamp uses an upper-case domain, which is
        // semantically equal to the local archive after JID
        // normalization but NOT equal as a raw string. A
        // string-equality strip would miss it; the typed `BareJid`
        // strip catches it.
        let local = full("alice@example.com/web");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        msg.payloads.push(build_stanza_id_element(
            "spoofed",
            &jid("alice@EXAMPLE.com"),
        ));

        run_with_id(&local, &mut msg, "fresh-id");

        let stamps = extract_stanza_ids(&msg);
        // After strip-and-stamp, only the freshly stamped id remains
        // (plus any non-matching cross-archive stamps — none here).
        assert_eq!(stamps.len(), 1);
        assert_eq!(stamps[0].id, "fresh-id");
    }

    #[test]
    fn xep_0359_defends_against_client_spoof_under_recipient_archive() {
        // Recipient pass; client claims a stanza-id under `by=bob@…`.
        // Must be stripped and replaced with the fresh stamp.
        let local = full("bob@example.com/desk");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
        msg.payloads
            .push(build_stanza_id_element("spoofed", &jid("bob@example.com")));

        run_with_id(&local, &mut msg, "real-bob-id");

        let stamps = extract_stanza_ids(&msg);
        let bob = jid("bob@example.com");
        let bob_stamps: Vec<_> = stamps.iter().filter(|s| s.by == bob).collect();
        assert_eq!(bob_stamps.len(), 1);
        assert_eq!(bob_stamps[0].id, "real-bob-id");
    }

    // -----------------------------------------------------------------
    // Locality and type guards
    // -----------------------------------------------------------------

    #[test]
    fn xep_0359_groupchat_is_skipped_user_side() {
        // Groupchat stamping is the room handler chain's job (PR5).
        let local = full("alice@example.com/web");
        let mut msg = Message::new(Some("room@conf.example.com".parse().expect("jid")));
        msg.from = Some("alice@example.com/web".parse().expect("jid"));
        msg.type_ = MessageType::Groupchat;

        run_with_id(&local, &mut msg, "should-not-be-stamped");

        assert!(extract_stanza_ids(&msg).is_empty());
    }

    #[test]
    fn xep_0359_neither_locality_is_skipped() {
        let local = full("eve@example.com/web");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com");

        run_with_id(&local, &mut msg, "should-not-stamp");

        assert!(extract_stanza_ids(&msg).is_empty());
    }
}
