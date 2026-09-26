use waddle_extensions::DisplayText;
use waddle_xmpp::xep::xep0428::{
    is_fallback_element, strip_fallback_ranges, FallbackRange, NS_FALLBACK,
};
use waddle_xmpp::xep::xep0461::{is_reply_element, parse_reply_from_message, NS_REPLY};
use xmpp_parsers::message::Message;

/// Derive the observer input without changing the accepted source body.
/// Only the one explicit reply range understood by current clients is removed.
/// Invalid or unsupported metadata retains the complete body for observation.
pub(super) fn observation_body(message: &Message, wire_body: &str) -> Option<DisplayText> {
    let body = match supported_reply_range(message, wire_body.chars().count()) {
        Some(range) => strip_fallback_ranges(wire_body, &[range]),
        None => wire_body.to_owned(),
    };
    DisplayText::new(body).ok()
}

fn supported_reply_range(message: &Message, body_len: usize) -> Option<FallbackRange> {
    let mut replies = message
        .payloads
        .iter()
        .filter(|elem| is_reply_element(elem));
    let reply = replies.next()?;
    if replies.next().is_some() || parse_reply_from_message(message).is_none() {
        return None;
    }
    // Native clients only expose the reply and its fallback when the shared
    // reply parser accepts `to`, although XEP-0461 makes that attribute optional.
    reply.attr("to")?.parse::<jid::Jid>().ok()?;
    let mut fallbacks = message.payloads.iter().filter(|elem| {
        is_fallback_element(elem) && elem.attr("for").map(str::trim) == Some(NS_REPLY)
    });
    let fallback = fallbacks.next()?;
    if fallbacks.next().is_some() || fallback.attr("for") != Some(NS_REPLY) {
        return None;
    }
    let mut children = fallback.children();
    let body = children.next()?;
    if !body.is("body", NS_FALLBACK) || children.next().is_some() {
        return None;
    }
    // Match the shared client's explicit-range parser exactly: u32 attributes
    // without whitespace normalization. A broader parse could hide text here
    // that supported clients continue to display.
    let start = body.attr("start")?.parse::<u32>().ok()? as usize;
    let end = body.attr("end")?.parse::<u32>().ok()? as usize;
    (start <= end && end <= body_len).then_some(FallbackRange { start, end })
}

#[cfg(test)]
mod tests;
