//! `urn:waddle:safety-scores:1` over XEP-0422 Message Fastening: dedicated
//! client suite.
//!
//! The server fastens per-category judgment probabilities to an already
//! delivered message. The client obligations covered here:
//! - the contract fixture parses into typed values for both groupchat and
//!   chat messages (fastening is message-type agnostic);
//! - XEP-0422 §Replacing / §Removing: a normal fastening is a replace, a
//!   `clear='true'` apply-to is a removal;
//! - XEP-0422 §Interaction with stanza encryption: a `shell='true'`
//!   placeholder carries no content and is ignored;
//! - XEP-0422 §Wrapped Payloads: only a payload wrapped in `<apply-to/>`
//!   counts, and other fastening types in the apply-to are not ours;
//! - forward compatibility: unknown categories and malformed scores are
//!   skipped without failing the rest of the payload.

use minidom::Element;
use waddle_xmpp_client::{
    messaging::{parse, InboundMessage, MessagingEvent},
    xep::safety_scores::{
        JudgmentCategory, SafetyScores, SafetyScoresFastening, SafetyScoresUpdate,
        NS_WADDLE_SAFETY_SCORES,
    },
};

const MODEL: &str = "typesafe/jev-1.13-20260917";

fn inbound(xml: &str) -> InboundMessage {
    let element: Element = xml.parse().expect("fixture XML parses");
    match parse(&element) {
        Some(MessagingEvent::Message(message)) => *message,
        other => panic!("expected an inbound message, got {other:?}"),
    }
}

fn fastening(xml: &str) -> Option<SafetyScoresFastening> {
    inbound(xml).safety_scores
}

fn replaced(xml: &str) -> (String, SafetyScores) {
    match fastening(xml) {
        Some(SafetyScoresFastening {
            target_id,
            update: SafetyScoresUpdate::Replace(scores),
        }) => (target_id, scores),
        other => panic!("expected a replace fastening, got {other:?}"),
    }
}

fn wrap(message_type: &str, apply_to_attrs: &str, payload: &str) -> String {
    format!(
        "<message xmlns='jabber:client' from='room@conference.example.org' \
         to='alice@example.org/web' type='{message_type}'>\
         <apply-to xmlns='urn:xmpp:fasten:0' {apply_to_attrs}>{payload}</apply-to>\
         </message>"
    )
}

fn scores_payload(scores: &str) -> String {
    format!("<safety-scores xmlns='{NS_WADDLE_SAFETY_SCORES}' model-version='{MODEL}'>{scores}</safety-scores>")
}

const CONTRACT_SCORES: &str = "\
    <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>\
    <score category='safety:hate_speech' probability='0.03' taxonomy-version='safety-hate-speech-v1'/>\
    <score category='safety:explicit' probability='0.01' taxonomy-version='safety-explicit-v1'/>\
    <score category='safety:harassment' probability='0.02' taxonomy-version='safety-harassment-v1'/>\
    <score category='safety:violence' probability='0.0' taxonomy-version='safety-violence-v1'/>\
    <score category='safety:self_harm' probability='0.0' taxonomy-version='safety-self-harm-v1'/>";

fn summary(scores: &SafetyScores) -> Vec<(JudgmentCategory, f64, String)> {
    scores
        .scores
        .iter()
        .map(|score| {
            (
                score.category,
                score.probability.value(),
                score.taxonomy_version.as_str().to_owned(),
            )
        })
        .collect()
}

#[test]
fn contract_fixture_parses_for_groupchat() {
    let xml = wrap(
        "groupchat",
        "id='stanza-1'",
        &scores_payload(CONTRACT_SCORES),
    );
    let message = inbound(&xml);
    assert!(message.body.is_none(), "a fastening carries no body");

    let (target, scores) = replaced(&xml);
    assert_eq!(target, "stanza-1");
    assert_eq!(scores.model_version.as_str(), MODEL);
    assert_eq!(
        summary(&scores),
        vec![
            (
                JudgmentCategory::IsQuestion,
                0.92,
                "is-question-v1".to_owned()
            ),
            (
                JudgmentCategory::HateSpeech,
                0.03,
                "safety-hate-speech-v1".to_owned()
            ),
            (
                JudgmentCategory::Explicit,
                0.01,
                "safety-explicit-v1".to_owned()
            ),
            (
                JudgmentCategory::Harassment,
                0.02,
                "safety-harassment-v1".to_owned()
            ),
            (
                JudgmentCategory::Violence,
                0.0,
                "safety-violence-v1".to_owned()
            ),
            (
                JudgmentCategory::SelfHarm,
                0.0,
                "safety-self-harm-v1".to_owned()
            ),
        ]
    );
}

#[test]
fn same_shape_parses_for_direct_chat() {
    let xml = wrap("chat", "id='dm-stanza'", &scores_payload(CONTRACT_SCORES));
    let (target, scores) = replaced(&xml);
    assert_eq!(target, "dm-stanza");
    assert_eq!(scores.scores.len(), 6);
}

