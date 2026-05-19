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
//! carry no children. `<propose/>` and `<finish/>` instead build the
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

use xmpp_parsers::jingle::{Reason as JingleReason, SessionId};
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
    let mut builder = minidom::Element::builder("propose", NS_JINGLE_MESSAGE).attr("id", sid.0);
    for kind in offer.media_kinds() {
        builder = builder.append(
            minidom::Element::builder("description", NS_JINGLE_RTP)
                .attr("media", kind.as_str())
                .build(),
        );
    }
    builder.build()
}

pub fn build_proceed(sid: SessionId) -> JingleMI {
    JingleMI::Proceed(sid)
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
    let mut builder = minidom::Element::builder("finish", NS_JINGLE_MESSAGE).attr("id", sid.0);
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
