//! XEP-0353: Jingle Message Initiation (JMI).
//!
//! JMI is the pre-call ringing layer: it lets the initiator broadcast
//! a `<propose/>` to all of the responder's resources so any one of
//! them can ring and accept before Jingle session-initiate commits to
//! a single resource. Waddle uses JMI as the canonical 1:1 call ring
//! signal.
//!
//! Thin wrapper over [`xmpp_parsers::jingle_message::JingleMI`]; this
//! module owns the feature-var constant and provides builder helpers
//! that pre-populate the audio + video `<description/>` payloads
//! Waddle calls always offer.

use xmpp_parsers::jingle::SessionId;
pub use xmpp_parsers::jingle_message::JingleMI;

use super::xep0167::{opus_audio_description, vp8_video_description, MediaKind};

pub const NS_JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";

/// The set of media kinds offered on a Jingle Message `<propose/>`.
/// XEP-0353 §5.1.1 specifies that propose carries one
/// `<description/>` element; Waddle attaches an empty
/// `urn:xmpp:jingle:apps:rtp:1` description per media kind to
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
}

/// Build a `<propose/>` JMI for an A/V call. XEP-0353 §5.1.1
/// allows the propose to carry one `<description/>`; to convey both
/// audio and video capability we use the video description as the
/// headline (the session-initiate that follows carries both audio
/// and video contents). For audio-only calls the audio description
/// is sent instead.
pub fn build_propose(sid: SessionId, offer: CallOffer) -> JingleMI {
    let headline = if offer.video {
        MediaKind::Video
    } else {
        MediaKind::Audio
    };
    let desc = match headline {
        MediaKind::Audio => opus_audio_description(),
        MediaKind::Video => vp8_video_description(),
    };
    JingleMI::Propose {
        sid,
        description: desc.into(),
    }
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
/// call ends. xmpp-parsers does not yet have a typed `Finish`
/// variant, so we build the element directly via [`minidom`]; the
/// receiver matches by element name + namespace.
pub fn build_finish(sid: SessionId) -> minidom::Element {
    minidom::Element::builder("finish", NS_JINGLE_MESSAGE)
        .attr("id", sid.0)
        .build()
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
    fn propose_audio_video_round_trips() {
        let sid = SessionId("c1".to_string());
        let propose = build_propose(sid.clone(), CallOffer::audio_video());
        let elem: Element = propose.into();
        assert_eq!(elem.name(), "propose");
        assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
        assert_eq!(elem.attr("id"), Some("c1"));

        let desc = elem
            .children()
            .find(|c| c.name() == "description")
            .expect("description child");
        assert_eq!(desc.ns(), super::super::xep0167::NS_JINGLE_RTP);
        assert_eq!(desc.attr("media"), Some("video"));

        let reparsed = JingleMI::try_from(elem).expect("reparses");
        match reparsed {
            JingleMI::Propose { sid: s, .. } => assert_eq!(s.0, "c1"),
            other => panic!("expected Propose, got {other:?}"),
        }
    }

    #[test]
    fn propose_audio_only_emits_audio_media() {
        let propose = build_propose(SessionId("c2".into()), CallOffer::audio_only());
        let elem: Element = propose.into();
        let desc = elem.children().find(|c| c.name() == "description").unwrap();
        assert_eq!(desc.attr("media"), Some("audio"));
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
    fn finish_emits_canonical_element() {
        let elem = build_finish(SessionId("c1".into()));
        assert_eq!(elem.name(), "finish");
        assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
        assert_eq!(elem.attr("id"), Some("c1"));
        assert!(elem.children().next().is_none());
    }
}