#[test]
fn every_known_category_round_trips_its_wire_token() {
    for category in JudgmentCategory::ALL {
        assert_eq!(
            JudgmentCategory::from_wire(category.as_wire()),
            Some(category)
        );
    }
    assert_eq!(JudgmentCategory::from_wire("safety:spam"), None);
}

#[test]
fn unknown_categories_are_skipped_not_fatal() {
    let xml = wrap(
        "groupchat",
        "id='t'",
        &scores_payload(
            "<score category='safety:spam' probability='0.5' taxonomy-version='safety-spam-v1'/>\
             <score category='is_question' probability='0.4' taxonomy-version='is-question-v1'/>",
        ),
    );
    let (_, scores) = replaced(&xml);
    assert_eq!(
        summary(&scores),
        vec![(
            JudgmentCategory::IsQuestion,
            0.4,
            "is-question-v1".to_owned()
        )]
    );
}

#[test]
fn malformed_scores_are_skipped_individually() {
    let xml = wrap(
        "groupchat",
        "id='t'",
        &scores_payload(
            "<score category='safety:hate_speech' probability='1.5' taxonomy-version='v'/>\
             <score category='safety:explicit' probability='NaN' taxonomy-version='v'/>\
             <score category='safety:harassment' probability='-0.1' taxonomy-version='v'/>\
             <score category='safety:violence' probability='high' taxonomy-version='v'/>\
             <score category='safety:self_harm' probability='0.2'/>\
             <score probability='0.2' taxonomy-version='v'/>\
             <score category='is_question' taxonomy-version='v'/>\
             <score category='safety:violence' probability='1' taxonomy-version='safety-violence-v1'/>",
        ),
    );
    let (_, scores) = replaced(&xml);
    assert_eq!(
        summary(&scores),
        vec![(
            JudgmentCategory::Violence,
            1.0,
            "safety-violence-v1".to_owned()
        )]
    );
}

#[test]
fn duplicate_category_keeps_the_first_score() {
    let xml = wrap(
        "groupchat",
        "id='t'",
        &scores_payload(
            "<score category='is_question' probability='0.1' taxonomy-version='is-question-v1'/>\
             <score category='is_question' probability='0.9' taxonomy-version='is-question-v2'/>",
        ),
    );
    let (_, scores) = replaced(&xml);
    assert_eq!(
        summary(&scores),
        vec![(
            JudgmentCategory::IsQuestion,
            0.1,
            "is-question-v1".to_owned()
        )]
    );
}

#[test]
fn payload_with_no_known_scores_is_an_empty_replace() {
    let xml = wrap("groupchat", "id='t'", &scores_payload(""));
    let (_, scores) = replaced(&xml);
    assert!(scores.scores.is_empty());
}

#[test]
fn clear_true_removes_the_fastening() {
    let xml = wrap(
        "groupchat",
        "id='t' clear='true'",
        &format!("<safety-scores xmlns='{NS_WADDLE_SAFETY_SCORES}'/>"),
    );
    assert_eq!(
        fastening(&xml),
        Some(SafetyScoresFastening {
            target_id: "t".to_owned(),
            update: SafetyScoresUpdate::Clear,
        })
    );
}

#[test]
fn encryption_shell_is_ignored() {
    let xml = wrap(
        "groupchat",
        "id='t' shell='true'",
        &scores_payload(CONTRACT_SCORES),
    );
    assert_eq!(fastening(&xml), None);
}

#[test]
fn missing_or_empty_target_id_is_rejected() {
    assert_eq!(
        fastening(&wrap("groupchat", "", &scores_payload(CONTRACT_SCORES))),
        None
    );
    assert_eq!(
        fastening(&wrap(
            "groupchat",
            "id=''",
            &scores_payload(CONTRACT_SCORES)
        )),
        None
    );
}

#[test]
fn missing_model_version_is_rejected() {
    let payload = format!(
        "<safety-scores xmlns='{NS_WADDLE_SAFETY_SCORES}'>{CONTRACT_SCORES}</safety-scores>"
    );
    assert_eq!(fastening(&wrap("groupchat", "id='t'", &payload)), None);
}

#[test]
fn other_fastening_types_are_not_safety_scores() {
    let xml = wrap(
        "groupchat",
        "id='anchor'",
        "<call-thread-ended xmlns='urn:waddle:call-thread:0' ended='2026-06-07T15:00:00Z' duration='PT30M'/>",
    );
    let message = inbound(&xml);
    assert!(message.safety_scores.is_none());
    assert!(message.call_thread_ended.is_some());
}

#[test]
fn payload_in_the_wrong_namespace_is_ignored() {
    let xml = wrap(
        "groupchat",
        "id='t'",
        &format!("<safety-scores xmlns='urn:waddle:safety-scores:0' model-version='{MODEL}'>{CONTRACT_SCORES}</safety-scores>"),
    );
    assert_eq!(fastening(&xml), None);
}

#[test]
fn unwrapped_payload_is_not_a_fastening() {
    let xml = format!(
        "<message xmlns='jabber:client' from='room@conference.example.org' type='groupchat'>{}</message>",
        scores_payload(CONTRACT_SCORES)
    );
    assert_eq!(fastening(&xml), None);
}
