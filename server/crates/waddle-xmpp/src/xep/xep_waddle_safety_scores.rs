//! Server broadcast of per-message community/safety judgments as a
//! XEP-0422 Message Fastening (`urn:waddle:safety-scores:1`).
//!
//! This is the send side only. Client-side receive/parse/display for this
//! exact wire shape already ships in `waddle-xmpp-client`'s
//! `xep::safety_scores` (consumed by the Apple, Android, and web clients);
//! this module produces the stanza the `message_judgment_outbox` drain
//! worker broadcasts once a judgment is durably recorded.
//!
//! Only the bare room JID may send this payload (`type='groupchat'`, `from`
//! carrying no resource) — the same authority rule every client-side parser
//! already enforces (see `parse_room_safety_scores_child` on the client
//! crate), so a mismatch here would silently produce a fastening no client
//! accepts.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::message::{Message, MessageType};

use super::xep0334::{build_hint_element, Hint};
use super::xep_waddle_call_thread::NS_FASTEN;

pub const NS_WADDLE_SAFETY_SCORES: &str = "urn:waddle:safety-scores:1";

/// One judgment category's result, ready to serialize as a `<score/>`
/// child. `category` must be the server's canonical judgment-name token
/// (e.g. `is_question`, `safety:hate_speech`) — the same value stored in
/// `message_judgments.judgment_name` and the value every client parser
/// matches against.
pub struct SafetyScoreToSend<'a> {
    pub category: &'a str,
    pub probability: f64,
    pub taxonomy_version: &'a str,
}

/// Builds the room-broadcast message carrying a safety-scores fastening
/// for the message identified by `target_stanza_id` (the room-assigned
/// XEP-0359 stanza-id of the judged message — the same identifier every
/// client-side parser resolves against).
pub fn build_safety_scores_message(
    room_jid: &BareJid,
    target_stanza_id: &str,
    model_version: &str,
    scores: &[SafetyScoreToSend<'_>],
) -> Message {
    let mut safety_scores_builder = Element::builder("safety-scores", NS_WADDLE_SAFETY_SCORES)
        .attr(
            minidom::rxml::xml_ncname!("model-version").to_owned(),
            model_version,
        );
    for score in scores {
        safety_scores_builder = safety_scores_builder.append(
            Element::builder("score", NS_WADDLE_SAFETY_SCORES)
                .attr(
                    minidom::rxml::xml_ncname!("category").to_owned(),
                    score.category,
                )
                .attr(
                    minidom::rxml::xml_ncname!("probability").to_owned(),
                    format!("{}", score.probability),
                )
                .attr(
                    minidom::rxml::xml_ncname!("taxonomy-version").to_owned(),
                    score.taxonomy_version,
                )
                .build(),
        );
    }
    let apply_to = Element::builder("apply-to", NS_FASTEN)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            target_stanza_id,
        )
        .append(safety_scores_builder.build())
        .build();
    let mut message = Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(jid::Jid::from(room_jid.clone()));
    message.type_ = MessageType::Groupchat;
    message.payloads.push(apply_to);
    message.payloads.push(build_hint_element(Hint::Store));
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> BareJid {
        "room@conference.example.org".parse().expect("room jid")
    }

    #[test]
    fn builds_a_room_groupchat_broadcast_with_every_score() {
        let scores = [
            SafetyScoreToSend {
                category: "is_question",
                probability: 0.92,
                taxonomy_version: "is-question-v1",
            },
            SafetyScoreToSend {
                category: "safety:hate_speech",
                probability: 0.03,
                taxonomy_version: "safety-hate-speech-v1",
            },
        ];
        let message = build_safety_scores_message(
            &room(),
            "judged-stanza-id",
            "typesafe/jev-1.13-20260917",
            &scores,
        );

        assert_eq!(message.type_, MessageType::Groupchat);
        assert_eq!(
            message.from.as_ref().map(|j| j.to_string()),
            Some(room().to_string())
        );
        assert_eq!(
            message.to.as_ref().map(|j| j.to_string()),
            Some(room().to_string())
        );

        let apply_to = message
            .payloads
            .iter()
            .find(|p| p.name() == "apply-to" && p.ns() == NS_FASTEN)
            .expect("apply-to present");
        assert_eq!(apply_to.attr("id"), Some("judged-stanza-id"));
        assert!(apply_to.attr("clear").is_none());

        let safety_scores = apply_to
            .children()
            .find(|c| c.name() == "safety-scores" && c.ns() == NS_WADDLE_SAFETY_SCORES)
            .expect("safety-scores present");
        assert_eq!(
            safety_scores.attr("model-version"),
            Some("typesafe/jev-1.13-20260917")
        );

        let score_elements: Vec<&Element> = safety_scores
            .children()
            .filter(|c| c.name() == "score" && c.ns() == NS_WADDLE_SAFETY_SCORES)
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

        assert!(message
            .payloads
            .iter()
            .any(|p| p.name() == "store" && p.ns() == super::super::xep0334::NS_HINTS));
    }
}
