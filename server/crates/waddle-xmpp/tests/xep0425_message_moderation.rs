//! XEP-0425: Moderated Message Retraction (v1) — dedicated suite.
//!
//! In-crate `xep::xep0425::tests` covers helper internals
//! (`<moderate>` IQ parse, builder/extractor round-trip). This file
//! pins the v1 audit-level invariants at the public API:
//!
//! - the §3 namespace string `urn:xmpp:message-moderate:1`,
//! - the §"Discovering support" MUST disco feature on every MUC
//!   room configuration,
//! - the v1 §3 broadcast wire shape: outer `<retract id=…>` with a
//!   nested `<moderated by=…><occupant-id/></moderated>` and optional
//!   `<reason>`,
//! - the XEP-0421 `<occupant-id/>` attribution invariant — the
//!   broadcast MUST carry the moderator's occupant-id when supplied,
//!   so semi-anonymous rooms still surface a stable identifier even
//!   when `by=` hides the real bare JID,
//! - parser robustness: empty `id="…"`, missing `<moderated>`, and
//!   wrong-namespace near-misses are all rejected.

use minidom::Element;
use waddle_xmpp::disco::{muc_room_features, Feature};
use waddle_xmpp::xep::xep0425::{
    build_moderated_retract_element, build_moderation_result_message, extract_moderation_result,
    is_moderation_result_message, parse_moderation_iq, ModerationCarrier, NS_MESSAGE_MODERATE,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0425_namespace_matches_spec_v1() {
    // XEP-0425 v1 (Proposed, post-fastening) bumped the namespace
    // from `:0` to `:1` when the spec dropped the XEP-0422
    // `<apply-to>` wrapping. Pin the literal so an accidental v0
    // import or a typo trips a test instead of silently routing
    // moderation broadcasts into "unknown payload."
    assert_eq!(NS_MESSAGE_MODERATE, "urn:xmpp:message-moderate:1");
}

// ── §"Discovering support" advertisement ────────────────────────────

#[test]
fn xep0425_advertised_on_every_room_configuration() {
    // XEP-0425 §"Discovering support": "If a groupchat supports
    // moderated message retraction, it MUST specify the
    // 'urn:xmpp:message-moderate:1' feature in its service
    // discovery information features." Waddle's MUC server enforces
    // moderation authz uniformly across room configurations, so the
    // advert MUST survive every (persistent × members_only ×
    // moderated × forum) cell.
    let target = Feature::message_moderation();
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features(persistent={persistent}, members_only={members_only}, \
                         moderated={moderated}, forum={forum}) MUST advertise \
                         `urn:xmpp:message-moderate:1`"
                    );
                }
            }
        }
    }
}

#[test]
fn xep0425_feature_constructor_pins_namespace_string() {
    // Defence in depth against a future constructor rename
    // silently changing the wire string.
    assert_eq!(
        Feature::message_moderation().0,
        "urn:xmpp:message-moderate:1"
    );
}

// ── §3 IQ request shape ─────────────────────────────────────────────

#[test]
fn xep0425_parses_v1_iq_request_with_reason() {
    // The §3 example moderation request: an IQ-set against the
    // room JID, with `<moderate id=…>` carrying a `<retract/>`
    // child and optional `<reason>`.
    let xml = "<iq xmlns='jabber:client' type='set' to='room@muc.example.com' id='m-1'>\
                  <moderate xmlns='urn:xmpp:message-moderate:1' id='target-stanza-1'>\
                    <retract xmlns='urn:xmpp:message-retract:1'/>\
                    <reason>off-topic</reason>\
                  </moderate>\
               </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("iq");

    let req = parse_moderation_iq(&iq).expect("v1 moderate IQ parses");
    assert_eq!(req.target_id, "target-stanza-1");
    assert_eq!(req.reason.as_deref(), Some("off-topic"));
}

