//! XEP-0422 `urn:waddle:safety-scores:1` FFI suite: the fastening
//! survives both the live (`inbound_to_ffi`) and the archive
//! (`archived_to_ffi`) conversions with its typed shape intact.

use minidom::Element;
use waddle_xmpp_client::messaging::{self, MessagingEvent};
use waddle_xmpp_client::InboundMessage;

use crate::convert::{archived_to_ffi, inbound_to_ffi};
use crate::{WaddleSafetyCategory, WaddleSafetyScore, WaddleSafetyScoresAction};

fn parse_message(xml: &str) -> InboundMessage {
    let stanza: Element = xml.parse().expect("fixture parses");
    match messaging::parse(&stanza) {
        Some(MessagingEvent::Message(message)) => *message,
        other => panic!("expected a message, got {other:?}"),
    }
}

fn parse_mam_archived(xml: &str) -> waddle_xmpp_client::ArchivedMessage {
    let stanza: Element = xml.parse().expect("fixture parses");
    waddle_xmpp_client::mam::parse_mam_result(&stanza).expect("expected a MAM result")
}

#[test]
fn live_fastening_maps_every_score() {
    let ffi = inbound_to_ffi(parse_message(
        "<message xmlns='jabber:client' type='groupchat' from='room@conference.example.org'>\
           <apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>\
             <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='typesafe/jev-1.13-20260917'>\
               <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>\
               <score category='safety:hate_speech' probability='0.03' taxonomy-version='safety-hate-speech-v1'/>\
               <score category='safety:explicit' probability='0.01' taxonomy-version='safety-explicit-v1'/>\
               <score category='safety:harassment' probability='0.02' taxonomy-version='safety-harassment-v1'/>\
               <score category='safety:violence' probability='0.0' taxonomy-version='safety-violence-v1'/>\
               <score category='safety:self_harm' probability='0.0' taxonomy-version='safety-self-harm-v1'/>\
               <score category='safety:spam' probability='0.5' taxonomy-version='safety-spam-v1'/>\
             </safety-scores>\
           </apply-to>\
         </message>",
    ));
    let fastening = ffi.safety_scores.expect("fastening survives conversion");
    assert_eq!(fastening.target_id, "stanza-1");
    let WaddleSafetyScoresAction::Apply { scores } = fastening.action else {
        panic!("expected scores");
    };
    assert_eq!(scores.model_version, "typesafe/jev-1.13-20260917");
    let score = |category, probability: f64, taxonomy: &str| WaddleSafetyScore {
        category,
        probability,
        taxonomy_version: taxonomy.to_owned(),
    };
    assert_eq!(
        scores.scores,
        vec![
            score(WaddleSafetyCategory::IsQuestion, 0.92, "is-question-v1"),
            score(
                WaddleSafetyCategory::HateSpeech,
                0.03,
                "safety-hate-speech-v1"
            ),
            score(WaddleSafetyCategory::Explicit, 0.01, "safety-explicit-v1"),
            score(
                WaddleSafetyCategory::Harassment,
                0.02,
                "safety-harassment-v1"
            ),
            score(WaddleSafetyCategory::Violence, 0.0, "safety-violence-v1"),
            score(WaddleSafetyCategory::SelfHarm, 0.0, "safety-self-harm-v1"),
        ]
    );
}

#[test]
fn live_clear_maps_to_clear() {
    let ffi = inbound_to_ffi(parse_message(
        "<message xmlns='jabber:client' type='groupchat' from='room@conference.example.org'>\
           <apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1' clear='true'>\
             <safety-scores xmlns='urn:waddle:safety-scores:1'/>\
           </apply-to>\
         </message>",
    ));
    let fastening = ffi.safety_scores.expect("clear survives conversion");
    assert_eq!(fastening.target_id, "stanza-1");
    assert_eq!(fastening.action, WaddleSafetyScoresAction::Clear);
}

#[test]
fn a_plain_message_carries_no_fastening() {
    let ffi = inbound_to_ffi(parse_message(
        "<message xmlns='jabber:client' type='groupchat' from='room@conference.example.org/bob'>\
           <body>hello</body>\
         </message>",
    ));
    assert!(ffi.safety_scores.is_none());
}

#[test]
fn archived_fastening_survives_conversion() {
    let archived = archived_to_ffi(parse_mam_archived(
        "<message xmlns='jabber:client'>\
           <result xmlns='urn:xmpp:mam:2' id='mam-scores' queryid='q1'>\
             <forwarded xmlns='urn:xmpp:forward:0'>\
               <delay xmlns='urn:xmpp:delay' stamp='2026-09-25T12:00:00Z'/>\
               <message xmlns='jabber:client' type='groupchat' from='room@conference.example.org'>\
                 <apply-to xmlns='urn:xmpp:fasten:0' id='stanza-1'>\
                   <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='m1'>\
                     <score category='safety:violence' probability='0.7' taxonomy-version='safety-violence-v1'/>\
                   </safety-scores>\
                 </apply-to>\
               </message>\
             </forwarded>\
           </result>\
         </message>",
    ))
    .expect("archived fastening row converts");
    let fastening = archived
        .safety_scores
        .expect("fastening survives archive conversion");
    assert_eq!(fastening.target_id, "stanza-1");
    let WaddleSafetyScoresAction::Apply { scores } = fastening.action else {
        panic!("expected scores");
    };
    assert_eq!(scores.model_version, "m1");
    assert_eq!(scores.scores.len(), 1);
    assert_eq!(scores.scores[0].category, WaddleSafetyCategory::Violence);
    assert_eq!(scores.scores[0].probability, 0.7);
}
