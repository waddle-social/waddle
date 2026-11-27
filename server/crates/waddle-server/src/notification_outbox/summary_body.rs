use waddle_xmpp::xep::xep0424::NS_MESSAGE_RETRACT;
use waddle_xmpp::xep::xep0428::{
    parse_fallbacks_from_message, strip_fallback_ranges, FallbackRegion,
};
use waddle_xmpp::xep::xep0461::NS_REPLY;
use xmpp_parsers::message::Message;

// Other fallback bodies can be the only usable preview, such as a file-share URL.
const PUSH_PREVIEW_FALLBACK_NAMESPACES: &[&str] = &[NS_REPLY, NS_MESSAGE_RETRACT];

pub(crate) fn selected_message_body(message: &Message) -> Option<&str> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .map(String::as_str)
}

pub(super) fn summary_body(message: &Message) -> Option<String> {
    let body = selected_message_body(message)?;
    let mut ranges = Vec::new();
    for fallback in parse_fallbacks_from_message(message) {
        if !fallback
            .for_ns
            .as_deref()
            .is_some_and(|ns| PUSH_PREVIEW_FALLBACK_NAMESPACES.contains(&ns))
        {
            continue;
        }
        match fallback.body {
            Some(FallbackRegion::Whole) => return None,
            Some(FallbackRegion::Ranges(body_ranges)) => ranges.extend(body_ranges),
            None => {}
        }
    }

    let stripped = (!ranges.is_empty()).then(|| strip_fallback_ranges(body, &ranges));
    let body = stripped.as_deref().unwrap_or(body).trim();
    (!body.is_empty()).then(|| body.to_owned())
}
