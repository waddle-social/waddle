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
//! - the XEP-0513 wire shape: `<mention type target active/>` inside an
//!   XEP-0394 `<markup><span start end>` wrapper, with Waddle-only data
//!   (jid, uri, noping) in `urn:waddle:mentions:0` and nothing extra in
//!   the official namespace,
//! - parser robustness: mentions without a `type` are dropped (would
//!   otherwise create phantom mentions); malformed JID strings and
//!   offsets don't panic.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, Feature};
use waddle_xmpp::xep::xep0513::{
    build_mention_markup, extract_explicit_mentions, has_explicit_mentions,
    has_malformed_mention_markup, parse_mention_markup, set_explicit_mentions,
    strip_explicit_mentions, ExplicitMention, ExplicitMentionCarrier, ExplicitMentions,
    CHANNEL_MENTION, NS_EXPLICIT_MENTIONS, NS_WADDLE_MENTIONS, OCCUPANT_MENTION,
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
fn xep0513_muc_rooms_advertise_mentions_and_channel_mentions() {
    let mentions = Feature::explicit_mentions();
    let channel = Feature::channel_mentions();

    // Defence-in-depth string pinning so a constructor rename
    // doesn't silently change the wire.
    assert_eq!(mentions.0, "urn:xmpp:mentions:0");
    assert_eq!(channel.0, "urn:xmpp:mentions:0#channel");

    // XEP-0513 §292 + §303: advertising `urn:xmpp:mentions:0` and
    // `urn:xmpp:mentions:0#channel` is binding — once advertised, the
    // room's `<query xmlns='urn:xmpp:mentions:0'/>` IQ MUST return a
    // form with `mentions#count` + `mentions#individual` (always
    // required) and `mentions#channel` (if and only if `#channel` is
    // advertised). Slices 3a/3b enforce a hardcoded server policy at
    // T0 candidate classification; slice 3c (#525) wires the §295 IQ
    // surface and re-advertises both namespaces — both arrive
    // together to satisfy CLAUDE.md's XEP conformance hard rule.
    // The advertisement-set is independent of the muc#roomconfig
    // axes (persistent / members_only / moderated / forum), so we
    // pin every combination.
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(
                        waddle_xmpp_core::disco::info::RoomPersistence::from_persistent_flag(
                            persistent,
                        ),
                        waddle_xmpp_core::disco::info::RoomMembership::from_members_only_flag(
                            members_only,
                        ),
                        waddle_xmpp_core::disco::info::RoomVisibility::Public,
                        waddle_xmpp_core::types::Moderation::from_moderated_flag(moderated),
                    );
                    assert!(
                        feats.iter().any(|f| f == &mentions),
                        "muc_room_features({persistent}, {members_only}, {moderated}, {forum}) \
                         MUST advertise `urn:xmpp:mentions:0` — slice 3c wires §295 IQ"
                    );
                    assert!(
                        feats.iter().any(|f| f == &channel),
                        "muc_room_features({persistent}, {members_only}, {moderated}, {forum}) \
                         MUST advertise `urn:xmpp:mentions:0#channel` — slice 3c exposes \
                         §303 `mentions#channel`"
                    );
                }
            }
        }
    }
}

// ── Wire shape: XEP-0394 markup/span wrapper ────────────────────────

const NS_MARKUP: &str = "urn:xmpp:markup:0";
const OCCUPANT_TYPE: &str = "urn:xmpp:mentions:0#occupant";

fn parse_xml(xml: &str) -> Element {
    xml.parse().expect("well-formed test XML")
}

fn single_span(markup: &Element) -> &Element {
    assert!(
        markup.is("markup", NS_MARKUP),
        "root is <markup/>: {markup:?}"
    );
    let mut spans = markup.children();
    let span = spans.next().expect("one span");
    assert!(spans.next().is_none(), "exactly one span");
    assert!(span.is("span", NS_MARKUP));
    span
}

