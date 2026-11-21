//! XEP-0353: Jingle Message Initiation (JMI).
//!
//! JMI is the pre-call ringing layer: it lets the initiator broadcast
//! a `<propose/>` to all of the responder's resources so any one of
//! them can ring and accept before Jingle session-initiate commits to
//! a single resource. Waddle uses JMI as the canonical 1:1 call ring
//! signal.
//!
//! `<proceed/>`, `<reject/>`, and `<retract/>` continue to ride the
//! typed [`xmpp_parsers::jingle_message::JingleMI`] enum since they
//! carry no children. `<propose/>`, `<ringing/>`, and `<finish/>` instead build the
//! element directly via [`minidom`]:
//!
//! - `JingleMI::Propose` accepts only a single `<description/>` child;
//!   XEP-0353 §3 mandates **one `<description/>` per media type**, so
//!   the typed variant is too narrow for an A/V offer. The element
//!   builder uses typed namespace constants and the typed
//!   [`MediaKind`] enum at every call site.
//! - `JingleMI` has no `Finish` variant yet, so `<finish/>` carries an
//!   optional typed [`xmpp_parsers::jingle::Reason`] child built via
//!   the [`Reason`-into-`Element`] conversion (XEP-0353 §3.5 SHOULD).

use minidom::Element;
use xmpp_parsers::jingle::{Reason as JingleReason, ReasonElement, SessionId};
use xmpp_parsers::message::{Message, MessageType};

use super::xep_waddle_call_thread::CallThreadMedia;
pub use xmpp_parsers::jingle_message::JingleMI;

use super::xep0167::{MediaKind, NS_JINGLE_RTP};

pub const NS_JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";
/// XEP-0166 Jingle namespace — used here only for the `<reason/>`
/// wrapper around the typed [`JingleReason`] child of `<finish/>`.
const NS_JINGLE: &str = "urn:xmpp:jingle:1";

/// The set of media kinds offered on a Jingle Message `<propose/>`.
/// XEP-0353 §3 specifies that propose MUST carry one
/// `<description/>` element **per media type**; Waddle attaches an
/// empty `urn:xmpp:jingle:apps:rtp:1` description per offered kind to
/// declare audio/video capability, with the rich codec list emitted
/// in the subsequent session-initiate instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallOffer {
    pub audio: bool,
    pub video: bool,
}

impl CallOffer {
    pub fn audio_only() -> Self {
        Self {
            audio: true,
            video: false,
        }
    }

    pub fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
        }
    }

    /// Iterate the offered media kinds in canonical (audio-then-video)
    /// order. Used by `build_propose` to emit one `<description/>` per
    /// kind without smuggling the audio/video booleans through the
    /// builder.
    fn media_kinds(self) -> impl Iterator<Item = MediaKind> {
        let audio = if self.audio {
            Some(MediaKind::Audio)
        } else {
            None
        };
        let video = if self.video {
            Some(MediaKind::Video)
        } else {
            None
        };
        audio.into_iter().chain(video)
    }
}

/// Recognize a top-level JMI proposal without the single-description
/// limitation of `JingleMI::Propose`. A non-empty session id is required;
/// unrecognized media do not discard the proposal identity.
pub fn jmi_propose(message: &Message) -> Option<(SessionId, CallThreadMedia)> {
    let propose = message.payloads.iter().find(|payload| {
        payload.is("propose", NS_JINGLE_MESSAGE)
            && payload.attr("id").is_some_and(|id| !id.is_empty())
    })?;
    let sid = propose.attr("id").filter(|id| !id.is_empty())?;
    let media = jmi_media(propose).unwrap_or(CallThreadMedia {
        audio: false,
        video: false,
    });
    Some((SessionId(sid.to_owned()), media))
}

/// Recognize a well-formed caller cancellation, excluding glare tie-breaking.
/// A JMI message contains one action; a retract may carry an optional
/// XEP-0166 reason. A tie-breaking retract joins the peer's call instead
/// of representing a missed call (XEP-0353 §4.1).
pub fn jmi_retract(message: &Message) -> Option<SessionId> {
    if message.type_ != MessageType::Chat {
        return None;
    }
    let mut actions = message
        .payloads
        .iter()
        .filter(|payload| payload.ns() == NS_JINGLE_MESSAGE);
    let retract = actions.next()?;
    if actions.next().is_some() || !retract.is("retract", NS_JINGLE_MESSAGE) {
        return None;
    }
    let sid = retract.attr("id").filter(|sid| !sid.is_empty())?;
    if retract
        .attrs()
        .iter()
        .any(|((namespace, name), _)| (namespace.as_ref(), name.as_ref()) != ("", "id"))
        || !retract.text().trim().is_empty()
    {
        return None;
    }
    let mut reason_seen = false;
    for child in retract.children() {
        if !child.is("reason", NS_JINGLE)
            || reason_seen
            || ReasonElement::try_from(child.clone()).is_err()
        {
            return None;
        }
        reason_seen = true;
    }
    Some(SessionId(sid.to_owned()))
}

