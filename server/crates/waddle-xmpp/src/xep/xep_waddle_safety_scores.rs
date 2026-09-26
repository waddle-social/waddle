//! Waddle per-message safety scores, fastened to an already-sent message
//! with XEP-0422 Message Fastening (issue #1831 Phase B).
//!
//! ```xml
//! <message type='groupchat' from='room@conference.example.com'>
//!   <apply-to xmlns='urn:xmpp:fasten:0' id='TARGET_ID'>
//!     <safety-scores xmlns='urn:waddle:safety-scores:1' model-version='…'>
//!       <score category='is_question' probability='0.92' taxonomy-version='is-question-v1'/>
//!     </safety-scores>
//!   </apply-to>
//! </message>
//! ```
//!
//! This is the send side of the wire shape the already-merged Apple/Android/
//! web client parsers implement (`waddle_xmpp_client::xep::safety_scores`).
//! The two are independently maintained (separate crates, no shared
//! dependency edge — `waddle-xmpp-client` depends only on
//! `waddle-xmpp-core`), so this module's own conformance test
//! (`tests/xep_waddle_safety_scores.rs`) builds a message here and parses it
//! with the client crate's exact `parse_room_safety_scores_child`, taken as
//! a dev-dependency, to prove the two independently-implemented sides agree
//! on the wire shape byte-for-byte rather than merely by written-down
//! convention.
//!
//! No XEP defines a moderation/judgment-score payload, so the payload lives
//! in a `urn:waddle:*` namespace; only the `<apply-to/>` wrapper is
//! XEP-0422. Authority rule (matched exactly on the client's
//! `parse_room_safety_scores_child`): only a `type='groupchat'` stanza from
//! the bare room JID (no `/resource`) is trusted, so this module always
//! builds exactly that shape — the host never emits this fastening from a
//! resource-bearing (occupant or bot) sender.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::message::{Message, MessageType};

/// `urn:xmpp:fasten:0` — XEP-0422 Message Fastening.
pub const NS_FASTEN: &str = "urn:xmpp:fasten:0";

/// `urn:waddle:safety-scores:1` — the fastened safety-score payload.
pub const NS_WADDLE_SAFETY_SCORES: &str = "urn:waddle:safety-scores:1";

/// One score to send, matching `waddle_xmpp_client::xep::safety_scores::SafetyScore`
/// field-for-field. `category` and `taxonomy_version` are opaque wire tokens
/// here — the caller (the extension job outbox) is responsible for using
/// tokens the client's `SafetyCategory::from_wire` recognizes; an unknown
/// token is not a protocol violation (XEP-0422 children may carry app data
/// the receiver does not understand), it is simply skipped by the parser.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScoreToSend {
    pub category: String,
    /// Must be finite and within `0.0..=1.0`; the caller is responsible for
    /// this (typically already enforced by a typed probability value
    /// upstream, e.g. `waddle_extensions::JudgmentProbability`).
    pub probability: f64,
    pub taxonomy_version: String,
}

/// One judgment batch to send, matching
/// `waddle_xmpp_client::xep::safety_scores::SafetyScores` field-for-field.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyScoresToSend {
    pub model_version: String,
    pub scores: Vec<SafetyScoreToSend>,
}

/// Build the full room-broadcast message: `type='groupchat'`, `from` the
/// bare room JID (never a `/resource` — see the module's authority-rule
/// doc), carrying one `<apply-to><safety-scores>…</safety-scores></apply-to>`
/// fastening targeting `target_stanza_id`.
pub fn build_safety_scores_fastening_message(
    from_room: BareJid,
    target_stanza_id: &str,
    scores: &SafetyScoresToSend,
) -> Message {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.from = Some(jid::Jid::from(from_room));
    msg.type_ = MessageType::Groupchat;
    msg.id = Some(xmpp_parsers::message::Id(uuid::Uuid::new_v4().to_string()));
    msg.payloads
        .push(build_apply_to_element(target_stanza_id, scores));
    msg
}

/// Build just the `<apply-to/>` payload element, for callers that already
/// have a `Message` to attach it to (e.g. a caller that also wants to reuse
/// an existing XEP-0359 stanza-id builder on the same message).
pub fn build_apply_to_element(target_stanza_id: &str, scores: &SafetyScoresToSend) -> Element {
    Element::builder("apply-to", NS_FASTEN)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            target_stanza_id,
        )
        .append(build_safety_scores_element(scores))
        .build()
}

fn build_safety_scores_element(scores: &SafetyScoresToSend) -> Element {
    let mut builder = Element::builder("safety-scores", NS_WADDLE_SAFETY_SCORES).attr(
        minidom::rxml::xml_ncname!("model-version").to_owned(),
        scores.model_version.as_str(),
    );
    for score in &scores.scores {
        builder = builder.append(build_score_element(score));
    }
    builder.build()
}

fn build_score_element(score: &SafetyScoreToSend) -> Element {
    Element::builder("score", NS_WADDLE_SAFETY_SCORES)
        .attr(
            minidom::rxml::xml_ncname!("category").to_owned(),
            score.category.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("probability").to_owned(),
            score.probability.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("taxonomy-version").to_owned(),
            score.taxonomy_version.as_str(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    probability: 0.01,
                    taxonomy_version: "safety-hate-speech-v1".to_string(),
                },
            ],
        }
    }

    #[test]
    fn builds_groupchat_message_from_bare_room_jid() {
        let room: BareJid = "room@conference.example.test".parse().expect("room jid");
        let msg = build_safety_scores_fastening_message(room.clone(), "target-1", &scores());

        assert_eq!(msg.type_, MessageType::Groupchat);
        assert_eq!(msg.from, Some(jid::Jid::from(room)));
        assert!(
            msg.from
                .as_ref()
                .is_some_and(|jid| !jid.to_string().contains('/')),
            "sender must be a bare JID, matching the client's authority rule"
        );
    }

    #[test]
    fn apply_to_carries_target_id_and_every_score() {
        let msg = build_safety_scores_fastening_message(
            "room@conference.example.test".parse().expect("room jid"),
            "target-42",
            &scores(),
        );
        let apply_to = msg
            .payloads
            .iter()
            .find(|el| el.is("apply-to", NS_FASTEN))
            .expect("apply-to present");
        assert_eq!(apply_to.attr("id"), Some("target-42"));
        assert!(apply_to.attr("clear").is_none());
        assert!(apply_to.attr("shell").is_none());

        let payload = apply_to
            .children()
            .find(|el| el.is("safety-scores", NS_WADDLE_SAFETY_SCORES))
            .expect("safety-scores present");
        assert_eq!(
            payload.attr("model-version"),
            Some("typesafe/jev-1.13-20260917")
        );
        let score_elements: Vec<&Element> = payload
            .children()
            .filter(|el| el.is("score", NS_WADDLE_SAFETY_SCORES))
            .collect();
        assert_eq!(score_elements.len(), 2);
        assert_eq!(score_elements[0].attr("category"), Some("is_question"));
        assert_eq!(score_elements[0].attr("probability"), Some("0.92"));
        assert_eq!(
            score_elements[0].attr("taxonomy-version"),
            Some("is-question-v1")
        );
        assert_eq!(
            score_elements[1].attr("category"),
            Some("safety:hate_speech")
        );
    }
}
