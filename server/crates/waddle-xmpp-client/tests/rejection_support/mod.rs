use minidom::Element;
use waddle_xmpp_client::{
    AccessToken, ClientConfig, ClientResource, ConnectionConfig, OAuthBearerConfig, SmResumeState,
    WebSocketConfig, XmppRuntime,
};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

pub fn runtime(resume: Option<SmResumeState>) -> XmppRuntime {
    let mut config = ClientConfig::new(
        ConnectionConfig::new("example.com".parse().unwrap()),
        WebSocketConfig::new("wss://example.com/ws".parse().unwrap()).unwrap(),
        OAuthBearerConfig::new(
            "alice@example.com".parse().unwrap(),
            ClientResource::new("web").unwrap(),
            AccessToken::new("test"),
        )
        .unwrap(),
    )
    .unwrap();
    config.session.stream_management.resume_state = resume;
    XmppRuntime::new(config).unwrap()
}

pub fn rejection(from: &str) -> Element {
    Element::builder("message", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "rejected")
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "alice@example.com/web",
        )
        .append(
            Element::builder("body", "jabber:client")
                .append("echoed body")
                .build(),
        )
        .append(Element::builder("composing", "http://jabber.org/protocol/chatstates").build())
        .append(
            Element::builder("reactions", "urn:xmpp:reactions:0")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "reacted")
                .append(
                    Element::builder("reaction", "urn:xmpp:reactions:0")
                        .append("👍")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("propose", "urn:xmpp:jingle-message:0")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call")
                .build(),
        )
        .append(Element::from(StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "",
            "No such account",
        )))
        .build()
}
