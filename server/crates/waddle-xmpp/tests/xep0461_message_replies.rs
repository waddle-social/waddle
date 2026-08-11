//! XEP-0461: Message Replies — dedicated conformance suite.
//!
//! In-crate `xep::xep0461::tests` covers the helper parse/build
//! cases. This suite pins the audit-level invariants that crossing
//! the public-API boundary exposes:
//!
//! - the §3 namespace string `urn:xmpp:reply:0`,
//! - the §"Discovering Support" advertisement obligation on both
//!   server-wide and per-MUC-room disco surfaces,
//! - §3 wire shape: `<reply xmlns='urn:xmpp:reply:0' id='…' to='…'/>`
//!   with `id` required and `to` optional,
//! - parser robustness: trailing whitespace, empty / malformed `to`,
//!   and `id`-less payloads, none of which may produce a valid
//!   reply reference.

use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, server_features, Feature};
use waddle_xmpp::xep::xep0461::{
    build_reply_element, is_reply_element, parse_reply_from_message, set_reply_payload,
    ReplyReference, NS_REPLY,
};
use xmpp_parsers::message::Message;

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0461_namespace_matches_spec() {
    // XEP-0461 §3 pins this namespace literal. Clients dispatch on
    // it, so a typo silently drops replies into "unknown payload".
    assert_eq!(NS_REPLY, "urn:xmpp:reply:0");
}

// ── §"Discovering Support" disco advert ─────────────────────────────

#[test]
fn xep0461_server_features_advertise_reply_namespace() {
    // XEP-0461 §"Discovering Support": a service supporting replies
    // SHOULD advertise `urn:xmpp:reply:0`. Waddle routes replies on
    // both DMs and groupchats, so the advert is mandatory.
    let feats = server_features();
    let target = Feature::replies();
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `urn:xmpp:reply:0`"
    );
}

#[test]
fn xep0461_muc_rooms_advertise_reply_feature_in_every_configuration() {
    // Replies are routinely used in group chats. The room-level
    // disco MUST also advertise reply support regardless of which
    // configuration cell (persistent × members_only × moderated ×
    // forum) the room sits in — the routing path is config-independent.
    let target = Feature::replies();
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, true, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features(persistent={persistent}, members_only={members_only}, \
                         moderated={moderated}, forum={forum}) must advertise `urn:xmpp:reply:0`"
                    );
                }
            }
        }
    }
}

#[test]
fn xep0461_feature_constructor_pins_namespace_string() {
    // Defence in depth against a future rename silently changing
    // what's on the wire.
    assert_eq!(Feature::replies().0, "urn:xmpp:reply:0");
}

// ── §3 wire shape ────────────────────────────────────────────────────

#[test]
fn xep0461_classifier_accepts_spec_shape_only() {
    let canonical = Element::builder("reply", NS_REPLY)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "msg-1")
        .build();
    assert!(is_reply_element(&canonical));

    let wrong_ns = Element::builder("reply", "wrong:ns").build();
    assert!(!is_reply_element(&wrong_ns));

    let wrong_name = Element::builder("response", NS_REPLY).build();
    assert!(!is_reply_element(&wrong_name));
}

#[test]
fn xep0461_builder_emits_id_required_to_optional() {
    // §3: `id` is REQUIRED, `to` is OPTIONAL. The builder must
    // produce both attribute shapes faithfully so peers can decide
    // whether to display the optional sender JID.
    let with_to = build_reply_element(
        &ReplyReference::new("msg-1").with_to("alice@example.com".parse().expect("valid jid")),
    );
    assert_eq!(with_to.name(), "reply");
    assert_eq!(with_to.ns(), NS_REPLY);
    assert_eq!(with_to.attr("id"), Some("msg-1"));
    assert_eq!(with_to.attr("to"), Some("alice@example.com"));

    let without_to = build_reply_element(&ReplyReference::new("msg-2"));
    assert_eq!(without_to.attr("id"), Some("msg-2"));
    assert_eq!(
        without_to.attr("to"),
        None,
        "optional `to` MUST be omitted when not set, not stringified as empty"
    );
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0461_parser_rejects_payload_without_id() {
    // §3 makes `id` REQUIRED. An attacker-supplied `<reply to='…'/>`
    // without `id` is malformed and MUST NOT produce a reply
    // reference (which the routing layer would then key into the
    // wrong message id).
    let xml = "<message xmlns='jabber:client'>\
                  <reply xmlns='urn:xmpp:reply:0' to='alice@example.com'/>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");
    assert!(parse_reply_from_message(&msg).is_none());
}