/// Recognize archived evidence that a call has an outcome. Unlike the
/// cancellation classifier, this accepts action children such as reasons
/// and glare tie-breaking: those still resolve the original proposal.
pub fn jmi_call_outcome(message: &Message) -> Option<SessionId> {
    message.payloads.iter().find_map(|payload| {
        if payload.ns() != NS_JINGLE_MESSAGE
            || !matches!(
                payload.name(),
                "proceed" | "accept" | "reject" | "finish" | "retract"
            )
        {
            return None;
        }
        payload
            .attr("id")
            .filter(|id| !id.is_empty())
            .map(|id| SessionId(id.to_owned()))
    })
}

/// Recognize a bodyless message with a top-level JMI `retract`, `proceed`,
/// `reject`, `finish`, `ringing`, or legacy `accept` and no top-level JMI `propose`.
pub fn is_bodyless_jmi_signal(message: &Message) -> bool {
    message.bodies.is_empty()
        && message.payloads.iter().any(|payload| {
            payload.ns() == NS_JINGLE_MESSAGE
                && matches!(
                    payload.name(),
                    "retract" | "proceed" | "reject" | "finish" | "ringing" | "accept"
                )
        })
        && !message
            .payloads
            .iter()
            .any(|payload| payload.is("propose", NS_JINGLE_MESSAGE))
}

/// Collect all recognized RTP media descriptions on a JMI element.
pub fn jmi_media(element: &Element) -> Option<CallThreadMedia> {
    let mut audio = false;
    let mut video = false;
    for description in element
        .children()
        .filter(|child| child.is("description", NS_JINGLE_RTP))
    {
        match description.attr("media") {
            Some("audio") => audio = true,
            Some("video") => video = true,
            _ => {}
        }
    }
    (audio || video).then_some(CallThreadMedia { audio, video })
}

/// Build a `<propose/>` JMI for an A/V call (XEP-0353 §3). The propose
/// MUST contain **one `<description/>` element per media type** —
/// emitting a single description for a combined A/V offer is a
/// protocol downgrade that prevents the responder from distinguishing
/// audio-only from audio+video at ring time.
///
/// The descriptions are emitted as empty
/// `<description xmlns='urn:xmpp:jingle:apps:rtp:1' media='…'/>` per
/// the XEP-0353 §3 example; the codec list rides the subsequent
/// session-initiate (XEP-0167) where it has somewhere to live.
///
/// XML is constructed via [`minidom::Element::builder`] with the
/// typed `NS_JINGLE_MESSAGE` / `NS_JINGLE_RTP` constants and the typed
/// [`MediaKind`] enum — no `format!`, no string concatenation
/// (CLAUDE.md XML hard rule).
pub fn build_propose(sid: SessionId, offer: CallOffer) -> minidom::Element {
    let mut builder = minidom::Element::builder("propose", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0);
    for kind in offer.media_kinds() {
        builder = builder.append(
            minidom::Element::builder("description", NS_JINGLE_RTP)
                .attr(
                    minidom::rxml::xml_ncname!("media").to_owned(),
                    kind.as_str(),
                )
                .build(),
        );
    }
    builder.build()
}

pub fn build_proceed(sid: SessionId) -> JingleMI {
    JingleMI::Proceed(sid)
}

pub fn build_ringing(sid: SessionId) -> minidom::Element {
    minidom::Element::builder("ringing", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0)
        .build()
}

pub fn build_reject(sid: SessionId) -> JingleMI {
    JingleMI::Reject(sid)
}

pub fn build_retract(sid: SessionId) -> JingleMI {
    JingleMI::Retract(sid)
}

