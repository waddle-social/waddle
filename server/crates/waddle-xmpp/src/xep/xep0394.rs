//! XEP-0394: Message Markup
//!
//! Builds markup metadata payloads that keep the message body as the single
//! textual source of truth while carrying semantic formatting separately.

use minidom::Element;

/// Namespace for XEP-0394 Message Markup.
pub const NS_MARKUP: &str = "urn:xmpp:markup:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkupKind {
    Blockquote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkupSpan {
    pub kind: MarkupKind,
    pub start: u32,
    pub end: u32,
}

pub fn build_message_markup_element(spans: &[MarkupSpan]) -> Option<Element> {
    if spans.is_empty() {
        return None;
    }
    let mut markup = Element::builder("markup", NS_MARKUP).build();
    for span in spans {
        let child = match span.kind {
            MarkupKind::Blockquote => Element::builder("bquote", NS_MARKUP)
                .attr(
                    minidom::rxml::xml_ncname!("start").to_owned(),
                    span.start.to_string(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("end").to_owned(),
                    span.end.to_string(),
                )
                .build(),
        };
        markup.append_child(child);
    }
    Some(markup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_blockquote_markup_element() {
        let element = build_message_markup_element(&[MarkupSpan {
            kind: MarkupKind::Blockquote,
            start: 0,
            end: 8,
        }])
        .expect("markup element");

        assert!(element.is("markup", NS_MARKUP));
        let quote = element
            .get_child("bquote", NS_MARKUP)
            .expect("blockquote child");
        assert_eq!(quote.attr("start"), Some("0"));
        assert_eq!(quote.attr("end"), Some("8"));
    }

    #[test]
    fn omits_empty_markup() {
        assert!(build_message_markup_element(&[]).is_none());
    }
}
