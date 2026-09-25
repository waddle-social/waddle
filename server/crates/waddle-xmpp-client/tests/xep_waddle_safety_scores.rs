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
//!
//! Fixtures are built with `minidom` builders (no string-built XML).

use minidom::{rxml::NcName, Element};
use waddle_xmpp_client::{
    messaging::{parse, InboundMessage, MessagingEvent, NS_CLIENT},
    xep::safety_scores::{
        JudgmentCategory, SafetyScores, SafetyScoresFastening, SafetyScoresUpdate, NS_FASTEN,
        NS_WADDLE_SAFETY_SCORES,
    },
};

const MODEL: &str = "typesafe/jev-1.13-20260917";
const NS_CALL_THREAD: &str = "urn:waddle:call-thread:0";

const CONTRACT_SCORES: [(&str, &str, &str); 6] = [
    ("is_question", "0.92", "is-question-v1"),
    ("safety:hate_speech", "0.03", "safety-hate-speech-v1"),
    ("safety:explicit", "0.01", "safety-explicit-v1"),
    ("safety:harassment", "0.02", "safety-harassment-v1"),
    ("safety:violence", "0.0", "safety-violence-v1"),
    ("safety:self_harm", "0.0", "safety-self-harm-v1"),
];

fn name(value: &str) -> NcName {
    NcName::try_from(value).expect("valid attribute name")
}

fn with_attrs(builder: minidom::ElementBuilder, attrs: &[(&str, &str)]) -> Element {
    attrs
        .iter()
        .fold(builder, |builder, (key, value)| {
            builder.attr(name(key), *value)
        })
        .build()
}

fn score(attrs: &[(&str, &str)]) -> Element {
    with_attrs(Element::builder("score", NS_WADDLE_SAFETY_SCORES), attrs)
}

fn full_score((category, probability, taxonomy): (&str, &str, &str)) -> Element {
    score(&[
        ("category", category),
        ("probability", probability),
        ("taxonomy-version", taxonomy),
    ])
}

fn safety_scores_in(ns: &str, attrs: &[(&str, &str)], scores: Vec<Element>) -> Element {
    let mut payload = with_attrs(Element::builder("safety-scores", ns), attrs);
    for child in scores {
        payload.append_child(child);
    }
    payload
}

fn safety_scores(scores: Vec<Element>) -> Element {
    safety_scores_in(NS_WADDLE_SAFETY_SCORES, &[("model-version", MODEL)], scores)
}

fn contract_payload() -> Element {
    safety_scores(CONTRACT_SCORES.into_iter().map(full_score).collect())
}

fn apply_to(attrs: &[(&str, &str)], payload: Element) -> Element {
    let mut apply_to = with_attrs(Element::builder("apply-to", NS_FASTEN), attrs);
    apply_to.append_child(payload);
    apply_to
}

fn message(message_type: &str, child: Element) -> Element {
    let mut message = with_attrs(
        Element::builder("message", NS_CLIENT),
        &[
            ("from", "room@conference.example.org"),
            ("to", "alice@example.org/web"),
            ("type", message_type),
        ],
    );
    message.append_child(child);
    message
}

fn inbound(element: &Element) -> InboundMessage {
    match parse(element) {
        Some(MessagingEvent::Message(message)) => *message,
        other => panic!("expected an inbound message, got {other:?}"),
    }
}

fn fastening(element: &Element) -> Option<SafetyScoresFastening> {
    inbound(element).safety_scores
}

