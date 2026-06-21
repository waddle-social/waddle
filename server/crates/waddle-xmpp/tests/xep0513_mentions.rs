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
use waddle_xmpp::xep::xep0004::NS_DATA_FORMS;
use waddle_xmpp::xep::xep0513::{
    build_mention_element, build_mentions_permissions_query, extract_explicit_mentions,
    has_explicit_mentions, is_mention_element, is_mentions_permissions_query,
    parse_mention_element, set_explicit_mentions, strip_explicit_mentions, ExplicitMention,
    ExplicitMentionCarrier, ExplicitMentions, MentionsPermission, MentionsPermissions,
    CHANNEL_MENTION, DEFAULT_MENTIONS_COUNT, FIELD_MENTIONS_CHANNEL, FIELD_MENTIONS_COUNT,
    FIELD_MENTIONS_INDIVIDUAL, NS_EXPLICIT_MENTIONS,
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
                    let feats = muc_room_features(persistent, members_only, true, moderated, forum);
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

// ── §295 / §303: Permissions IQ form ────────────────────────────────

#[test]
fn xep0513_mentions_permissions_query_recogniser_pins_name_and_ns() {
    let canonical = Element::builder("query", NS_EXPLICIT_MENTIONS).build();
    assert!(is_mentions_permissions_query(&canonical));

    // §295 IQ payload uses element name `query` (not `permissions`,
    // not `mentions`) — pin both axes so a constructor or namespace
    // typo can't silently mis-route.
    let wrong_ns = Element::builder("query", "urn:xmpp:mentions:1").build();
    assert!(!is_mentions_permissions_query(&wrong_ns));

    let wrong_name = Element::builder("permissions", NS_EXPLICIT_MENTIONS).build();
    assert!(!is_mentions_permissions_query(&wrong_name));

    // The `<mention>` payload from §3 lives in the same namespace —
    // make sure the §295 recogniser does NOT match a `<mention>` so
    // an IQ carrying a stray `<mention/>` can't be answered as a
    // permissions query.
    let mention = Element::builder("mention", NS_EXPLICIT_MENTIONS).build();
    assert!(!is_mentions_permissions_query(&mention));
}

#[test]
fn xep0513_mention_permission_wire_values_and_labels_match_spec() {
    // §303 form definition: option values are `participants`,
    // `moderators`, `none`; the matching labels are `Participants`,
    // `Moderators Only`, `Nobody`. Pin both — a single-character
    // drift on the wire would silently desync the form's `<value/>`
    // from the option `<value/>` and clients would reject the
    // submission.
    assert_eq!(MentionsPermission::Participants.as_wire(), "participants");
    assert_eq!(MentionsPermission::Moderators.as_wire(), "moderators");
    assert_eq!(MentionsPermission::Nobody.as_wire(), "none");

    assert_eq!(MentionsPermission::Participants.label(), "Participants");
    assert_eq!(MentionsPermission::Moderators.label(), "Moderators Only");
    assert_eq!(MentionsPermission::Nobody.label(), "Nobody");
}

#[test]
fn xep0513_default_permissions_match_server_policy() {
    let policy = MentionsPermissions::server_default();
    // §301 example value for the count threshold; mirrored in the
    // T0 classification gate from slice 3b (PR #741).
    assert_eq!(policy.count, DEFAULT_MENTIONS_COUNT);
    assert_eq!(policy.count, 5);
    // Slice 3a (PR #738) hardcodes `mentions#channel = moderators`.
    assert_eq!(policy.channel, Some(MentionsPermission::Moderators));
    // Individual mentions are open to participants — there is no
    // per-recipient sender gate at T0 today.
    assert_eq!(policy.individual, MentionsPermission::Participants);
}

#[test]
fn xep0513_permissions_form_matches_spec_shape() {
    let policy = MentionsPermissions::server_default();
    let query = build_mentions_permissions_query(&policy);

    // §303: payload is `<query xmlns='urn:xmpp:mentions:0'>` wrapping
    // a `<x xmlns='jabber:x:data' type='form'/>`.
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), NS_EXPLICIT_MENTIONS);
    let form = query
        .get_child("x", NS_DATA_FORMS)
        .expect("§303 form is a jabber:x:data child of <query/>");
    assert_eq!(
        form.attr("type"),
        Some("form"),
        "§303 form `type` MUST be `form` (server publishing config to client)"
    );

    // FORM_TYPE hidden field with NS value — required by XEP-0068 /
    // §303 form pinning so submit payloads round-trip the type.
    let form_type_field = form
        .children()
        .filter(|c| c.is("field", NS_DATA_FORMS))
        .find(|c| c.attr("var") == Some("FORM_TYPE"))
        .expect("FORM_TYPE field is present");
    assert_eq!(form_type_field.attr("type"), Some("hidden"));
    let form_type_value = form_type_field
        .get_child("value", NS_DATA_FORMS)
        .expect("FORM_TYPE has a value child")
        .text();
    assert_eq!(form_type_value, NS_EXPLICIT_MENTIONS);

    // Required fields — §303: "the `mentions#count` and
    // `mentions#individual` fields MUST be present at minimum."
    for var in [FIELD_MENTIONS_COUNT, FIELD_MENTIONS_INDIVIDUAL] {
        let field = form
            .children()
            .filter(|c| c.is("field", NS_DATA_FORMS))
            .find(|c| c.attr("var") == Some(var))
            .unwrap_or_else(|| {
                panic!("§303: required field `{var}` is missing from the permissions form")
            });
        assert!(
            field.has_child("required", NS_DATA_FORMS),
            "§303: required field `{var}` carries `<required/>`"
        );
    }

    // count field value = DEFAULT_MENTIONS_COUNT (5) — must be the
    // text-single representation of the §301 example, otherwise
    // a recipient that lies dormant in the room with no per-room
    // override would compute a different threshold than the server.
    let count_value = form
        .children()
        .filter(|c| c.is("field", NS_DATA_FORMS))
        .find(|c| c.attr("var") == Some(FIELD_MENTIONS_COUNT))
        .and_then(|f| f.get_child("value", NS_DATA_FORMS))
        .map(Element::text)
        .expect("mentions#count carries a <value/>");
    assert_eq!(count_value, DEFAULT_MENTIONS_COUNT.to_string());

    // channel field — present iff policy.channel.is_some(); shape is
    // list-single with exactly the three §303 options
    // (participants/moderators/none) and its `<value/>` matches the
    // policy.
    let channel_field = form
        .children()
        .filter(|c| c.is("field", NS_DATA_FORMS))
        .find(|c| c.attr("var") == Some(FIELD_MENTIONS_CHANNEL))
        .expect("§303: `mentions#channel` MUST be present when `#channel` is advertised");
    assert_eq!(channel_field.attr("type"), Some("list-single"));
    assert!(channel_field.has_child("required", NS_DATA_FORMS));
    let channel_value = channel_field
        .get_child("value", NS_DATA_FORMS)
        .map(Element::text)
        .expect("mentions#channel carries a <value/>");
    assert_eq!(channel_value, MentionsPermission::Moderators.as_wire());
    let option_values: Vec<String> = channel_field
        .children()
        .filter(|c| c.is("option", NS_DATA_FORMS))
        .filter_map(|o| o.get_child("value", NS_DATA_FORMS).map(Element::text))
        .collect();
    assert_eq!(
        option_values,
        vec![
            MentionsPermission::Participants.as_wire(),
            MentionsPermission::Moderators.as_wire(),
            MentionsPermission::Nobody.as_wire(),
        ],
        "§303: `mentions#channel` option set must be exactly \
         {{participants, moderators, none}} in spec order"
    );
}

