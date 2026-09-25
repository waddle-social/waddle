//! XEP-0422 Message Fastening dedicated client suite, for the one
//! fastening payload the client consumes: `urn:waddle:safety-scores:1`.
//!
//! XEP-0422 obligations covered: the `<apply-to id='…'/>` target is
//! surfaced; a message carrying more than one `<apply-to/>` is rejected
//! (§Business Rules); a `shell='true'` apply-to is ignored (§Interaction
//! with stanza encryption); `clear='true'` with an empty fastening clears
//! (§Removing fastenings) while a non-empty one is rejected; unknown
//! children of `<apply-to/>` are ignored (§External Payloads).
//!
//! Payload obligations covered: batch-level `model-version` is required;
//! unknown categories and malformed scores are skipped without failing
//! the batch; a repeated category keeps its first score.

use minidom::Element;
use waddle_xmpp_client::messaging::{parse, MessagingEvent, NS_CLIENT};
use waddle_xmpp_client::xep::safety_scores::{
    parse_safety_scores_fastening, SafetyCategory, SafetyScores, SafetyScoresAction,
    SafetyScoresFastening, NS_FASTEN, NS_WADDLE_SAFETY_SCORES,
};

const FULL_BATCH: &str = r#"<message xmlns='jabber:client' from='room@conference.example.org' to='alice@example.org/phone' type='groupchat'>
  <apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
    <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='typesafe/jev-1.13-20260917'>
      <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>
      <score category='safety:hate_speech' probability='0.03' taxonomy-version='safety-hate-speech-v1'/>
      <score category='safety:explicit' probability='0.01' taxonomy-version='safety-explicit-v1'/>
      <score category='safety:harassment' probability='0.02' taxonomy-version='safety-harassment-v1'/>
      <score category='safety:violence' probability='0.0' taxonomy-version='safety-violence-v1'/>
      <score category='safety:self_harm' probability='0.0' taxonomy-version='safety-self-harm-v1'/>
    </safety-scores>
  </apply-to>
</message>"#;

fn element(xml: &str) -> Element {
    xml.parse().expect("fixture XML parses")
}

fn fastening(xml: &str) -> Option<SafetyScoresFastening> {
    parse_safety_scores_fastening(&element(xml))
}

/// A room message whose children are `children`, each a standalone XML
/// fragment, assembled with the element builder rather than by string
/// concatenation.
fn room_message(children: &[&str]) -> Element {
    children
        .iter()
        .fold(
            Element::builder("message", NS_CLIENT)
                .attr(
                    minidom::rxml::xml_ncname!("from").to_owned(),
                    "room@conference.example.org",
                )
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat"),
            |builder, child| builder.append(element(child)),
        )
        .build()
}

fn fastened(children: &[&str]) -> Option<SafetyScoresFastening> {
    parse_safety_scores_fastening(&room_message(children))
}

fn applied(apply_to: &str) -> SafetyScores {
    match fastened(&[apply_to]).expect("fastening parses").action {
        SafetyScoresAction::Apply(scores) => scores,
        SafetyScoresAction::Clear => panic!("expected scores, got a clear"),
    }
}

#[test]
fn namespaces_match_the_contract() {
    // XEP-0422 §Namespace registration.
    assert_eq!(NS_FASTEN, "urn:xmpp:fasten:0");
    assert_eq!(NS_WADDLE_SAFETY_SCORES, "urn:waddle:safety-scores:1");
}