#[test]
fn xep0513_namespace_constants_for_wire_shape() {
    assert_eq!(NS_WADDLE_MENTIONS, "urn:waddle:mentions:0");
    assert_eq!(OCCUPANT_MENTION, OCCUPANT_TYPE);
}

#[test]
fn xep0513_occupant_mention_matches_spec_example() {
    // XEP-0513 "Mentioning a user in a room": the <mention/> is a child
    // of an XEP-0394 <span start end> inside <markup>, with a required
    // `type` and the XEP-0421 occupant-id as `target`.
    let markup = build_mention_markup(&ExplicitMention {
        begin: Some(7),
        end: Some(12),
        ..ExplicitMention::occupant_id("dd72603deec90a38ba552f7c68cbcc61bca202cd")
    });
    let expected = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'>\
           <span start='7' end='12'>\
             <mention xmlns='urn:xmpp:mentions:0' \
               type='urn:xmpp:mentions:0#occupant' \
               target='dd72603deec90a38ba552f7c68cbcc61bca202cd'/>\
           </span>\
         </markup>",
    );
    assert_eq!(markup, expected);

    let parsed = parse_mention_markup(&markup);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].begin, Some(7));
    assert_eq!(parsed[0].end, Some(12));
    assert_eq!(
        parsed[0].occupant_id.as_deref(),
        Some("dd72603deec90a38ba552f7c68cbcc61bca202cd")
    );
}

#[test]
fn xep0513_channel_mention_has_type_and_no_target() {
    // "@room": type '#channel', and 'target' MUST NOT be specified.
    let markup = build_mention_markup(&ExplicitMention {
        begin: Some(5),
        end: Some(10),
        ..ExplicitMention::channel()
    });
    let expected = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'>\
           <span start='5' end='10'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel'/>\
           </span>\
         </markup>",
    );
    assert_eq!(markup, expected);

    let parsed = parse_mention_markup(&markup);
    assert_eq!(parsed.len(), 1);
    assert!(parsed[0].is_channel());
    assert!(!parsed[0].is_individual());
}

#[test]
fn xep0513_active_is_a_boolean_attribute() {
    // "@here": active='true' on the <mention/>.
    let markup = build_mention_markup(&ExplicitMention {
        begin: Some(0),
        end: Some(5),
        ..ExplicitMention::active_channel()
    });
    let expected = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'>\
           <span start='0' end='5'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel' \
               active='true'/>\
           </span>\
         </markup>",
    );
    assert_eq!(markup, expected);
    assert!(parse_mention_markup(&markup)[0].active);
}

#[test]
fn xep0513_official_namespace_carries_only_spec_attributes() {
    // jid / uri / noping are Waddle-only: they MUST live in
    // urn:waddle:mentions:0, never as attributes or children of the
    // official <mention/>.
    let mention = ExplicitMention {
        begin: Some(5),
        end: Some(11),
        jid: Some("alice@example.com".parse().expect("valid jid")),
        occupant_id: Some("opaque-occupant-id".to_owned()),
        mentions: None,
        uri: Some("xmpp:alice@example.com".to_owned()),
        active: false,
        noping: true,
    };
    let markup = build_mention_markup(&mention);
    let span = single_span(&markup);
    assert_eq!(span.attr("start"), Some("5"));
    assert_eq!(span.attr("end"), Some("11"));

    let official = span
        .get_child("mention", NS_EXPLICIT_MENTIONS)
        .expect("XEP-0513 mention");
    assert_eq!(
        official.attrs().into_iter().count(),
        2,
        "only type and target"
    );
    assert_eq!(official.attr("type"), Some(OCCUPANT_TYPE));
    assert_eq!(official.attr("target"), Some("opaque-occupant-id"));
    assert_eq!(official.children().count(), 0);

    let extension = span
        .get_child("mention", NS_WADDLE_MENTIONS)
        .expect("Waddle extension");
    assert_eq!(extension.attr("jid"), Some("alice@example.com"));
    assert_eq!(extension.attr("uri"), Some("xmpp:alice@example.com"));
    assert_eq!(extension.attr("noping"), Some("true"));

    // Whole-mention round-trip.
    let parsed = parse_mention_markup(&markup);
    assert_eq!(parsed, vec![mention]);
}

