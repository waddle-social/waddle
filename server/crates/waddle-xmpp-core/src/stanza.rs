//! Shared typed stanza helpers.

/// Parsed stanza types.
///
/// `Iq` is boxed because xmpp-parsers 0.22 restructured `Iq` from a
/// struct into a four-variant enum with embedded payload data, ballooning
/// its size to ~424 bytes vs. ~216 for the next-largest variant. Boxing
/// keeps the enum compact (it's pattern-matched and cloned through a lot
/// of dispatcher code) and silences the clippy
/// `large_enum_variant` lint without a lint-suppression escape hatch.
#[derive(Debug, Clone)]
pub enum Stanza {
    Message(xmpp_parsers::message::Message),
    Presence(xmpp_parsers::presence::Presence),
    Iq(Box<xmpp_parsers::iq::Iq>),
}

impl Stanza {
    /// Get the stanza type name for tracing.
    pub fn name(&self) -> &'static str {
        match self {
            Stanza::Message(_) => "message",
            Stanza::Presence(_) => "presence",
            Stanza::Iq(_) => "iq",
        }
    }

    /// Convert the stanza to a minidom Element.
    pub fn to_element(&self) -> minidom::Element {
        match self {
            Stanza::Message(message) => {
                let mut element = message.clone().into();
                crate::parser_utils::ensure_thread_element(
                    &mut element,
                    message.thread.as_ref().map(|thread| thread.id.as_str()),
                );
                element
            }
            Stanza::Presence(presence) => presence.clone().into(),
            Stanza::Iq(iq) => (**iq).clone().into(),
        }
    }
}
