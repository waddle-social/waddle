//! XEP-0513: Explicit Mentions — dedicated conformance suite.
//!
//! Pins the audit-level invariants at the public API:
//!
//! - the namespace string `urn:xmpp:mentions:0` and the channel
//!   mention URI `urn:xmpp:mentions:0#channel`,
//! - disco advertisement on every MUC room configuration plus the
//!   distinct `…#channel` feature (the spec carves channel-mention
//!   support out as its own URI so non-supporting servers can
//!   refuse to broadcast `@channel`-style pings without dropping
//!   per-occupant mentions),
//! - the `<mention>` wire shape: builder attributes (begin/end,
//!   jid, occupantid, mentions, uri) plus the `<active/>` and
//!   `<noping/>` children,
//! - parser robustness: payloads with no mention-identifying
//!   attribute are dropped (would otherwise create phantom
//!   mentions); `jid=""` and malformed JID strings don't panic.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, Feature};
use waddle_xmpp::xep::xep0513::{
    build_mention_element, extract_explicit_mentions, has_explicit_mentions, is_mention_element,
    parse_mention_element, set_explicit_mentions, strip_explicit_mentions, ExplicitMention,
    ExplicitMentionCarrier, ExplicitMentions, CHANNEL_MENTION, NS_EXPLICIT_MENTIONS,
};
use xmpp_parsers::message::Message;

// ── Namespace + spec URIs ───────────────────────────────────────────

#[test]
fn xep0513_namespace_constants_match_spec() {
    assert_eq!(NS_EXPLICIT_MENTIONS, "urn:xmpp:mentions:0");
    assert_eq!(CHANNEL_MENTION, "urn:xmpp:mentions:0#channel");
}

// ── §"Discovering support" advertisement ────────────────────────────

#[test]
fn xep0513_muc_rooms_advertise_both_mention_features() {
    let mentions = Feature::explicit_mentions();
    let channel = Feature::channel_mentions();

    // Defence-in-depth string pinning so a constructor rename
    // doesn't silently change the wire.
    assert_eq!(mentions.0, "urn:xmpp:mentions:0");
    assert_eq!(channel.0, "urn:xmpp:mentions:0#channel");

    // The spec carves channel-mention support out as its own URI:
    // a server that supports per-occupant mentions but rejects
    // `@channel`-style pings advertises only the base feature.
    // Waddle accepts both — pin both adverts on the room surface
    // across every (persistent × members_only × moderated × forum)
    // configuration. XEP-0513 is MUC-focused, so the dedicated
    // advert lives on the room disco surface; server-wide disco
    // doesn't currently surface it (DMs / 1:1 chats don't broadcast
    // `@channel`, and per-recipient mentions are inferred from
    // body without disco gating).
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &mentions),
                        "muc_room_features({persistent}, {members_only}, {moderated}, {forum}) \
                         MUST advertise `urn:xmpp:mentions:0`"
                    );
                    assert!(
                        feats.iter().any(|f| f == &channel),
                        "muc_room_features({persistent}, {members_only}, {moderated}, {forum}) \
                         MUST advertise `urn:xmpp:mentions:0#channel`"
                    );
                }
            }
        }
    }
}

// ── §3 wire shape ────────────────────────────────────────────────────

#[test]
fn xep0513_classifier_accepts_spec_shape_only() {
    let canonical = Element::builder("mention", NS_EXPLICIT_MENTIONS).build();
    assert!(is_mention_element(&canonical));

    let wrong_ns = Element::builder("mention", "wrong:ns").build();
    assert!(!is_mention_element(&wrong_ns));

    let wrong_name = Element::builder("mentions", NS_EXPLICIT_MENTIONS).build();
    assert!(!is_mention_element(&wrong_name));
}

#[test]
fn xep0513_jid_mention_round_trip_preserves_all_attributes() {
    let mention = ExplicitMention {
        begin: Some(5),
        end: Some(11),
        jid: Some("alice@example.com".parse().expect("valid jid")),
        occupant_id: Some("opaque-occupant-id".to_owned()),
        mentions: None,
        uri: Some("xmpp:alice@example.com".to_owned()),
        active: false,
        noping: false,
    };
    let elem = build_mention_element(&mention);
    assert_eq!(elem.name(), "mention");
    assert_eq!(elem.ns(), NS_EXPLICIT_MENTIONS);
    assert_eq!(elem.attr("begin"), Some("5"));
    assert_eq!(elem.attr("end"), Some("11"));
    assert_eq!(elem.attr("jid"), Some("alice@example.com"));
    assert_eq!(elem.attr("occupantid"), Some("opaque-occupant-id"));
    assert_eq!(elem.attr("uri"), Some("xmpp:alice@example.com"));
    // `mentions` attr is for the channel URI; absent on a per-JID
    // mention.
    assert!(elem.attr("mentions").is_none());

    let parsed = parse_mention_element(&elem).expect("mention round-trips");
    assert_eq!(parsed.begin, Some(5));
    assert_eq!(parsed.end, Some(11));
    assert_eq!(
        parsed.jid.as_ref().map(ToString::to_string).as_deref(),
        Some("alice@example.com")
    );
    assert_eq!(parsed.occupant_id.as_deref(), Some("opaque-occupant-id"));
    assert_eq!(parsed.uri.as_deref(), Some("xmpp:alice@example.com"));
    assert!(!parsed.active);
    assert!(!parsed.noping);
}

