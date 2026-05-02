//! Shared typed stanza helpers.

/// Parsed stanza types.
#[derive(Debug, Clone)]
pub enum Stanza {
    Message(xmpp_parsers::message::Message),
    Presence(xmpp_parsers::presence::Presence),
    Iq(xmpp_parsers::iq::Iq),
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
                    message.thread.as_ref().map(|thread| thread.0.as_str()),
                );
                element
            }
            Stanza::Presence(presence) => presence.clone().into(),
            Stanza::Iq(iq) => iq.clone().into(),
        }
    }
}