#[test]
fn xep0513_jid_only_mention_has_no_official_element() {
    // XEP-0513 defines no JID target, so a JID-only mention emits no
    // element in the official namespace.
    let markup = build_mention_markup(&ExplicitMention {
        begin: Some(0),
        end: Some(5),
        ..ExplicitMention::jid("alice@example.com".parse().expect("jid"))
    });
    let span = single_span(&markup);
    assert!(span.get_child("mention", NS_EXPLICIT_MENTIONS).is_none());
    assert!(span.get_child("mention", NS_WADDLE_MENTIONS).is_some());
    let parsed = parse_mention_markup(&markup);
    assert_eq!(parsed.len(), 1);
    assert_eq!(
        parsed[0].jid.as_ref().map(ToString::to_string).as_deref(),
        Some("alice@example.com")
    );
}

#[test]
fn xep0513_group_type_wins_over_occupant_target() {
    // A group mention MUST NOT carry a target; a mention holding both a
    // group URI and an occupant-id serializes as the group mention only.
    let mention = ExplicitMention {
        mentions: Some(CHANNEL_MENTION.to_owned()),
        ..ExplicitMention::occupant_id("occ-1")
    };
    let markup = build_mention_markup(&mention);
    let official = single_span(&markup)
        .get_child("mention", NS_EXPLICIT_MENTIONS)
        .expect("official mention");
    assert_eq!(official.attr("type"), Some(CHANNEL_MENTION));
    assert_eq!(official.attr("target"), None);
}

#[test]
fn xep0513_multiple_mentions_share_one_markup_element() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_explicit_mentions(
        &mut msg,
        &ExplicitMentions::new()
            .with_mention(ExplicitMention {
                begin: Some(0),
                end: Some(3),
                ..ExplicitMention::occupant_id("a")
            })
            .with_mention(ExplicitMention {
                begin: Some(4),
                end: Some(9),
                ..ExplicitMention::channel()
            }),
    );
    assert_eq!(msg.payloads.len(), 1, "one <markup/> payload");
    assert_eq!(msg.payloads[0].children().count(), 2, "two spans");
    let parsed = extract_explicit_mentions(&msg).expect("mentions");
    assert_eq!(parsed.mentions.len(), 2);
    assert_eq!(parsed.mentions[0].occupant_id.as_deref(), Some("a"));
    assert!(parsed.mentions[1].is_channel());
}

#[test]
fn xep0513_several_mentions_in_one_span_all_parse() {
    // XEP-0513: "Multiple mentions may be contained in the same <span />".
    let markup = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><span start='3' end='14'>\
           <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' \
             target='dd72603deec90a38ba552f7c68cbcc61bca202cd'/>\
           <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' \
             target='3deec90a38ba552f7c68cbcc61bca202cddd7260'/>\
         </span></markup>",
    );
    let parsed = parse_mention_markup(&markup);
    assert_eq!(parsed.len(), 2);
    assert!(parsed
        .iter()
        .all(|m| m.begin == Some(3) && m.end == Some(14)));
}

#[test]
fn xep0513_role_and_hat_types_parse_as_group_mentions() {
    let markup = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><span start='4' end='13'>\
           <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#moderators'/>\
         </span></markup>",
    );
    let parsed = parse_mention_markup(&markup);
    assert_eq!(
        parsed[0].mentions.as_deref(),
        Some("urn:xmpp:mentions:0#moderators")
    );
    assert!(!parsed[0].is_channel());
}