/// Build a `<finish/>` JMI envelope (XEP-0353 §3.5). The envelope
/// signals call termination; both parties SHOULD send it when the
/// call ends. xmpp-parsers does not yet have a typed `Finish` variant
/// on [`JingleMI`], so we build the element directly via [`minidom`].
///
/// XEP-0353 §3.5 says `<finish/>` SHOULD contain a `<reason/>` child
/// (typically `<success/>`, `<expired/>`, or `<connectivity-error/>`)
/// per XEP-0166 §7.4. The reason is taken as a typed
/// [`xmpp_parsers::jingle::Reason`] so callers cannot ship a malformed
/// condition over the wire; `None` omits the `<reason/>` child (still
/// valid — the SHOULD is informational).
pub fn build_finish(sid: SessionId, reason: Option<JingleReason>) -> minidom::Element {
    let mut builder = minidom::Element::builder("finish", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0);
    if let Some(condition) = reason {
        let reason_elem = minidom::Element::builder("reason", NS_JINGLE)
            .append(minidom::Element::from(condition))
            .build();
        builder = builder.append(reason_elem);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[test]
    fn call_outcomes_include_actions_with_reason_and_tie_break_children() {
        for name in ["proceed", "accept", "reject", "finish", "retract"] {
            let mut message = Message::new(None::<jid::Jid>);
            message.payloads.push(
                Element::builder(name, NS_JINGLE_MESSAGE)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call")
                    .append(Element::builder("tie-break", NS_JINGLE_MESSAGE).build())
                    .append(Element::builder("reason", NS_JINGLE).build())
                    .build(),
            );
            assert_eq!(
                jmi_call_outcome(&message),
                Some(SessionId("call".into())),
                "{name}"
            );
        }
    }

    #[test]
    fn call_outcomes_require_top_level_action_namespace_and_nonempty_id() {
        for payload in [
            Element::builder("reject", NS_JINGLE_MESSAGE).build(),
            Element::builder("retract", NS_JINGLE_MESSAGE)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "")
                .build(),
            Element::builder("reject", NS_JINGLE)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call")
                .build(),
            build_propose(SessionId("call".into()), CallOffer::audio_only()),
            build_ringing(SessionId("call".into())),
            Element::builder("wrapper", NS_JINGLE_MESSAGE)
                .append(Element::from(build_reject(SessionId("call".into()))))
                .build(),
        ] {
            let mut message = Message::new(None::<jid::Jid>);
            message.payloads.push(payload);
            assert!(jmi_call_outcome(&message).is_none());
        }
    }

    #[test]
    fn bodyless_jmi_signals_include_each_recognized_element() {
        for name in [
            "retract", "proceed", "reject", "finish", "ringing", "accept",
        ] {
            let mut message = Message::new(None::<jid::Jid>);
            message
                .payloads
                .push(Element::builder(name, NS_JINGLE_MESSAGE).build());
            assert!(is_bodyless_jmi_signal(&message), "{name}");
        }
    }

    #[test]
    fn top_level_jmi_propose_prevents_signal_suppression() {
        let mut message = Message::new(None::<jid::Jid>);
        message.payloads.push(build_propose(
            SessionId("call".into()),
            CallOffer::audio_only(),
        ));
        assert!(!is_bodyless_jmi_signal(&message));
        message
            .payloads
            .push(build_retract(SessionId("call".into())).into());
        assert!(!is_bodyless_jmi_signal(&message));
    }

    #[test]
    fn jmi_signal_with_any_body_is_not_bodyless() {
        for name in [
            "retract", "proceed", "reject", "finish", "ringing", "accept",
        ] {
            let mut message = Message::new(None::<jid::Jid>);
            message
                .payloads
                .push(Element::builder(name, NS_JINGLE_MESSAGE).build());
            message.bodies.insert("no".into(), "call ended".into());
            assert!(!is_bodyless_jmi_signal(&message), "{name}");
            message.bodies.insert("no".into(), "".into());
            assert!(!is_bodyless_jmi_signal(&message), "empty body on {name}");
        }
    }

    #[test]
    fn bodyless_message_without_top_level_jmi_signal_is_not_suppressed() {
        let mut message = Message::new(None::<jid::Jid>);
        assert!(!is_bodyless_jmi_signal(&message));
        message.payloads.push(
            Element::builder("future-signal", NS_JINGLE_MESSAGE)
                .append(Element::builder("retract", NS_JINGLE_MESSAGE).build())
                .build(),
        );
        assert!(!is_bodyless_jmi_signal(&message));
    }

    #[test]
    fn ns_matches_xmpp_parsers() {
        assert_eq!(NS_JINGLE_MESSAGE, xmpp_parsers::ns::JINGLE_MESSAGE);
    }

    #[test]
    fn propose_audio_video_emits_one_description_per_media() {
        // XEP-0353 §3: `<propose/>` MUST contain one `<description/>`
        // element for **each** media type. A single combined
        // description is non-conformant — the receiver can't tell
        // audio-only apart from audio+video before session-initiate.
        let sid = SessionId("c1".to_string());
        let elem = build_propose(sid.clone(), CallOffer::audio_video());
        assert_eq!(elem.name(), "propose");
        assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
        assert_eq!(elem.attr("id"), Some("c1"));

        let media: Vec<_> = elem
            .children()
            .filter(|c| c.name() == "description" && c.ns() == super::super::xep0167::NS_JINGLE_RTP)
            .filter_map(|c| c.attr("media"))
            .collect();
        assert_eq!(media, vec!["audio", "video"], "one description per media");
    }

    #[test]
    fn propose_audio_only_emits_one_audio_description() {
        let elem = build_propose(SessionId("c2".into()), CallOffer::audio_only());
        let descs: Vec<_> = elem
            .children()
            .filter(|c| c.name() == "description" && c.ns() == super::super::xep0167::NS_JINGLE_RTP)
            .collect();
        assert_eq!(descs.len(), 1, "audio-only offers exactly one description");
        assert_eq!(descs[0].attr("media"), Some("audio"));
        // Empty description per the XEP-0353 §3 example; payload list
        // rides session-initiate (XEP-0167).
        assert!(descs[0].children().next().is_none());
    }

    #[test]
    fn propose_audio_video_emits_exactly_two_descriptions() {
        let elem = build_propose(SessionId("c3".into()), CallOffer::audio_video());
        let descs: Vec<_> = elem
            .children()
            .filter(|c| c.name() == "description" && c.ns() == super::super::xep0167::NS_JINGLE_RTP)
            .collect();
        assert_eq!(descs.len(), 2, "audio+video offer carries 2 descriptions");
    }

    #[test]
    fn proceed_round_trips() {
        let proceed = build_proceed(SessionId("c1".into()));
        let elem: Element = proceed.into();
        assert_eq!(elem.name(), "proceed");
        assert_eq!(elem.attr("id"), Some("c1"));
        let reparsed = JingleMI::try_from(elem).unwrap();
        assert!(matches!(reparsed, JingleMI::Proceed(_)));
    }

    #[test]
    fn ringing_has_expected_shape() {
        let elem = build_ringing(SessionId("c1".into()));
        assert_eq!(elem.name(), "ringing");
        assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
        assert_eq!(elem.attr("id"), Some("c1"));
        assert!(elem.children().next().is_none());
    }

    #[test]
    fn reject_round_trips() {
        let reject = build_reject(SessionId("c1".into()));
        let elem: Element = reject.into();
        assert_eq!(elem.name(), "reject");
        assert!(matches!(
            JingleMI::try_from(elem).unwrap(),
            JingleMI::Reject(_)
        ));
    }

    #[test]
    fn retract_round_trips() {
        let retract = build_retract(SessionId("c1".into()));
        let elem: Element = retract.into();
        assert_eq!(elem.name(), "retract");
        assert!(matches!(
            JingleMI::try_from(elem).unwrap(),
            JingleMI::Retract(_)
        ));
    }

    #[test]
    fn finish_without_reason_emits_no_reason_child() {
        let elem = build_finish(SessionId("c1".into()), None);
        assert_eq!(elem.name(), "finish");
        assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
        assert_eq!(elem.attr("id"), Some("c1"));
        assert!(
            elem.children().all(|c| c.name() != "reason"),
            "finish with no reason has no <reason/> child"
        );
    }

    #[test]
    fn finish_with_reason_emits_typed_reason_child() {
        // XEP-0353 §3.5: `<finish/>` SHOULD carry a `<reason/>` child
        // qualified by the Jingle namespace. The condition itself is
        // typed (XEP-0166 §7.4) so a typo cannot ship a malformed
        // reason over the wire.
        let elem = build_finish(SessionId("c1".into()), Some(JingleReason::Success));
        let reason = elem
            .children()
            .find(|c| c.name() == "reason")
            .expect("<reason/> child required by XEP-0353 §3.5 SHOULD");
        assert_eq!(reason.ns(), NS_JINGLE);
        let condition = reason.children().next().expect("reason has condition");
        assert_eq!(condition.name(), "success");

        let elem = build_finish(
            SessionId("c2".into()),
            Some(JingleReason::ConnectivityError),
        );
        let reason = elem.children().find(|c| c.name() == "reason").unwrap();
        assert_eq!(
            reason.children().next().unwrap().name(),
            "connectivity-error"
        );

        let elem = build_finish(SessionId("c3".into()), Some(JingleReason::Expired));
        let reason = elem.children().find(|c| c.name() == "reason").unwrap();
        assert_eq!(reason.children().next().unwrap().name(), "expired");
    }
}