fn replaced(element: &Element) -> (String, SafetyScores) {
    match fastening(element) {
        Some(SafetyScoresFastening {
            target_id,
            update: SafetyScoresUpdate::Replace(scores),
        }) => (target_id.as_str().to_owned(), scores),
        other => panic!("expected a replace fastening, got {other:?}"),
    }
}

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
    let element = message(
        "groupchat",
        apply_to(&[("id", "stanza-1")], contract_payload()),
    );
    assert!(
        inbound(&element).body.is_none(),
        "a fastening carries no body"
    );

    let (target, scores) = replaced(&element);
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
    let element = message("chat", apply_to(&[("id", "dm-stanza")], contract_payload()));
    let (target, scores) = replaced(&element);
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
    let element = message(
        "groupchat",
        apply_to(
            &[("id", "t")],
            safety_scores(vec![
                full_score(("safety:spam", "0.5", "safety-spam-v1")),
                full_score(("is_question", "0.4", "is-question-v1")),
            ]),
        ),
    );
    let (_, scores) = replaced(&element);
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
    let element = message(
        "groupchat",
        apply_to(
            &[("id", "t")],
            safety_scores(vec![
                full_score(("safety:hate_speech", "1.5", "v")),
                full_score(("safety:explicit", "NaN", "v")),
                full_score(("safety:harassment", "-0.1", "v")),
                full_score(("safety:violence", "high", "v")),
                score(&[("category", "safety:self_harm"), ("probability", "0.2")]),
                score(&[("probability", "0.2"), ("taxonomy-version", "v")]),
                score(&[("category", "is_question"), ("taxonomy-version", "v")]),
                full_score(("safety:violence", "1", "safety-violence-v1")),
            ]),
        ),
    );
    let (_, scores) = replaced(&element);
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
    let element = message(
        "groupchat",
        apply_to(
            &[("id", "t")],
            safety_scores(vec![
                full_score(("is_question", "0.1", "is-question-v1")),
                full_score(("is_question", "0.9", "is-question-v2")),
            ]),
        ),
    );
    let (_, scores) = replaced(&element);
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
    let element = message("groupchat", apply_to(&[("id", "t")], safety_scores(vec![])));
    let (_, scores) = replaced(&element);
    assert!(scores.scores.is_empty());
}

#[test]
fn clear_true_removes_the_fastening() {
    let element = message(
        "groupchat",
        apply_to(
            &[("id", "t"), ("clear", "true")],
            safety_scores_in(NS_WADDLE_SAFETY_SCORES, &[], vec![]),
        ),
    );
    let parsed = fastening(&element).expect("clear fastening parses");
    assert_eq!(parsed.target_id.as_str(), "t");
    assert_eq!(parsed.update, SafetyScoresUpdate::Clear);
}

#[test]
fn encryption_shell_is_ignored() {
    let element = message(
        "groupchat",
        apply_to(&[("id", "t"), ("shell", "true")], contract_payload()),
    );
    assert_eq!(fastening(&element), None);
}

#[test]
fn missing_or_empty_target_id_is_rejected() {
    assert_eq!(
        fastening(&message("groupchat", apply_to(&[], contract_payload()))),
        None
    );
    assert_eq!(
        fastening(&message(
            "groupchat",
            apply_to(&[("id", "")], contract_payload())
        )),
        None
    );
}

#[test]
fn missing_model_version_is_rejected() {
    let payload = safety_scores_in(
        NS_WADDLE_SAFETY_SCORES,
        &[],
        CONTRACT_SCORES.into_iter().map(full_score).collect(),
    );
    assert_eq!(
        fastening(&message("groupchat", apply_to(&[("id", "t")], payload))),
        None
    );
}

#[test]
fn other_fastening_types_are_not_safety_scores() {
    let ended = with_attrs(
        Element::builder("call-thread-ended", NS_CALL_THREAD),
        &[("ended", "2026-06-07T15:00:00Z"), ("duration", "PT30M")],
    );
    let parsed = inbound(&message("groupchat", apply_to(&[("id", "anchor")], ended)));
    assert!(parsed.safety_scores.is_none());
    assert!(parsed.call_thread_ended.is_some());
}

#[test]
fn payload_in_the_wrong_namespace_is_ignored() {
    let payload = safety_scores_in(
        "urn:waddle:safety-scores:0",
        &[("model-version", MODEL)],
        CONTRACT_SCORES.into_iter().map(full_score).collect(),
    );
    assert_eq!(
        fastening(&message("groupchat", apply_to(&[("id", "t")], payload))),
        None
    );
}

#[test]
fn unwrapped_payload_is_not_a_fastening() {
    assert_eq!(fastening(&message("groupchat", contract_payload())), None);
}