#[test]
fn parses_every_category_of_a_full_batch() {
    let parsed = fastening(FULL_BATCH).expect("fastening parses");
    assert_eq!(parsed.target_id.as_str(), "stanza-1");
    let SafetyScoresAction::Apply(scores) = parsed.action else {
        panic!("expected scores");
    };
    assert_eq!(scores.model_version.as_str(), "typesafe/jev-1.13-20260917");
    let summary: Vec<(SafetyCategory, f64, &str)> = scores
        .scores
        .iter()
        .map(|score| {
            (
                score.category,
                score.probability.value(),
                score.taxonomy_version.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (SafetyCategory::IsQuestion, 0.92, "is-question-v1"),
            (SafetyCategory::HateSpeech, 0.03, "safety-hate-speech-v1"),
            (SafetyCategory::Explicit, 0.01, "safety-explicit-v1"),
            (SafetyCategory::Harassment, 0.02, "safety-harassment-v1"),
            (SafetyCategory::Violence, 0.0, "safety-violence-v1"),
            (SafetyCategory::SelfHarm, 0.0, "safety-self-harm-v1"),
        ]
    );
}

#[test]
fn category_tokens_round_trip_the_server_judgment_names() {
    for (category, token) in [
        (SafetyCategory::IsQuestion, "is_question"),
        (SafetyCategory::HateSpeech, "safety:hate_speech"),
        (SafetyCategory::Explicit, "safety:explicit"),
        (SafetyCategory::Harassment, "safety:harassment"),
        (SafetyCategory::Violence, "safety:violence"),
        (SafetyCategory::SelfHarm, "safety:self_harm"),
    ] {
        assert_eq!(category.as_wire(), token);
        assert_eq!(SafetyCategory::from_wire(token), Some(category));
    }
    assert_eq!(SafetyCategory::from_wire("safety:spam"), None);
}

#[test]
fn the_messaging_parser_surfaces_the_fastening() {
    let Some(MessagingEvent::Message(message)) = parse(&element(FULL_BATCH)) else {
        panic!("expected a message event");
    };
    let fastening = message.safety_scores.expect("safety scores surfaced");
    assert_eq!(fastening.target_id.as_str(), "stanza-1");
    assert!(message.body.is_none());
}

#[test]
fn direct_messages_use_the_same_shape() {
    let apply_to = element(FULL_BATCH)
        .get_child("apply-to", NS_FASTEN)
        .expect("fixture carries an apply-to")
        .clone();
    let message = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), "example.org")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .append(apply_to)
        .build();
    assert!(parse_safety_scores_fastening(&message).is_some());
}

#[test]
fn unknown_categories_are_skipped_not_fatal() {
    let scores = applied(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>
             <score category='safety:spam' probability='0.5' taxonomy-version='safety-spam-v1'/>
             <score category='is_question' probability='0.4' taxonomy-version='is-question-v1'/>
           </safety-scores>
         </apply-to>",
    );
    assert_eq!(scores.scores.len(), 1);
    assert_eq!(scores.scores[0].category, SafetyCategory::IsQuestion);
}

#[test]
fn a_batch_of_only_unknown_categories_is_still_a_batch() {
    let scores = applied(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>
             <score category='safety:spam' probability='0.5' taxonomy-version='safety-spam-v1'/>
           </safety-scores>
         </apply-to>",
    );
    assert!(scores.scores.is_empty());
}

#[test]
fn malformed_scores_are_skipped() {
    let scores = applied(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>
             <score category='safety:explicit' probability='1.5' taxonomy-version='v1'/>
             <score category='safety:violence' probability='-0.1' taxonomy-version='v1'/>
             <score category='safety:harassment' probability='NaN' taxonomy-version='v1'/>
             <score category='safety:self_harm' probability='high' taxonomy-version='v1'/>
             <score category='safety:hate_speech' taxonomy-version='v1'/>
             <score category='is_question' probability='0.5'/>
             <score category='is_question' probability='0.5' taxonomy-version='  '/>
             <score probability='0.5' taxonomy-version='v1'/>
             <score category='safety:explicit' probability='1' taxonomy-version='safety-explicit-v1'/>
           </safety-scores>
         </apply-to>",
    );
    assert_eq!(scores.scores.len(), 1);
    assert_eq!(scores.scores[0].category, SafetyCategory::Explicit);
    assert_eq!(scores.scores[0].probability.value(), 1.0);
}

#[test]
fn a_repeated_category_keeps_its_first_score() {
    let scores = applied(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>
             <score category='is_question' probability='0.1' taxonomy-version='is-question-v1'/>
             <score category='is_question' probability='0.9' taxonomy-version='is-question-v1'/>
           </safety-scores>
         </apply-to>",
    );
    assert_eq!(scores.scores.len(), 1);
    assert_eq!(scores.scores[0].probability.value(), 0.1);
}