#[test]
fn xep0513_legacy_bare_mention_payload_is_not_a_mention() {
    // The pre-markup shape (a top-level <mention begin end occupantid/>
    // payload) is not XEP-0513 and is no longer recognised.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(parse_xml(
        "<mention xmlns='urn:xmpp:mentions:0' begin='0' end='6' occupantid='x'/>",
    ));
    assert!(!has_explicit_mentions(&msg));
    assert!(extract_explicit_mentions(&msg).is_none());
}

// ── Carrier-trait surface + collection helpers ──────────────────────

#[test]
fn xep0513_set_explicit_mentions_replaces_prior_payloads() {
    // The collection mutator: each call replaces every prior
    // mention. Otherwise repeated edits would accumulate duplicate
    // mentions on the same message.
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
fn xep0513_strip_clears_mentions_and_keeps_other_markup() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><bquote start='0' end='4'/></markup>",
    ));
    set_explicit_mentions(
        &mut msg,
        &ExplicitMentions::new()
            .with_mention(ExplicitMention::jid(
                "alice@example.com".parse().expect("jid"),
            ))
            .with_channel(),
    );
    assert_eq!(msg.payloads.len(), 1, "mentions join the existing markup");
    strip_explicit_mentions(&mut msg);
    assert!(!has_explicit_mentions(&msg));
    assert!(extract_explicit_mentions(&msg).is_none());
    assert_eq!(msg.payloads.len(), 1, "blockquote markup is preserved");
    assert!(msg.payloads[0].get_child("bquote", NS_MARKUP).is_some());

    // A markup element holding only mentions disappears entirely.
    let mut only = Message::new(None::<jid::Jid>);
    set_explicit_mentions(&mut only, &ExplicitMentions::new().with_channel());
    strip_explicit_mentions(&mut only);
    assert!(only.payloads.is_empty());
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
fn xep0513_mention_without_type_is_dropped_and_malformed() {
    // `type` is REQUIRED. A <mention/> without it is not interpretable;
    // the parser drops it and message validation flags it.
    let markup = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><span start='5' end='11'>\
           <mention xmlns='urn:xmpp:mentions:0' target='x'/>\
         </span></markup>",
    );
    assert!(parse_mention_markup(&markup).is_empty());
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(markup);
    assert!(has_malformed_mention_markup(&msg));
}

#[test]
fn xep0513_occupant_mention_without_target_is_malformed() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><span start='5' end='11'>\
           <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant'/>\
         </span></markup>",
    ));
    assert!(has_malformed_mention_markup(&msg));

    let mut ok = Message::new(None::<jid::Jid>);
    set_explicit_mentions(&mut ok, &ExplicitMentions::new().with_channel());
    assert!(!has_malformed_mention_markup(&ok));
}

#[test]
fn xep0513_waddle_jid_extension_is_rejected_by_validation() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads
        .push(build_mention_markup(&ExplicitMention::jid(
            "alice@example.com".parse().expect("jid"),
        )));
    assert!(has_malformed_mention_markup(&msg));
}

#[test]
fn xep0513_parse_drops_invalid_jid_attribute_silently() {
    // A BareJid-unparseable string drops to None for the jid field
    // without poisoning the surrounding mention.
    let markup = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='3'>\
           <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel'/>\
           <mention xmlns='urn:waddle:mentions:0' jid='@@@'/>\
         </span></markup>",
    );
    let parsed = parse_mention_markup(&markup);
    assert_eq!(parsed.len(), 1);
    assert!(parsed[0].jid.is_none(), "malformed jid dropped to None");
    assert!(parsed[0].is_channel());
}

#[test]
fn xep0513_parse_drops_malformed_numeric_offsets() {
    // `start="three"` MUST drop to None rather than panic or coerce
    // to 0 (which would point the mention at the wrong range).
    let markup = parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><span start='three' end=''>\
           <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel'/>\
         </span></markup>",
    );
    let parsed = parse_mention_markup(&markup);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].begin, None);
    assert_eq!(parsed[0].end, None);
}

