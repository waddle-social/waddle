//! XEP-0245: The /me Command dedicated client suite.
//!
//! XEP-0245 "Recommended Handling": the command "does not result in the
//! generation of any XMPP protocol" — it is sent as-is in `<body/>` and the
//! receiver string-matches the first four characters against "/me ". The
//! "Integration With XHTML-IM" section adds that formatted variants MUST NOT
//! modify the command string. The client's obligations are therefore that
//! the prefix survives the outbound builders verbatim (plain, corrected,
//! with XEP-0394 markup) and that inbound parsing hands the body over
//! unmodified, with markup offsets still indexing the full body.

use minidom::Element;
use waddle_xmpp_client::{
    messaging::{
        build_correction_message, build_outbound_message, parse, MarkupSpanData, MarkupSpanType,
        MessagingEvent, SendMessageOptions, NS_CLIENT, NS_MESSAGE_CORRECT,
    },
    StanzaId,
};

/// XEP-0394 Message Markup namespace.
const NS_MARKUP: &str = "urn:xmpp:markup:0";

const ROOM: &str = "lobby@muc.example.com";
const ACTION_BODY: &str = "/me waves hello";

fn body_text(message: &Element) -> String {
    message
        .get_child("body", NS_CLIENT)
        .expect("message carries a body")
        .text()
}

fn bold_span(start: u32, end: u32) -> MarkupSpanData {
    MarkupSpanData {
        span_type: "bold".to_owned(),
        start,
        end,
        uri: None,
    }
}

fn inbound_body_and_spans(
    message: &Element,
) -> (Option<String>, Vec<(MarkupSpanType, usize, usize)>) {
    match parse(message) {
        Some(MessagingEvent::Message(parsed)) => (
            parsed.body.clone(),
            parsed
                .markup_spans
                .iter()
                .map(|span| (span.span_type.clone(), span.start, span.end))
                .collect(),
        ),
        other => panic!("expected an inbound message, got {other:?}"),
    }
}

#[test]
fn xep0245_outbound_groupchat_keeps_the_me_prefix_verbatim() {
    let (_, message) = build_outbound_message(
        ROOM,
        "groupchat",
        ACTION_BODY,
        &SendMessageOptions::default(),
    )
    .expect("action message builds");

    assert_eq!(
        body_text(&message),
        ACTION_BODY,
        "the command is sent as-is"
    );
}

#[test]
fn xep0245_outbound_chat_keeps_the_me_prefix_verbatim() {
    let (_, message) = build_outbound_message(
        "juliet@example.com",
        "chat",
        ACTION_BODY,
        &SendMessageOptions::default(),
    )
    .expect("action message builds");

    assert_eq!(body_text(&message), ACTION_BODY);
}

#[test]
fn xep0245_adds_no_protocol_element_of_its_own() {
    let (_, plain) = build_outbound_message(
        ROOM,
        "groupchat",
        "waves hello",
        &SendMessageOptions::default(),
    )
    .expect("plain message builds");
    let (_, action) = build_outbound_message(
        ROOM,
        "groupchat",
        ACTION_BODY,
        &SendMessageOptions::default(),
    )
    .expect("action message builds");

    let child_names = |message: &Element| -> Vec<(String, String)> {
        message
            .children()
            .map(|child| (child.name().to_owned(), child.ns()))
            .collect()
    };
    assert_eq!(
        child_names(&action),
        child_names(&plain),
        "an action differs from a plain message only in its body text"
    );
}

#[test]
fn xep0245_markup_offsets_index_the_full_body_including_the_prefix() {
    // Bold over "waves" (scalar offsets 4..9 of "/me waves hello").
    let options = SendMessageOptions {
        markup_spans: vec![bold_span(4, 9)],
        ..SendMessageOptions::default()
    };
    let (_, message) = build_outbound_message(ROOM, "groupchat", ACTION_BODY, &options)
        .expect("styled action message builds");

    assert_eq!(body_text(&message), ACTION_BODY);
    let span = message
        .get_child("markup", NS_MARKUP)
        .and_then(|markup| markup.get_child("span", NS_MARKUP))
        .expect("markup span is present");
    assert_eq!(span.attr("start"), Some("4"));
    assert_eq!(span.attr("end"), Some("9"));
}

#[test]
fn xep0245_correction_of_an_action_keeps_the_prefix() {
    let (_, message) = build_correction_message(
        ROOM,
        "groupchat",
        "/me waves goodbye",
        "original-id",
        &SendMessageOptions::default(),
    )
    .expect("corrected action builds");

    assert_eq!(body_text(&message), "/me waves goodbye");
    assert_eq!(
        message
            .get_child("replace", NS_MESSAGE_CORRECT)
            .and_then(|replace| replace.attr("id")),
        Some("original-id")
    );
}

#[test]
fn xep0245_inbound_action_body_and_markup_survive_parsing_unmodified() {
    let options = SendMessageOptions {
        stanza_id: Some(StanzaId::new("action-1").expect("non-empty stanza id")),
        markup_spans: vec![bold_span(4, 9)],
        ..SendMessageOptions::default()
    };
    let (_, message) = build_outbound_message(ROOM, "groupchat", ACTION_BODY, &options)
        .expect("action message builds");

    let (body, spans) = inbound_body_and_spans(&message);

    assert_eq!(body.as_deref(), Some(ACTION_BODY));
    assert_eq!(spans, vec![(MarkupSpanType::Bold, 4, 9)]);
}

#[test]
fn xep0245_spec_non_commands_pass_through_untouched() {
    // The "Some Non-Commands" example bodies must not be special-cased by
    // the transport layer either: they are carried and parsed verbatim.
    for body in [
        "/meshrugs in disgust",
        "/me's disgusted",
        " /me shrugs in disgust",
        "\"/me shrugs in disgust\"",
        "* Atlas shrugs in disgust",
        "Why did Atlas say \"/me shrugs in disgust\"?",
    ] {
        let (_, message) =
            build_outbound_message(ROOM, "groupchat", body, &SendMessageOptions::default())
                .expect("message builds");
        assert_eq!(body_text(&message), body);
        let (parsed, _) = inbound_body_and_spans(&message);
        assert_eq!(parsed.as_deref(), Some(body));
    }
}