#[test]
fn xep0461_parser_rejects_payload_with_empty_id() {
    // `id=""` is not a valid message id; treating it as such would
    // route replies into a phantom thread.
    let xml = "<message xmlns='jabber:client'>\
                  <reply xmlns='urn:xmpp:reply:0' id='' to='alice@example.com'/>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");
    assert!(parse_reply_from_message(&msg).is_none());
}

#[test]
fn xep0461_parser_keeps_id_drops_malformed_to() {
    // §3 makes `to` optional and JID-typed. A non-parseable `to`
    // must not poison the entire reply — keep the well-formed `id`
    // and just drop `to`. (Hard ingress validation lives elsewhere;
    // this helper is the "best effort to surface a usable reply"
    // boundary.)
    let xml = "<message xmlns='jabber:client'>\
                  <reply xmlns='urn:xmpp:reply:0' id='msg-1' to=' '/>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");
    let reply = parse_reply_from_message(&msg).expect("id-only reply still surfaces");
    assert_eq!(reply.id, "msg-1");
    assert_eq!(reply.to, None);
}

#[test]
fn xep0461_set_reply_replaces_existing_reply() {
    // §"Editing" semantics: there is one reply reference per
    // message. The mutator must replace, never duplicate, otherwise
    // routing has to choose between conflicting reply ids.
    let xml = "<message xmlns='jabber:client'>\
                  <reply xmlns='urn:xmpp:reply:0' id='old'/>\
               </message>";
    let mut msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");

    set_reply_payload(
        &mut msg,
        &ReplyReference::new("new-id").with_to("bob@example.com".parse().expect("jid")),
    );

    let reply = parse_reply_from_message(&msg).expect("reply present");
    assert_eq!(reply.id, "new-id");
    assert_eq!(
        msg.payloads
            .iter()
            .filter(|elem| is_reply_element(elem))
            .count(),
        1,
        "exactly one reply payload survives the set; no duplicates"
    );
}

/// XEP-0461 conformance of the ingress semantic-digest boundary (#1650),
/// in this XEP's dedicated suite per the repo test rule: strict reply
/// parsing with original id bytes preserved (the archive paths retain and
/// replay the raw attribute) and whitespace-only ids rejected.
mod ingress_digest_boundary {
    use minidom::{rxml::xml_ncname, Element};
    use waddle_xmpp::ingress::{DigestContext, DigestInput, DigestInputError, NormalizedTarget};
    use waddle_xmpp::xep::xep0461::NS_REPLY;
    use xmpp_parsers::message::Message;

    fn context() -> DigestContext {
        DigestContext {
            target: NormalizedTarget::Absent,
            server_authorities: Vec::new(),
            stanza_lang: None,
        }
    }

    fn with_reply(id: &str) -> Message {
        Message::normal(None).with_payloads(vec![Element::builder("reply", NS_REPLY)
            .attr(xml_ncname!("id").to_owned(), id)
            .build()])
    }

    #[test]
    fn reply_id_bytes_are_preserved_like_the_archive_paths() {
        let parsed =
            DigestInput::from_parsed(&with_reply("  r-9  "), &context()).expect("valid input");
        assert_eq!(
            parsed.reply().map(|reply| reply.id.as_str()),
            Some("  r-9  "),
            "archive consumers retain the raw reply id; the digest boundary must too"
        );
    }

    #[test]
    fn whitespace_only_reply_id_is_rejected() {
        assert!(matches!(
            DigestInput::from_parsed(&with_reply("   "), &context()),
            Err(DigestInputError::ReplyMalformed)
        ));
    }

    #[test]
    fn duplicate_reply_elements_are_rejected() {
        let message = Message::normal(None).with_payloads(vec![
            Element::builder("reply", NS_REPLY)
                .attr(xml_ncname!("id").to_owned(), "a")
                .build(),
            Element::builder("reply", NS_REPLY)
                .attr(xml_ncname!("id").to_owned(), "b")
                .build(),
        ]);
        assert!(matches!(
            DigestInput::from_parsed(&message, &context()),
            Err(DigestInputError::DuplicateReply)
        ));
    }
}