#[test]
fn xep0513_channel_mention_uses_dedicated_uri() {
    // The channel `@channel`-style mention is identified by the
    // `mentions="urn:xmpp:mentions:0#channel"` attribute, NOT by a
    // JID. Building via the `channel()` helper MUST set that URI.
    let elem = build_mention_element(&ExplicitMention::channel());
    assert_eq!(elem.attr("mentions"), Some(CHANNEL_MENTION));
    assert!(
        elem.attr("jid").is_none(),
        "channel mention MUST NOT carry a JID"
    );

    let parsed = parse_mention_element(&elem).expect("channel mention parses");
    assert!(parsed.is_channel());
    assert!(!parsed.is_individual());
}

#[test]
fn xep0513_active_and_noping_emit_child_elements_not_attributes() {
    // §3.1 / §3.2: `<active/>` and `<noping/>` are CHILD elements
    // (not attributes) so the wire shape carries clear booleans.
    // The builder must put them under `<mention>`, namespaced
    // under XEP-0513.
    let elem = build_mention_element(&ExplicitMention {
        active: true,
        noping: true,
        ..ExplicitMention::channel()
    });

    let active = elem
        .children()
        .find(|c| c.name() == "active" && c.ns() == NS_EXPLICIT_MENTIONS);
    assert!(active.is_some(), "<active/> child MUST be present");

    let noping = elem
        .children()
        .find(|c| c.name() == "noping" && c.ns() == NS_EXPLICIT_MENTIONS);
    assert!(noping.is_some(), "<noping/> child MUST be present");

    let parsed = parse_mention_element(&elem).expect("parses");
    assert!(parsed.active);
    assert!(parsed.noping);
}

// ── Carrier-trait surface + collection helpers ──────────────────────

#[test]
fn xep0513_set_explicit_mentions_replaces_prior_payloads() {
    // The collection mutator: each call replaces every prior
    // namespaced payload. Otherwise repeated edits would
    // accumulate duplicate mentions on the same message.
    let mut msg = Message::new(None::<jid::Jid>);
    set_explicit_mentions(
        &mut msg,
        &ExplicitMentions::new().with_mention(ExplicitMention::jid(
            "alice@example.com".parse().expect("jid"),
        )),
    );
    set_explicit_mentions(&mut msg, &ExplicitMentions::new().with_active_channel());

    let parsed = extract_explicit_mentions(&msg).expect("mentions present");
    assert_eq!(parsed.mentions.len(), 1, "exactly one mention survives");
    assert!(parsed.has_channel());
    assert!(parsed.mentions[0].active);
}

#[test]
fn xep0513_strip_clears_every_namespaced_payload() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_explicit_mentions(
        &mut msg,
        &ExplicitMentions::new()
            .with_mention(ExplicitMention::jid(
                "alice@example.com".parse().expect("jid"),
            ))
            .with_channel(),
    );
    strip_explicit_mentions(&mut msg);
    assert!(!has_explicit_mentions(&msg));
    assert!(extract_explicit_mentions(&msg).is_none());
}

#[test]
fn xep0513_carrier_trait_surfaces_mentions_via_explicit_mentions() {
    let mut msg = Message::new(None::<jid::Jid>);
    let jid: BareJid = "bob@example.com".parse().expect("jid");
    set_explicit_mentions(
        &mut msg,
        &ExplicitMentions::new().with_mention(ExplicitMention::jid(jid.clone())),
    );

    let surfaced = msg.explicit_mentions().expect("trait surfaces mentions");
    assert!(surfaced.mentions_jid(&jid));
    assert!(msg.has_explicit_mentions());
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0513_parse_drops_mention_with_no_identifying_attribute() {
    // A `<mention begin="5" end="11"/>` with no jid, occupantid,
    // mentions, uri, active, or noping is a structural shell —
    // not a real mention. The parser MUST reject it; otherwise
    // every message with a stray empty `<mention/>` would falsely
    // claim to mention someone (downstream notification logic
    // would then page the wrong person, or noone).
    let stub = Element::builder("mention", NS_EXPLICIT_MENTIONS)
        .attr("begin", "5")
        .attr("end", "11")
        .build();
    assert!(parse_mention_element(&stub).is_none());
}

#[test]
fn xep0513_parse_drops_invalid_jid_attribute_silently() {
    // BareJid-unparseable strings must drop to None for the jid
    // field without poisoning the surrounding mention. If the
    // mention also has another identifying attribute (e.g.
    // `mentions=`), it still surfaces. `@@@` is unambiguous garbage
    // — RFC 7622 / PRECIS reject multiple `@`s in a bare JID, so
    // the parse MUST fail.
    let elem = Element::builder("mention", NS_EXPLICIT_MENTIONS)
        .attr("jid", "@@@")
        .attr("mentions", CHANNEL_MENTION)
        .build();
    let parsed = parse_mention_element(&elem).expect("channel-mentions identity survives");
    assert!(parsed.jid.is_none(), "malformed jid dropped to None");
    assert!(parsed.is_channel());
}

#[test]
fn xep0513_parse_drops_malformed_numeric_offsets() {
    // `begin="three"` MUST drop to None rather than panic or
    // coerce to 0 (a 0-offset would make the mention point at the
    // wrong character range — silent corruption of the rendered
    // span).
    let elem = Element::builder("mention", NS_EXPLICIT_MENTIONS)
        .attr("begin", "three")
        .attr("end", "")
        .attr("jid", "alice@example.com")
        .build();
    let parsed = parse_mention_element(&elem).expect("identifies via jid");
    assert_eq!(parsed.begin, None);
    assert_eq!(parsed.end, None);
}

#[test]
fn xep0513_extract_returns_none_when_no_mention_payloads() {
    let msg = Message::new(None::<jid::Jid>);
    assert!(!has_explicit_mentions(&msg));
    assert!(extract_explicit_mentions(&msg).is_none());
}