#[test]
fn xep0513_permissions_form_omits_channel_when_not_advertised() {
    // §303: "All other fields are OPTIONAL, but they MUST be present
    // if and only if the corresponding feature is advertised in
    // service discovery." A hypothetical room that advertised
    // `urn:xmpp:mentions:0` only (no `#channel`) MUST omit the
    // `mentions#channel` field. Slice 3c always advertises both, but
    // the builder MUST honour the typed `None` so the contract holds
    // for future rooms or test fixtures that disable channel mentions.
    let policy = MentionsPermissions {
        count: DEFAULT_MENTIONS_COUNT,
        individual: MentionsPermission::Participants,
        channel: None,
    };
    let query = build_mentions_permissions_query(&policy);
    let form = query
        .get_child("x", NS_DATA_FORMS)
        .expect("form child present");
    let channel_present = form
        .children()
        .filter(|c| c.is("field", NS_DATA_FORMS))
        .any(|c| c.attr("var") == Some(FIELD_MENTIONS_CHANNEL));
    assert!(
        !channel_present,
        "§303: `mentions#channel` MUST NOT appear when `urn:xmpp:mentions:0#channel` \
         is not advertised"
    );
}

#[test]
fn xep0513_permissions_form_omits_unadvertised_fields() {
    // §303: "All other fields are OPTIONAL, but they MUST be present
    // if and only if the corresponding feature is advertised in
    // service discovery." Waddle deliberately doesn't advertise
    // `mentions#space`, `mentions#server`, `mentions#associations`,
    // or `mentions#hats` (see scope discipline on #525); the form
    // builder MUST therefore NEVER emit those `var` attributes,
    // regardless of `MentionsPermissions` field values. This pin
    // closes the form-builder side of slice 3d's classifier-side
    // unsupported-groups pin (adversarial §303-alignment review on
    // PR #756).
    let policy = MentionsPermissions::server_default();
    let query = build_mentions_permissions_query(&policy);
    let form = query
        .get_child("x", NS_DATA_FORMS)
        .expect("§303 form payload");

    let advertised_vars: std::collections::BTreeSet<&str> = form
        .children()
        .filter(|c| c.is("field", NS_DATA_FORMS))
        .filter_map(|c| c.attr("var"))
        .collect();

    let unadvertised_vars = [
        "mentions#space",
        "mentions#server",
        "mentions#associations",
        "mentions#hats",
    ];
    for unadvertised in unadvertised_vars {
        assert!(
            !advertised_vars.contains(unadvertised),
            "§303 form MUST NOT emit `var='{unadvertised}'` — Waddle \
             does not advertise the corresponding feature. Found: \
             {advertised_vars:?}"
        );
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
        .attr(minidom::rxml::xml_ncname!("begin").to_owned(), "5")
        .attr(minidom::rxml::xml_ncname!("end").to_owned(), "11")
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
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), "@@@")
        .attr(
            minidom::rxml::xml_ncname!("mentions").to_owned(),
            CHANNEL_MENTION,
        )
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
        .attr(minidom::rxml::xml_ncname!("begin").to_owned(), "three")
        .attr(minidom::rxml::xml_ncname!("end").to_owned(), "")
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "alice@example.com",
        )
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