#[test]
fn scores_in_a_foreign_namespace_are_ignored() {
    let scores = applied(
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>
             <score xmlns='urn:example:other' category='is_question' probability='0.4' taxonomy-version='v1'/>
           </safety-scores>
         </apply-to>",
    );
    assert!(scores.scores.is_empty());
}

#[test]
fn a_payload_in_a_foreign_namespace_is_not_safety_scores() {
    // The `<safety-scores/>` qualified name is namespace-scoped: a
    // same-named element in a different (or unversioned) namespace is not
    // our payload, and the fastening carries none.
    assert!(
        fastened(&["<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:0' model-version='m1'>
             <score category='is_question' probability='0.4' taxonomy-version='v1'/>
           </safety-scores>
         </apply-to>",])
        .is_none()
    );
}

#[test]
fn a_batch_without_model_version_is_rejected() {
    assert!(
        fastened(&["<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1'>
             <score category='is_question' probability='0.4' taxonomy-version='v1'/>
           </safety-scores>
         </apply-to>"])
        .is_none()
    );
}

#[test]
fn an_apply_to_without_a_target_is_rejected() {
    for apply_to in [
        "<apply-to xmlns='urn:xmpp:fasten:0'><safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/></apply-to>",
        "<apply-to xmlns='urn:xmpp:fasten:0' id=' '><safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/></apply-to>",
    ] {
        assert!(fastened(&[apply_to]).is_none());
    }
}

#[test]
fn two_apply_to_elements_are_rejected() {
    // XEP-0422 §Business Rules: a message cannot be fastened to several
    // messages.
    assert!(fastened(&[
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/>
         </apply-to>",
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-2'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/>
         </apply-to>",
    ])
    .is_none());
}

#[test]
fn two_payloads_in_one_apply_to_are_rejected() {
    assert!(
        fastened(&["<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m2'/>
         </apply-to>"])
        .is_none()
    );
}

#[test]
fn a_shell_apply_to_is_ignored() {
    // XEP-0422 §Interaction with stanza encryption: the shell carries no
    // content; the real apply-to beside it is used.
    let parsed = fastened(&[
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1' shell='true'/>",
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/>
         </apply-to>",
    ])
    .expect("the non-shell apply-to is used");
    assert!(matches!(parsed.action, SafetyScoresAction::Apply(_)));

    assert!(
        fastened(&["<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1' shell='true'/>"]).is_none()
    );
}

#[test]
fn clear_with_an_empty_fastening_clears() {
    for apply_to in [
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1' clear='true'>
           <safety-scores xmlns='urn:waddle:safety-scores:1'/>
         </apply-to>",
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1' clear='1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1'/>
         </apply-to>",
    ] {
        let parsed = fastened(&[apply_to]).expect("clear parses");
        assert_eq!(parsed.target_id.as_str(), "stanza-1");
        assert_eq!(parsed.action, SafetyScoresAction::Clear);
    }
}

#[test]
fn clear_with_a_non_empty_fastening_is_rejected() {
    // XEP-0422 §Removing: the fastening is sent with no attributes and no
    // children.
    for apply_to in [
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1' clear='true'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/>
         </apply-to>",
        "<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1' clear='true'>
           <safety-scores xmlns='urn:waddle:safety-scores:1'>
             <score category='is_question' probability='0.4' taxonomy-version='v1'/>
           </safety-scores>
         </apply-to>",
    ] {
        assert!(fastened(&[apply_to]).is_none());
    }
}

#[test]
fn unknown_apply_to_children_are_ignored() {
    // XEP-0422 §External Payloads: unknown namespaced children of
    // <apply-to/> are ignored.
    assert!(
        fastened(&["<apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>
           <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'/>
           <future xmlns='urn:example:future'/>
         </apply-to>"])
        .is_some()
    );
}

#[test]
fn other_fastenings_are_not_safety_scores() {
    assert!(fastened(&["<apply-to xmlns='urn:xmpp:fasten:0' id='anchor'>
           <call-thread-ended xmlns='urn:waddle:call-thread:0' ended='2026-06-07T14:35:00Z' duration='PT5M'/>
         </apply-to>"])
    .is_none());
    assert!(
        fastening("<message xmlns='jabber:client' type='groupchat'><body>hi</body></message>")
            .is_none()
    );
}
