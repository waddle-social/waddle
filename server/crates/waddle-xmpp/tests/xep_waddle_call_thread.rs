//! `urn:waddle:call-thread:0` — Waddle's call-thread anchor marker.
//!
//! The conversation thread remains the standard XEP-0201 `<thread/>`.
//! This custom marker only links that thread to a typed call session.

use chrono::{DateTime, Utc};
use minidom::Element;
use waddle_xmpp::xep::{
    build_call_thread_anchor, build_call_thread_ended, parse_call_thread_anchor,
    parse_call_thread_ended, parse_call_thread_ended_child, CallThreadAnchor, CallThreadDuration,
    CallThreadEnded, CallThreadKind, CallThreadMedia, CallThreadParseError, NS_FASTEN,
    NS_WADDLE_CALL_THREAD,
};
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::Message;

fn anchor(media: CallThreadMedia) -> CallThreadAnchor {
    CallThreadAnchor {
        kind: CallThreadKind::Muc,
        sid: SessionId("session-123".to_owned()),
        media,
        initiator: "alice@example.test".parse().expect("initiator"),
        started: "2026-06-07T14:30:00Z"
            .parse::<DateTime<Utc>>()
            .expect("started"),
    }
}

#[test]
fn call_thread_marker_namespace_is_versioned_zero() {
    assert_eq!(NS_WADDLE_CALL_THREAD, "urn:waddle:call-thread:0");
}

#[test]
fn call_thread_marker_round_trips_audio_video_media() {
    let original = anchor(CallThreadMedia::audio_video());
    let element = build_call_thread_anchor(&original);

    assert_eq!(element.name(), "call-thread");
    assert_eq!(element.ns(), NS_WADDLE_CALL_THREAD);
    assert_eq!(element.attr("kind"), Some("muc"));
    assert_eq!(element.attr("sid"), Some("session-123"));
    assert_eq!(element.attr("media"), Some("audio video"));
    assert_eq!(element.attr("initiator"), Some("alice@example.test"));
    assert_eq!(element.attr("started"), Some("2026-06-07T14:30:00Z"));

    let parsed = parse_call_thread_anchor(&element).expect("marker parses");
    assert_eq!(parsed, original);
}

#[test]
fn call_thread_marker_round_trips_video_only_media() {
    let original = anchor(CallThreadMedia {
        audio: false,
        video: true,
    });
    let element = build_call_thread_anchor(&original);

    assert_eq!(element.attr("media"), Some("video"));
    assert_eq!(
        parse_call_thread_anchor(&element).expect("marker parses"),
        original
    );
}

#[test]
fn call_thread_ended_round_trips_duration_summary() {
    let original = CallThreadEnded {
        ended: "2026-06-07T14:35:00Z"
            .parse::<DateTime<Utc>>()
            .expect("ended"),
        duration: CallThreadDuration::parse("PT5M").expect("duration"),
    };

    let element = build_call_thread_ended(&original);

    assert_eq!(element.name(), "call-thread-ended");
    assert_eq!(element.ns(), NS_WADDLE_CALL_THREAD);
    assert_eq!(element.attr("ended"), Some("2026-06-07T14:35:00Z"));
    assert_eq!(element.attr("duration"), Some("PT5M"));
    assert_eq!(
        parse_call_thread_ended(&element).expect("ended marker parses"),
        original
    );
}

#[test]
fn call_thread_ended_child_parses_xep0422_fastening_shape() {
    let expected = CallThreadEnded {
        ended: "2026-06-07T14:35:00Z"
            .parse::<DateTime<Utc>>()
            .expect("ended"),
        duration: CallThreadDuration::parse("PT5M").expect("duration"),
    };
    let apply_to = Element::builder("apply-to", NS_FASTEN)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            "anchor-origin-id",
        )
        .append(build_call_thread_ended(&expected))
        .build();
    let mut message = Message::new(None);
    message.payloads.push(apply_to);

    assert_eq!(parse_call_thread_ended_child(&message), Some(expected));
}

#[test]
#[should_panic(expected = "call-thread marker requires audio or video media")]
fn call_thread_marker_builder_rejects_empty_media() {
    let _ = build_call_thread_anchor(&anchor(CallThreadMedia {
        audio: false,
        video: false,
    }));
}

#[test]
fn call_thread_marker_parser_rejects_empty_media() {
    let element: Element = "<call-thread xmlns='urn:waddle:call-thread:0' \
          kind='muc' \
          sid='session-123' \
          media='' \
          initiator='alice@example.test' \
          started='2026-06-07T14:30:00Z'/>"
        .parse()
        .expect("fixture parses");

    let err = parse_call_thread_anchor(&element).expect_err("empty media is invalid");
    assert!(matches!(err, CallThreadParseError::InvalidMedia));
}
