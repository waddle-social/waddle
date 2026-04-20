//! XEP-0461 Message Replies + XEP-0428 Fallback Indication.

use jid::Jid;
use minidom::Element;

/// XEP-0461 reply element namespace.
pub const NS_REPLY: &str = "urn:xmpp:reply:0";

/// XEP-0428 fallback indication namespace.
pub const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

/// A typed reply marker: which message is being replied to, and its author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyMarker {
    /// The `to` attribute on `<reply/>` — the JID of the author of the message
    /// being replied to. For MUC this is the occupant full JID; for 1:1 the
    /// bare JID.
    pub to: Jid,
    /// The `id` attribute on `<reply/>` — id of the message being replied to.
    pub id: String,
}

/// A character-offset range identifying the fallback text inside the body.
/// Per XEP-0428, offsets count Unicode scalar values and `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackRange {
    pub start: u32,
    pub end: u32,
}

/// Build a `<reply xmlns='urn:xmpp:reply:0' to='…' id='…'/>` element from a
/// typed marker.
pub fn build_reply_element(marker: &ReplyMarker) -> Element {
    Element::builder("reply", NS_REPLY)
        .attr("to", marker.to.to_string())
        .attr("id", marker.id.as_str())
        .build()
}

/// Build a `<fallback>` element scoped to XEP-0461 carrying a single
/// `<body start='…' end='…'/>` range.
pub fn build_fallback_element(range: &FallbackRange) -> Element {
    Element::builder("fallback", NS_FALLBACK)
        .attr("for", NS_REPLY)
        .append(
            Element::builder("body", NS_FALLBACK)
                .attr("start", range.start.to_string())
                .attr("end", range.end.to_string())
                .build(),
        )
        .build()
}

/// Parse `<reply xmlns='urn:xmpp:reply:0' to='…' id='…'/>` out of a message
/// element. Returns `None` if no reply child is present or if required
/// attributes are missing or malformed.
pub fn parse_reply(message: &Element) -> Option<ReplyMarker> {
    let reply_el = message.get_child("reply", NS_REPLY)?;
    let id = reply_el.attr("id")?.to_string();
    let to = reply_el.attr("to")?.parse::<Jid>().ok()?;
    Some(ReplyMarker { to, id })
}

/// Parse the reply fallback range, if present:
///
/// ```xml
/// <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>
///   <body start='0' end='42'/>
/// </fallback>
/// ```
///
/// Only fallback elements scoped to the XEP-0461 reply namespace are
/// recognised.
pub fn parse_fallback(message: &Element) -> Option<FallbackRange> {
    for fb in message.children().filter(|c| {
        c.name() == "fallback" && c.ns() == NS_FALLBACK && c.attr("for") == Some(NS_REPLY)
    }) {
        let body = fb.get_child("body", NS_FALLBACK)?;
        let start: u32 = body.attr("start")?.parse().ok()?;
        let end: u32 = body.attr("end")?.parse().ok()?;
        if end < start {
            return None;
        }
        return Some(FallbackRange { start, end });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_el(xml: &str) -> Element {
        xml.parse().expect("invalid XML")
    }

    #[test]
    fn parses_reply_marker() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <reply xmlns='urn:xmpp:reply:0' to='alice@example.com' id='m42'/>\
             </message>",
        );
        let marker = parse_reply(&el).expect("expected reply");
        assert_eq!(marker.id, "m42");
        assert_eq!(marker.to.to_string(), "alice@example.com");
    }

    #[test]
    fn returns_none_when_no_reply_child() {
        let el = parse_el("<message xmlns='jabber:client'><body>hi</body></message>");
        assert!(parse_reply(&el).is_none());
    }

    #[test]
    fn returns_none_when_id_missing() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <reply xmlns='urn:xmpp:reply:0' to='alice@example.com'/>\
             </message>",
        );
        assert!(parse_reply(&el).is_none());
    }

    #[test]
    fn parses_fallback_range() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                 <body start='0' end='42'/>\
               </fallback>\
             </message>",
        );
        let range = parse_fallback(&el).expect("expected range");
        assert_eq!(range.start, 0);
        assert_eq!(range.end, 42);
    }

    #[test]
    fn ignores_fallback_for_other_xeps() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <fallback xmlns='urn:xmpp:fallback:0' for='some:other:xep'>\
                 <body start='0' end='10'/>\
               </fallback>\
             </message>",
        );
        assert!(parse_fallback(&el).is_none());
    }

    #[test]
    fn reply_without_fallback_parses_marker_only() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <body>just a reply, no fallback</body>\
               <reply xmlns='urn:xmpp:reply:0' to='alice@example.com' id='m99'/>\
             </message>",
        );
        assert!(parse_reply(&el).is_some());
        assert!(parse_fallback(&el).is_none());
    }

    #[test]
    fn rejects_end_before_start() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                 <body start='50' end='10'/>\
               </fallback>\
             </message>",
        );
        assert!(parse_fallback(&el).is_none());
    }

    #[test]
    fn builds_reply_element() {
        let marker = ReplyMarker {
            to: "alice@example.com".parse::<Jid>().expect("valid jid"),
            id: "target-msg-id".to_string(),
        };
        let el = build_reply_element(&marker);
        assert_eq!(el.name(), "reply");
        assert_eq!(el.ns(), NS_REPLY);
        assert_eq!(el.attr("to"), Some("alice@example.com"));
        assert_eq!(el.attr("id"), Some("target-msg-id"));
    }

    #[test]
    fn builds_fallback_element() {
        let range = FallbackRange { start: 0, end: 42 };
        let el = build_fallback_element(&range);
        assert_eq!(el.name(), "fallback");
        assert_eq!(el.ns(), NS_FALLBACK);
        assert_eq!(el.attr("for"), Some(NS_REPLY));
        let body = el
            .get_child("body", NS_FALLBACK)
            .expect("body child present");
        assert_eq!(body.attr("start"), Some("0"));
        assert_eq!(body.attr("end"), Some("42"));
    }

    #[test]
    fn build_and_parse_reply_roundtrip() {
        let marker = ReplyMarker {
            to: "bob@example.com/phone".parse::<Jid>().expect("valid jid"),
            id: "m-123".to_string(),
        };
        let range = FallbackRange { start: 5, end: 100 };

        let message = Element::builder("message", "jabber:client")
            .attr("type", "groupchat")
            .append(build_reply_element(&marker))
            .append(build_fallback_element(&range))
            .build();

        let parsed_marker = parse_reply(&message).expect("marker parses");
        assert_eq!(parsed_marker.id, "m-123");
        assert_eq!(parsed_marker.to.to_string(), "bob@example.com/phone");
        let parsed_range = parse_fallback(&message).expect("range parses");
        assert_eq!(parsed_range, range);
    }
}
