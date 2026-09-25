//! XEP-0422 Message Fastening: `urn:waddle:safety-scores:1` dedicated
//! client suite (issue #1831).
//!
//! Covers the typed parse of the fastening wrapper and payload, the
//! XEP-0422 replace/clear semantics the parser exposes, forward
//! compatibility with unknown categories, and the room-authority gate the
//! inbound message parser applies.

use minidom::Element;
use waddle_xmpp_client::{
    messaging::{parse, MessagingEvent},
    xep::safety_scores::{
        parse_room_safety_scores_child, parse_safety_scores, parse_safety_scores_fastening,
        SafetyProbability, SafetyScoreCategory, SafetyScoresFastening, SafetyScoresParseError,
        SafetyScoresPayload, NS_FASTEN, NS_WADDLE_SAFETY_SCORES,
    },
};

const ROOM: &str = "general@muc.waddle.test";

fn element(xml: &str) -> Element {
    xml.parse().expect("fixture XML parses")
}

fn room_message(from: &str, message_type: &str, apply_to: &str) -> Element {
    Element::builder("message", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), message_type)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "f1")
        .append(element(apply_to))
        .build()
}

fn occupant(nick: &str) -> String {
    let mut jid = String::from(ROOM);
    jid.push('/');
    jid.push_str(nick);
    jid
}

const FULL_BATCH: &str = "<apply-to xmlns='urn:xmpp:fasten:0' id='judged-1'>\
    <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='typesafe/jev-1.13-20260917'>\
      <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>\
      <score category='safety:hate_speech' probability='0.03' taxonomy-version='safety-hate-speech-v1'/>\
      <score category='safety:explicit' probability='0.01' taxonomy-version='safety-explicit-v1'/>\
      <score category='safety:harassment' probability='0.02' taxonomy-version='safety-harassment-v1'/>\
      <score category='safety:violence' probability='0.0' taxonomy-version='safety-violence-v1'/>\
      <score category='safety:self_harm' probability='0.0' taxonomy-version='safety-self-harm-v1'/>\
    </safety-scores>\
  </apply-to>";

fn scores_of(
    fastening: SafetyScoresFastening,
) -> waddle_xmpp_client::xep::safety_scores::SafetyScores {
    match fastening.payload {
        SafetyScoresPayload::Scores(scores) => scores,
        SafetyScoresPayload::Cleared => panic!("expected scores, got a clear"),
    }
}

#[test]
fn namespaces_are_the_contracted_values() {
    assert_eq!(NS_FASTEN, "urn:xmpp:fasten:0");
    assert_eq!(NS_WADDLE_SAFETY_SCORES, "urn:waddle:safety-scores:1");
}

#[test]
fn category_tokens_match_server_judgment_names() {
    let tokens: Vec<&str> = SafetyScoreCategory::ALL
        .into_iter()
        .map(SafetyScoreCategory::as_token)
        .collect();
    assert_eq!(
        tokens,
        [
            "is_question",
            "safety:hate_speech",
            "safety:explicit",
            "safety:harassment",
            "safety:violence",
            "safety:self_harm",
        ]
    );
    for category in SafetyScoreCategory::ALL {
        assert_eq!(
            SafetyScoreCategory::parse_token(category.as_token()),
            Some(category)
        );
    }
    assert_eq!(SafetyScoreCategory::parse_token("safety:spam"), None);
}

#[test]
fn parses_the_full_contract_batch_in_wire_order() {
    let fastening =
        parse_safety_scores_fastening(&element(FULL_BATCH)).expect("contract batch parses");
    assert_eq!(fastening.target_id.as_str(), "judged-1");
    let scores = scores_of(fastening);
    assert_eq!(scores.model_version.as_str(), "typesafe/jev-1.13-20260917");
    let categories: Vec<_> = scores.scores.iter().map(|s| s.category).collect();
    assert_eq!(categories, SafetyScoreCategory::ALL);
    assert_eq!(scores.scores[0].probability.value(), 0.92);
    assert_eq!(scores.scores[0].taxonomy_version.as_str(), "is-question-v1");
    assert_eq!(scores.scores[4].probability.value(), 0.0);
}

