//! XEP-0422 score fastening: origin-id target with room-authoritative binding.

use minidom::Element;
use waddle_xmpp_client::xep::safety_scores::{
    parse_room_safety_scores_child, parse_safety_scores_fastening, SafetyCategory, NS_FASTEN,
    NS_WADDLE_SAFETY_SCORES,
};

const ROOM: &str = "room@conference.example.org";
const APPLY: &str = "<apply-to xmlns='urn:xmpp:fasten:0' id='origin-1'>
  <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='jev-1'
    target-stanza-id='room-1' target-stanza-by='room@conference.example.org'
    source-revision-id='room-1'>
    <score category='is_question' probability='0.92' taxonomy-version='q-v1'/>
    <score category='safety:hate_speech' probability='0.03' taxonomy-version='hate-v1'/>
  </safety-scores>
</apply-to>";

fn element(xml: &str) -> Element {
    xml.parse().expect("fixture XML parses")
}

fn message(from: &str, kind: &str, apply: &str) -> Element {
    Element::builder("message", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), kind)
        .append(element(apply))
        .build()
}

#[test]
fn namespace_contract() {
    assert_eq!(NS_FASTEN, "urn:xmpp:fasten:0");
    assert_eq!(NS_WADDLE_SAFETY_SCORES, "urn:waddle:safety-scores:1");
}

#[test]
fn parses_origin_room_identity_revision_and_scores() {
    let parsed = parse_room_safety_scores_child(&message(ROOM, "groupchat", APPLY)).unwrap();
    assert_eq!(parsed.target_origin_id.as_str(), "origin-1");
    assert_eq!(parsed.target_stanza_id.as_str(), "room-1");
    assert_eq!(parsed.target_stanza_id.by.to_string(), ROOM);
    assert_eq!(parsed.source_revision_id.as_str(), "room-1");
    assert_eq!(parsed.scores.model_version.as_str(), "jev-1");
    assert_eq!(parsed.scores.scores.len(), 2);
    assert_eq!(parsed.scores.scores[0].category, SafetyCategory::IsQuestion);
    assert_eq!(parsed.scores.scores[0].probability.value(), 0.92);
}

#[test]
fn accepts_later_revision() {
    let apply = APPLY.replace("source-revision-id='room-1'", "source-revision-id='edit-2'");
    let parsed = parse_room_safety_scores_child(&message(ROOM, "groupchat", &apply)).unwrap();
    assert_eq!(parsed.source_revision_id.as_str(), "edit-2");
}

#[test]
fn missing_or_mismatched_binding_is_rejected() {
    for apply in [
        APPLY.replace(" target-stanza-id='room-1'", ""),
        APPLY.replace(" target-stanza-by='room@conference.example.org'", ""),
        APPLY.replace(" source-revision-id='room-1'", ""),
        APPLY.replace(
            "target-stanza-by='room@conference.example.org'",
            "target-stanza-by='other@conference.example.org'",
        ),
        APPLY.replace(
            "target-stanza-by='room@conference.example.org'",
            "target-stanza-by='room@conference.example.org/nick'",
        ),
    ] {
        assert!(parse_room_safety_scores_child(&message(ROOM, "groupchat", &apply)).is_none());
    }
}

#[test]
fn untrusted_sender_is_rejected() {
    assert!(parse_room_safety_scores_child(&message(
        "room@conference.example.org/mallory",
        "groupchat",
        APPLY
    ))
    .is_none());
    assert!(parse_room_safety_scores_child(&message("peer@example.org", "chat", APPLY)).is_none());
}

#[test]
fn missing_origin_or_model_is_rejected() {
    for apply in [
        APPLY.replace(" id='origin-1'", ""),
        APPLY.replace(" model-version='jev-1'", ""),
    ] {
        assert!(parse_safety_scores_fastening(&message(ROOM, "groupchat", &apply)).is_none());
    }
}

#[test]
fn repeated_apply_to_or_score_payload_is_rejected() {
    let twice = Element::builder("message", "jabber:client")
        .append(element(APPLY))
        .append(element(APPLY))
        .build();
    assert!(parse_safety_scores_fastening(&twice).is_none());
    let duplicate = APPLY.replace("</safety-scores>", "</safety-scores><safety-scores xmlns='urn:waddle:safety-scores:1' model-version='jev-2' target-stanza-id='room-1' target-stanza-by='room@conference.example.org' source-revision-id='room-1'/>");
    assert!(parse_safety_scores_fastening(&message(ROOM, "groupchat", &duplicate)).is_none());
}

#[test]
fn shell_is_ignored_and_clear_is_not_a_result() {
    let shell = Element::builder("message", "jabber:client")
        .append(element(
            "<apply-to xmlns='urn:xmpp:fasten:0' id='origin-1' shell='true'/>",
        ))
        .append(element(APPLY))
        .build();
    assert!(parse_safety_scores_fastening(&shell).is_some());
    let clear = "<apply-to xmlns='urn:xmpp:fasten:0' id='origin-1' clear='true'><safety-scores xmlns='urn:waddle:safety-scores:1'/></apply-to>";
    assert!(parse_safety_scores_fastening(&message(ROOM, "groupchat", clear)).is_none());
}

#[test]
fn malformed_and_unknown_scores_are_skipped() {
    let apply = APPLY.replace("</safety-scores>", "<score category='future' probability='0.5' taxonomy-version='v1'/><score category='safety:explicit' probability='NaN' taxonomy-version='v1'/><score category='is_question' probability='0.1' taxonomy-version='duplicate'/></safety-scores>");
    let parsed = parse_room_safety_scores_child(&message(ROOM, "groupchat", &apply)).unwrap();
    assert_eq!(parsed.scores.scores.len(), 2);
    assert_eq!(parsed.scores.scores[0].probability.value(), 0.92);
}
