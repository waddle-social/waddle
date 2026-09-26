//! Dedicated conformance test for the `urn:waddle:safety-scores:1` XEP-0422
//! fastening (issue #1831 Phase B): proves the SERVER-side builder
//! (`waddle_xmpp::xep::xep_waddle_safety_scores`) and the already-merged
//! CLIENT-side parser (`waddle_xmpp_client::xep::safety_scores`, shipped in
//! the Apple/Android/web client PRs #1849/#1850/#1851) agree on the wire
//! shape byte-for-byte — not merely by written-down convention. The two
//! sides are independently maintained in separate crates with no shared
//! dependency edge (`waddle-xmpp-client` depends only on
//! `waddle-xmpp-core`), so this is a real cross-implementation round trip,
//! taken as a dev-dependency here for exactly this test.

use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_xmpp::xep::xep_waddle_safety_scores::{
    build_safety_scores_fastening_message, SafetyScoreToSend, SafetyScoresToSend,
};
use waddle_xmpp_client::xep::safety_scores::{
    parse_room_safety_scores_child, SafetyCategory, SafetyScoresAction,
};

fn scores() -> SafetyScoresToSend {
    SafetyScoresToSend {
        model_version: "typesafe/jev-1.13-20260917".to_string(),
        scores: vec![
            SafetyScoreToSend {
                category: "is_question".to_string(),
                probability: 0.92,
                taxonomy_version: "is-question-v1".to_string(),
            },
            SafetyScoreToSend {
                category: "safety:hate_speech".to_string(),
                probability: 0.03,
                taxonomy_version: "safety-hate-speech-v1".to_string(),
            },
        ],
    }
}

#[test]
fn client_parser_accepts_the_server_builder_output_from_the_bare_room_jid() {
    let room: jid::BareJid = "waddlers@conference.example.test"
        .parse()
        .expect("room jid");
    let message = build_safety_scores_fastening_message(room, "archived-stanza-42", &scores());
    // The builder itself sets `from` to the bare room JID and
    // `type='groupchat'` — the exact authority rule
    // `parse_room_safety_scores_child` enforces — with no test-side
    // override needed.
    let element = Element::from(message);
    assert_eq!(
        element.attr("from"),
        Some("waddlers@conference.example.test")
    );
    assert_eq!(element.attr("type"), Some("groupchat"));

    let fastening =
        parse_room_safety_scores_child(&element).expect("client parser must accept the stanza");
    assert_eq!(fastening.target_id.as_str(), "archived-stanza-42");
    let SafetyScoresAction::Apply(parsed) = fastening.action else {
        panic!("expected an Apply action, got Clear");
    };
    assert_eq!(parsed.model_version.as_str(), "typesafe/jev-1.13-20260917");
    assert_eq!(parsed.scores.len(), 2);
    assert_eq!(parsed.scores[0].category, SafetyCategory::IsQuestion);
    assert_eq!(parsed.scores[0].probability.value(), 0.92);
    assert_eq!(parsed.scores[0].taxonomy_version.as_str(), "is-question-v1");
    assert_eq!(parsed.scores[1].category, SafetyCategory::HateSpeech);
    assert_eq!(parsed.scores[1].probability.value(), 0.03);
}

#[test]
fn client_parser_rejects_the_same_stanza_from_an_occupant_resource() {
    // The exact authority check this payload relies on: a sender JID with
    // a `/resource` (an occupant's own reflected message, or a forged
    // claim) must never be trusted, even though the payload shape is
    // otherwise identical to a genuine room broadcast.
    let room: jid::BareJid = "waddlers@conference.example.test"
        .parse()
        .expect("room jid");
    let message = build_safety_scores_fastening_message(room, "archived-stanza-42", &scores());
    let mut element = Element::from(message);
    element.set_attr(
        minidom::rxml::Namespace::NONE,
        xml_ncname!("from").to_owned(),
        "waddlers@conference.example.test/alice",
    );

    assert!(
        parse_room_safety_scores_child(&element).is_none(),
        "a resource-bearing sender must never be accepted as the trusted room authority"
    );
}

#[test]
fn client_parser_rejects_a_non_groupchat_type() {
    let room: jid::BareJid = "waddlers@conference.example.test"
        .parse()
        .expect("room jid");
    let message = build_safety_scores_fastening_message(room, "archived-stanza-42", &scores());
    let mut element = Element::from(message);
    element.set_attr(
        minidom::rxml::Namespace::NONE,
        xml_ncname!("from").to_owned(),
        "waddlers@conference.example.test",
    );
    element.set_attr(
        minidom::rxml::Namespace::NONE,
        xml_ncname!("type").to_owned(),
        "chat",
    );

    assert!(
        parse_room_safety_scores_child(&element).is_none(),
        "only type='groupchat' is the trusted room-broadcast shape"
    );
}