#[test]
fn unknown_categories_are_skipped_not_fatal() {
    let apply_to = element(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='judged-1'>\
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>\
             <score category='safety:spam' probability='0.7' taxonomy-version='spam-v1'/>\
             <score category='safety:violence' probability='0.4' taxonomy-version='v1'/>\
           </safety-scores>\
         </apply-to>",
    );
    let scores = scores_of(parse_safety_scores_fastening(&apply_to).expect("parses"));
    assert_eq!(scores.scores.len(), 1);
    assert_eq!(scores.scores[0].category, SafetyScoreCategory::Violence);
}

#[test]
fn a_batch_of_only_unknown_categories_still_replaces() {
    let apply_to = element(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='judged-1'>\
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m2'>\
             <score category='safety:spam' probability='0.7' taxonomy-version='spam-v1'/>\
           </safety-scores>\
         </apply-to>",
    );
    let scores = scores_of(parse_safety_scores_fastening(&apply_to).expect("parses"));
    assert_eq!(scores.model_version.as_str(), "m2");
    assert!(scores.scores.is_empty());
}

#[test]
fn malformed_scores_are_skipped_individually() {
    let payload = element(
        "<safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>\
           <score category='is_question' probability='1.5' taxonomy-version='v'/>\
           <score category='safety:explicit' probability='-0.1' taxonomy-version='v'/>\
           <score category='safety:harassment' probability='NaN' taxonomy-version='v'/>\
           <score category='safety:violence' probability='inf' taxonomy-version='v'/>\
           <score category='safety:hate_speech' probability='high' taxonomy-version='v'/>\
           <score category='safety:self_harm' probability='0.2'/>\
           <score category='safety:self_harm' probability='0.3' taxonomy-version='  '/>\
           <score probability='0.3' taxonomy-version='v'/>\
           <score xmlns='urn:example:other' category='is_question' probability='0.3' taxonomy-version='v'/>\
           <score category='is_question' probability='1' taxonomy-version='v'/>\
         </safety-scores>",
    );
    let scores = parse_safety_scores(&payload).expect("batch parses");
    assert_eq!(scores.scores.len(), 1);
    assert_eq!(scores.scores[0].category, SafetyScoreCategory::IsQuestion);
    assert_eq!(scores.scores[0].probability.value(), 1.0);
}

#[test]
fn repeated_categories_keep_the_first_occurrence() {
    let payload = element(
        "<safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>\
           <score category='safety:violence' probability='0.1' taxonomy-version='a'/>\
           <score category='safety:violence' probability='0.9' taxonomy-version='b'/>\
         </safety-scores>",
    );
    let scores = parse_safety_scores(&payload).expect("batch parses");
    assert_eq!(scores.scores.len(), 1);
    assert_eq!(scores.scores[0].probability.value(), 0.1);
    assert_eq!(scores.scores[0].taxonomy_version.as_str(), "a");
}

#[test]
fn missing_model_version_rejects_the_batch() {
    let payload = element(
        "<safety-scores xmlns='urn:waddle:safety-scores:1'>\
           <score category='is_question' probability='0.1' taxonomy-version='a'/>\
         </safety-scores>",
    );
    assert_eq!(
        parse_safety_scores(&payload),
        Err(SafetyScoresParseError::MissingAttribute("model-version"))
    );
    let blank = element("<safety-scores xmlns='urn:waddle:safety-scores:1' model-version=' '/>");
    assert_eq!(
        parse_safety_scores(&blank),
        Err(SafetyScoresParseError::EmptyVersion)
    );
}

#[test]
fn apply_to_requires_a_target_id_and_the_safety_scores_payload() {
    let no_id = element(
        "<apply-to xmlns='urn:xmpp:fasten:0'>\
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m'/>\
         </apply-to>",
    );
    assert_eq!(
        parse_safety_scores_fastening(&no_id),
        Err(SafetyScoresParseError::MissingTargetId)
    );
    let blank_id = element(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='  '>\
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m'/>\
         </apply-to>",
    );
    assert_eq!(
        parse_safety_scores_fastening(&blank_id),
        Err(SafetyScoresParseError::MissingTargetId)
    );
    let other_payload = element(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='x'>\
           <call-thread-ended xmlns='urn:waddle:call-thread:0' ended='2026-06-07T14:35:00Z' duration='PT5M'/>\
         </apply-to>",
    );
    assert_eq!(
        parse_safety_scores_fastening(&other_payload),
        Err(SafetyScoresParseError::NotSafetyScores)
    );
    let wrong_ns = element(
        "<apply-to xmlns='urn:example:fasten' id='x'>\
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m'/>\
         </apply-to>",
    );
    assert_eq!(
        parse_safety_scores_fastening(&wrong_ns),
        Err(SafetyScoresParseError::NotApplyTo)
    );
}