#[test]
fn xep0513_span_without_mentions_is_not_a_mention() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(parse_xml(
        "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='3'><emphasis/></span></markup>",
    ));
    assert!(!has_explicit_mentions(&msg));
    assert!(extract_explicit_mentions(&msg).is_none());
}

#[test]
fn xep0513_extract_returns_none_when_no_mention_payloads() {
    let msg = Message::new(None::<jid::Jid>);
    assert!(!has_explicit_mentions(&msg));
    assert!(extract_explicit_mentions(&msg).is_none());
}

// ── Who may broadcast a channel mention ─────────────────────────────

/// The `sender_can_broadcast_channel_mention` flag of every inbox
/// projection the room chain emits for a groupchat from a moderator-role
/// occupant, with `decision` passed in as the interpreter's pre-dispatch
/// decision.
fn channel_mention_flags(decision: Option<bool>) -> Vec<bool> {
    use waddle_xmpp::protocol::event::OutboundEvent;
    use waddle_xmpp::protocol::id_gen::FixedIdGenerator;
    use waddle_xmpp::protocol::room::context::{OccupantSnapshot, RoomContext};
    use waddle_xmpp::protocol::room::inbox::MucInboxHandler;
    use waddle_xmpp::protocol::room::traits::{RoomHandler, RoomHandlerOutcome};
    use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};
    use waddle_xmpp_core::types::{Affiliation, Role};

    let room: BareJid = "team@conference.example.com".parse().expect("room");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
    let occupants = vec![OccupantSnapshot {
        full_jid: sender.clone(),
        nick: "alice".to_string(),
        affiliation: Affiliation::Member,
        role: Role::Moderator,
    }];
    let durable = vec!["bob@example.com".parse::<BareJid>().expect("recipient")];
    let id_gen = FixedIdGenerator("fixed-stanza-id".to_string());
    let secret =
        OccupantIdSecret::try_new(vec![7u8; OCCUPANT_ID_SECRET_MIN_BYTES]).expect("secret");
    let ctx = RoomContext {
        room: &room,
        sender_full: &sender,
        occupants: &occupants,
        durable_recipient_bare_jids: &durable,
        mention_recipients: waddle_xmpp::protocol::room::MentionRecipients::Empty,
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: true,
        pin_permission: waddle_xmpp::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        channel_mention_decision: decision,
        dispatch_timestamp: 0,
    };
    let mut message = Message::new(Some(jid::Jid::from(room.clone())));
    message.from = Some(jid::Jid::from(sender.clone()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello all".to_string());
    set_explicit_mentions(
        &mut message,
        &ExplicitMentions::new().with_mention(ExplicitMention::channel()),
    );
    let RoomHandlerOutcome::Continue(events) = MucInboxHandler.handle(&mut message, &ctx) else {
        panic!("the inbox projection halted the chain");
    };
    events
        .into_iter()
        .filter_map(|event| match event {
            OutboundEvent::ProjectGroupchatInbox {
                sender_can_broadcast_channel_mention,
                ..
            } => Some(sender_can_broadcast_channel_mention),
            _ => None,
        })
        .collect()
}

/// §"Multi-User Chats Permissions": the room decides who may broadcast a
/// channel mention. Without a decision of the server's own, a
/// moderator-role sender may. Where the server decided it before dispatch
/// (in a room whose grants are held outside the room's own roles), that
/// decision applies to every projection, whatever role the room seats the
/// sender with.
#[test]
fn xep0513_a_servers_decision_on_channel_mentions_overrides_the_seated_role() {
    let by_role = channel_mention_flags(None);
    assert_eq!(by_role, vec![true, true], "role-decided projections");
    let refused = channel_mention_flags(Some(false));
    assert_eq!(refused, vec![false, false], "server-refused projections");
}