#[test]
fn xep0425_rejects_v0_apply_to_fastening_iq() {
    // Pre-v1 (XEP-0425 v0) wrapped `<moderate>` inside an
    // XEP-0422 `<apply-to>` element on a message. v1 dropped that
    // entirely; the parser MUST NOT recognise the legacy shape.
    let xml = "<iq xmlns='jabber:client' type='set' to='room@muc.example.com' id='m-2'>\
                  <apply-to xmlns='urn:xmpp:fasten:0' id='target-stanza-2'>\
                    <moderate xmlns='urn:xmpp:message-moderate:1'>\
                      <retract xmlns='urn:xmpp:message-retract:1'/>\
                    </moderate>\
                  </apply-to>\
               </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("iq");

    assert!(parse_moderation_iq(&iq).is_none());
}

#[test]
fn xep0425_request_iq_requires_inner_retract_child() {
    // §3 mandates the `<retract/>` child inside `<moderate>`. A
    // `<moderate>` without it is malformed and MUST NOT parse, since
    // moderating-but-not-retracting has no defined semantics in v1.
    let xml = "<iq xmlns='jabber:client' type='set' to='room@muc.example.com' id='m-3'>\
                  <moderate xmlns='urn:xmpp:message-moderate:1' id='target-stanza-3'>\
                    <reason>nope</reason>\
                  </moderate>\
               </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("iq");

    assert!(parse_moderation_iq(&iq).is_none());
}

#[test]
fn xep0425_request_iq_rejects_get_type() {
    // §3 fixes the request as `type='set'`. A `get` on the same
    // shape is meaningless and MUST NOT be classified as a request,
    // otherwise the moderation handler would be reachable via the
    // wrong IQ type.
    let xml = "<iq xmlns='jabber:client' type='get' to='room@muc.example.com' id='m-4'>\
                  <moderate xmlns='urn:xmpp:message-moderate:1' id='target-stanza-4'>\
                    <retract xmlns='urn:xmpp:message-retract:1'/>\
                  </moderate>\
               </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("iq");

    assert!(parse_moderation_iq(&iq).is_none());
}

// ── §3 broadcast wire shape ─────────────────────────────────────────

#[test]
fn xep0425_broadcast_carries_occupant_id_attribution() {
    // XEP-0425 v1 §3 spec example places `<occupant-id>` inside
    // `<moderated>`. Without it, semi-anonymous rooms (which hide
    // the moderator's real JID via XEP-0421) have no stable
    // identifier to cite the moderator from the broadcast alone.
    //
    // We assert against the typed minidom tree rather than the
    // serialised string so attribute quote-style ambiguity (the
    // minidom serializer mixes single and double quotes) doesn't
    // leak into the test.
    let elem = build_moderated_retract_element(
        "victim-stanza-id",
        "room@muc.example.com/mod-nick",
        Some("dd72603deec90a38ba552f7c68cbcc61"),
        Some("Inappropriate content"),
    );

    assert_eq!(elem.name(), "retract");
    assert_eq!(elem.ns(), "urn:xmpp:message-retract:1");
    assert_eq!(elem.attr("id"), Some("victim-stanza-id"));

    let moderated = elem
        .children()
        .find(|c| c.name() == "moderated" && c.ns() == "urn:xmpp:message-moderate:1")
        .expect("<moderated> child present");
    assert_eq!(moderated.attr("by"), Some("room@muc.example.com/mod-nick"));

    let occ = moderated
        .children()
        .find(|c| c.name() == "occupant-id" && c.ns() == "urn:xmpp:occupant-id:0")
        .expect(
            "broadcast MUST embed moderator's XEP-0421 occupant-id \
             for semi-anonymous attribution",
        );
    assert_eq!(occ.attr("id"), Some("dd72603deec90a38ba552f7c68cbcc61"));

    let reason = elem
        .children()
        .find(|c| c.name() == "reason" && c.ns() == "urn:xmpp:message-retract:1")
        .expect("<reason> child present");
    assert_eq!(reason.text(), "Inappropriate content");
}