#[test]
fn xep0422_unknown_apply_to_children_are_ignored() {
    let apply_to = element(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='judged-1'>\
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m'>\
             <score category='is_question' probability='0.5' taxonomy-version='v'/>\
           </safety-scores>\
           <external xmlns='urn:xmpp:fasten:0' name='body'/>\
           <future xmlns='urn:example:future'/>\
         </apply-to>",
    );
    let scores = scores_of(parse_safety_scores_fastening(&apply_to).expect("parses"));
    assert_eq!(scores.scores.len(), 1);
}

#[test]
fn xep0422_clear_removes_the_fastening() {
    for flag in ["true", "1"] {
        let apply_to = Element::builder("apply-to", NS_FASTEN)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "judged-1")
            .attr(minidom::rxml::xml_ncname!("clear").to_owned(), flag)
            .append(Element::builder("safety-scores", NS_WADDLE_SAFETY_SCORES).build())
            .build();
        let fastening = parse_safety_scores_fastening(&apply_to).expect("clear parses");
        assert_eq!(fastening.target_id.as_str(), "judged-1");
        assert_eq!(fastening.payload, SafetyScoresPayload::Cleared);
    }
    let not_clear = element(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='judged-1' clear='false'>\
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m'/>\
         </apply-to>",
    );
    assert!(matches!(
        parse_safety_scores_fastening(&not_clear)
            .expect("parses")
            .payload,
        SafetyScoresPayload::Scores(_)
    ));
}

#[test]
fn probability_bounds_are_inclusive() {
    assert_eq!(SafetyProbability::parse("0").map(|p| p.value()), Ok(0.0));
    assert_eq!(SafetyProbability::parse("1.0").map(|p| p.value()), Ok(1.0));
    assert_eq!(
        SafetyProbability::parse(" 0.25 ").map(|p| p.value()),
        Ok(0.25)
    );
    assert!(SafetyProbability::parse("1.0000001").is_err());
    assert!(SafetyProbability::new(f64::NAN).is_err());
}

#[test]
fn room_bare_jid_groupchat_broadcast_is_accepted() {
    let message = room_message(ROOM, "groupchat", FULL_BATCH);
    let fastening = parse_room_safety_scores_child(&message).expect("room broadcast accepted");
    assert_eq!(fastening.target_id.as_str(), "judged-1");
}

#[test]
fn occupant_authored_scores_are_rejected() {
    let message = room_message(&occupant("mallory"), "groupchat", FULL_BATCH);
    assert!(parse_room_safety_scores_child(&message).is_none());
}

#[test]
fn direct_message_scores_are_not_accepted_until_a_sender_is_defined() {
    for message_type in ["chat", "normal"] {
        let message = room_message("peer@waddle.test", message_type, FULL_BATCH);
        assert!(parse_room_safety_scores_child(&message).is_none());
    }
    // The wrapper itself is type-agnostic: the same apply-to parses.
    let message = room_message("peer@waddle.test", "chat", FULL_BATCH);
    let apply_to = message
        .get_child("apply-to", NS_FASTEN)
        .expect("fixture has apply-to");
    assert!(parse_safety_scores_fastening(apply_to).is_ok());
}

#[test]
fn inbound_message_parser_surfaces_room_scores() {
    let message = room_message(ROOM, "groupchat", FULL_BATCH);
    let Some(MessagingEvent::Message(inbound)) = parse(&message) else {
        panic!("expected a parsed message");
    };
    let fastening = inbound
        .safety_scores
        .expect("scores on the inbound message");
    assert_eq!(fastening.target_id.as_str(), "judged-1");
    assert!(inbound.body.is_none());
    assert!(inbound.call_thread_ended.is_none());
}

#[test]
fn inbound_message_parser_drops_spoofed_scores() {
    let message = room_message(&occupant("mallory"), "groupchat", FULL_BATCH);
    let Some(MessagingEvent::Message(inbound)) = parse(&message) else {
        panic!("expected a parsed message");
    };
    assert!(inbound.safety_scores.is_none());
}
