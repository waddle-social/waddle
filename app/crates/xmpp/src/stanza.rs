use std::str::FromStr;

use xmpp_parsers::{iq::Iq, message::Message, minidom::Element, presence::Presence};

use crate::error::PipelineError;

#[derive(Debug, Clone, PartialEq)]
pub enum Stanza {
    Message(Box<Message>),
    Presence(Box<Presence>),
    Iq(Box<Iq>),
}

impl Stanza {
    pub fn parse(raw: &[u8]) -> Result<Self, PipelineError> {
        parse_stanza(raw)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, PipelineError> {
        serialize_stanza(self)
    }

    pub fn to_element(&self) -> Element {
        match self {
            Stanza::Message(message) => (**message).clone().into(),
            Stanza::Presence(presence) => (**presence).clone().into(),
            Stanza::Iq(iq) => (**iq).clone().into(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Stanza::Message(_) => "message",
            Stanza::Presence(_) => "presence",
            Stanza::Iq(_) => "iq",
        }
    }
}

impl TryFrom<Element> for Stanza {
    type Error = PipelineError;

    fn try_from(element: Element) -> Result<Self, Self::Error> {
        parse_stanza_element(element)
    }
}

impl From<Stanza> for Element {
    fn from(value: Stanza) -> Self {
        match value {
            Stanza::Message(message) => (*message).into(),
            Stanza::Presence(presence) => (*presence).into(),
            Stanza::Iq(iq) => (*iq).into(),
        }
    }
}

impl From<&Stanza> for Element {
    fn from(value: &Stanza) -> Self {
        value.to_element()
    }
}

pub fn parse_stanza(raw: &[u8]) -> Result<Stanza, PipelineError> {
    let xml = std::str::from_utf8(raw).map_err(|error| {
        PipelineError::ParseFailed(format!("invalid UTF-8 stanza bytes: {error}"))
    })?;
    let trimmed = xml.trim();
    if trimmed.is_empty() {
        return Err(PipelineError::ParseFailed(
            "stanza payload is empty".to_string(),
        ));
    }

    let element = Element::from_str(trimmed).map_err(|error| {
        PipelineError::ParseFailed(format!("failed to parse stanza XML: {error}"))
    })?;
    parse_stanza_element(element)
}

pub fn serialize_stanza(stanza: &Stanza) -> Result<Vec<u8>, PipelineError> {
    let element = stanza.to_element();
    let mut payload = Vec::new();
    element.write_to(&mut payload).map_err(|error| {
        PipelineError::ProcessorFailed(format!(
            "failed to serialize <{}/> stanza: {error}",
            stanza.name()
        ))
    })?;
    Ok(payload)
}

fn parse_stanza_element(element: Element) -> Result<Stanza, PipelineError> {
    match element.name() {
        "message" => Message::try_from(element)
            .map(|message| Stanza::Message(Box::new(message)))
            .map_err(|error| {
                PipelineError::ParseFailed(format!("failed to parse <message/> stanza: {error}"))
            }),
        "presence" => Presence::try_from(element)
            .map(|presence| Stanza::Presence(Box::new(presence)))
            .map_err(|error| {
                PipelineError::ParseFailed(format!("failed to parse <presence/> stanza: {error}"))
            }),
        "iq" => Iq::try_from(element)
            .map(|iq| Stanza::Iq(Box::new(iq)))
            .map_err(|error| {
                PipelineError::ParseFailed(format!("failed to parse <iq/> stanza: {error}"))
            }),
        other => Err(PipelineError::ParseFailed(format!(
            "unsupported stanza element <{other}/>"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use xmpp_parsers::{message::MessageType, presence::Show};

    use super::*;

    const MESSAGE_XML: &str = "<message xmlns='jabber:client' type='chat' from='alice@example.com' to='bob@example.com'><body>hello</body></message>";
    const PRESENCE_XML: &str =
        "<presence xmlns='jabber:client'><show>away</show><status>out</status></presence>";
    const IQ_XML: &str =
        "<iq xmlns='jabber:client' type='get' id='ping-1'><ping xmlns='urn:xmpp:ping'/></iq>";

    #[test]
    fn parses_message_stanza() {
        let stanza = parse_stanza(MESSAGE_XML.as_bytes()).expect("message stanza should parse");
        let Stanza::Message(message) = stanza else {
            panic!("expected message stanza");
        };

        assert_eq!(message.type_, MessageType::Chat);
        assert_eq!(message.bodies.get("").map(String::as_str), Some("hello"));
    }

    #[test]
    fn parses_presence_stanza() {
        let stanza = parse_stanza(PRESENCE_XML.as_bytes()).expect("presence stanza should parse");
        let Stanza::Presence(presence) = stanza else {
            panic!("expected presence stanza");
        };

        assert_eq!(presence.show, Some(Show::Away));
        assert_eq!(presence.statuses.get("").map(String::as_str), Some("out"));
    }

    #[test]
    fn parses_iq_stanza() {
        let stanza = parse_stanza(IQ_XML.as_bytes()).expect("iq stanza should parse");
        let Stanza::Iq(iq) = stanza else {
            panic!("expected iq stanza");
        };

        assert_eq!(iq.id(), "ping-1");
    }

    #[test]
    fn parse_rejects_unknown_root_element() {
        let error = parse_stanza(b"<foo xmlns='jabber:client'/>").expect_err("must fail");
        assert!(matches!(error, PipelineError::ParseFailed(_)));
        assert!(
            error
                .to_string()
                .contains("unsupported stanza element <foo/>")
        );
    }

    #[test]
    fn parse_rejects_invalid_utf8() {
        let error = parse_stanza(&[0xFF, 0xFE]).expect_err("must fail");
        assert!(matches!(error, PipelineError::ParseFailed(_)));
        assert!(error.to_string().contains("invalid UTF-8 stanza bytes"));
    }

    #[test]
    fn serializes_and_round_trips_core_stanza_types() {
        for raw in [MESSAGE_XML, PRESENCE_XML, IQ_XML] {
            let stanza = parse_stanza(raw.as_bytes()).expect("stanza should parse");
            let encoded = serialize_stanza(&stanza).expect("stanza should serialize");
            let decoded = parse_stanza(&encoded).expect("serialized stanza should parse");
            assert_eq!(decoded, stanza);
        }
    }
}