#[test]
fn xep0425_broadcast_omits_occupant_id_child_when_none_supplied() {
    // For non-anonymous rooms the spec allows the moderator's real
    // JID via `by=` alone; the builder MUST honor `None` rather
    // than emitting a placeholder `<occupant-id id=""/>` that would
    // be unparseable on the receiving side.
    let elem = build_moderated_retract_element(
        "victim-stanza-id",
        "room@muc.example.com/mod-nick",
        None,
        None,
    );

    let moderated = elem
        .children()
        .find(|c| c.name() == "moderated" && c.ns() == "urn:xmpp:message-moderate:1")
        .expect("<moderated> child present");
    assert!(
        moderated
            .children()
            .find(|c| c.name() == "occupant-id")
            .is_none(),
        "absent occupant-id MUST NOT be invented"
    );
    assert!(
        elem.children().find(|c| c.name() == "reason").is_none(),
        "absent reason MUST NOT be invented"
    );
}

#[test]
fn xep0425_broadcast_round_trip_preserves_occupant_id_attribution() {
    // Round-trip: build the broadcast, parse it back, confirm every
    // field is recovered including the occupant-id. The audit-bug
    // this guards against is the prior `stamp: String::new()` mode
    // where part of the parsed value was unconditionally empty.
    let msg = build_moderation_result_message(
        "room@muc.example.com".parse::<jid::Jid>().ok(),
        "msg-99",
        "room@muc.example.com/moderator",
        Some("opaque-occupant-id-abcdef"),
        Some("Spam"),
    );

    assert!(
        is_moderation_result_message(&msg),
        "built broadcast must classify as a moderation result"
    );
    let result = msg.moderation_result().expect("extractable");
    assert_eq!(result.target_id, "msg-99");
    assert_eq!(result.moderated_by, "room@muc.example.com/moderator");
    assert_eq!(
        result.moderator_occupant_id.as_deref(),
        Some("opaque-occupant-id-abcdef")
    );
    assert_eq!(result.reason.as_deref(), Some("Spam"));
}

// ── Parser robustness ───────────────────────────────────────────────

#[test]
fn xep0425_extract_rejects_empty_target_id() {
    // `id=""` would route the moderation announcement against a
    // phantom message id; consumers must treat it as malformed.
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                  <retract xmlns='urn:xmpp:message-retract:1' id=''>\
                    <moderated xmlns='urn:xmpp:message-moderate:1' by='room@example/m'/>\
                  </retract>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");

    assert!(extract_moderation_result(&msg).is_none());
}

#[test]
fn xep0425_extract_rejects_missing_moderated_child() {
    // A `<retract>` without `<moderated>` is a plain XEP-0424
    // self-retraction, not a XEP-0425 moderator broadcast. The
    // classifier MUST distinguish them — otherwise the inbox /
    // archive paths would attribute every self-retraction to a
    // moderator.
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                  <retract xmlns='urn:xmpp:message-retract:1' id='abc'/>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");

    assert!(extract_moderation_result(&msg).is_none());
}

#[test]
fn xep0425_extract_rejects_moderated_with_empty_by() {
    // `<moderated by=""/>` strips the attribution that the spec
    // example treats as required. Consumers can't show "moderated
    // by ⟨nothing⟩" — drop it.
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                  <retract xmlns='urn:xmpp:message-retract:1' id='abc'>\
                    <moderated xmlns='urn:xmpp:message-moderate:1' by=''/>\
                  </retract>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");

    assert!(extract_moderation_result(&msg).is_none());
}

#[test]
fn xep0425_extract_accepts_broadcast_without_reason() {
    // §3 allows `<reason>` to be absent — moderators don't have to
    // justify every action. The classifier must accept the
    // reasonless shape.
    let xml = "<message xmlns='jabber:client' type='groupchat'>\
                  <retract xmlns='urn:xmpp:message-retract:1' id='abc'>\
                    <moderated xmlns='urn:xmpp:message-moderate:1' by='room@example/m'>\
                      <occupant-id xmlns='urn:xmpp:occupant-id:0' id='xyz'/>\
                    </moderated>\
                  </retract>\
               </message>";
    let msg = Message::try_from(xml.parse::<Element>().expect("xml")).expect("message");

    let result = extract_moderation_result(&msg).expect("reasonless broadcast is valid");
    assert_eq!(result.reason, None);
    assert_eq!(result.moderator_occupant_id.as_deref(), Some("xyz"));
}
