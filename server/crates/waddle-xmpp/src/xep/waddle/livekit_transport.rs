//! `urn:waddle:transports:livekit:0` — Waddle's Jingle transport for
//! LiveKit-backed calls.
//!
//! Two forms exist on the wire:
//!
//! * [`WaddleLiveKitTransport::Request`] — empty element. Sent by the
//!   initiator (and by the responder in `session-accept`) to ask the
//!   server to fill in LiveKit join credentials before the stanza is
//!   forwarded to the remote peer.
//! * [`WaddleLiveKitTransport::Issued`] — fully-populated element
//!   carrying the LiveKit websocket URL, room name, participant
//!   identity, and signed JWT. Inserted by the server's Jingle
//!   handler when it routes a session-initiate/accept onward.
//!
//! The XEP-conformance hard rule (CLAUDE.md) requires this custom
//! namespace because no XEP-defined transport models an SFU join
//! token. XEP-0167 + XEP-0176 assume peer-to-peer ICE negotiation
//! which doesn't match LiveKit's WebSocket signaling.
//!
//! Wire shape:
//!
//! ```xml
//! <!-- Empty (request) form: -->
//! <transport xmlns='urn:waddle:transports:livekit:0'/>
//!
//! <!-- Filled (issued) form: -->
//! <transport xmlns='urn:waddle:transports:livekit:0'
//!            url='wss://livekit.waddle.social'
//!            room='c1'
//!            identity='alice@waddle.social/desktop'>
//!   <token>eyJhbGc...</token>
//! </transport>
//! ```

use minidom::Element;
use thiserror::Error;
use url::Url;

use waddle_sfu::{CallId, Identity, JoinToken, Jwt, SfuError, WebsocketUrl};

pub const NS_WADDLE_LIVEKIT_TRANSPORT: &str = "urn:waddle:transports:livekit:0";
pub const TRANSPORT_NAME: &str = "transport";
const TOKEN_NAME: &str = "token";
const ATTR_URL: &str = "url";
const ATTR_ROOM: &str = "room";
const ATTR_IDENTITY: &str = "identity";

/// Typed Waddle LiveKit transport element.
#[derive(Debug, Clone)]
pub enum WaddleLiveKitTransport {
    /// Empty element sent by clients to request a server-issued token.
    Request,
    /// Fully-populated element carrying server-issued LiveKit join
    /// credentials.
    Issued(IssuedTransport),
}

/// Server-issued transport contents. Constructed from a
/// [`JoinToken`] returned by the SFU service.
#[derive(Debug, Clone)]
pub struct IssuedTransport {
    pub url: WebsocketUrl,
    pub room: CallId,
    pub identity: Identity,
    pub token: Jwt,
}

impl WaddleLiveKitTransport {
    /// Build the empty request form. Used by client-side test
    /// helpers and by the wasm client when assembling outbound
    /// session stanzas.
    pub fn request() -> Self {
        Self::Request
    }

    /// Build the issued form from a [`JoinToken`].
    pub fn from_join_token(token: JoinToken) -> Self {
        Self::Issued(IssuedTransport {
            url: token.url,
            room: token.room,
            identity: token.identity,
            token: token.jwt,
        })
    }

    /// Serialize to `<transport xmlns='urn:waddle:transports:livekit:0'.../>`.
    pub fn to_element(&self) -> Element {
        match self {
            Self::Request => Element::builder(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT).build(),
            Self::Issued(t) => Element::builder(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT)
                .attr(minidom::rxml::xml_ncname!("url").to_owned(), t.url.as_str())
                .attr(
                    minidom::rxml::xml_ncname!("room").to_owned(),
                    t.room.as_str(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("identity").to_owned(),
                    t.identity.as_livekit_identity(),
                )
                .append(
                    Element::builder(TOKEN_NAME, NS_WADDLE_LIVEKIT_TRANSPORT)
                        .append(t.token.as_str())
                        .build(),
                )
                .build(),
        }
    }
}

#[derive(Debug, Error)]
pub enum TransportParseError {
    #[error("element is not a <transport/> in urn:waddle:transports:livekit:0")]
    WrongElement,
    #[error("partially-filled transport: attribute {0} is missing")]
    MissingAttribute(&'static str),
    #[error("partially-filled transport: <token/> child is missing")]
    MissingToken,
    #[error("invalid LiveKit websocket URL: {0}")]
    InvalidUrl(String),
    #[error("invalid identity JID: {0}")]
    InvalidIdentity(String),
    #[error(transparent)]
    InvalidCallId(SfuError),
}

impl TryFrom<&Element> for WaddleLiveKitTransport {
    type Error = TransportParseError;

    fn try_from(elem: &Element) -> Result<Self, Self::Error> {
        if elem.name() != TRANSPORT_NAME || elem.ns() != NS_WADDLE_LIVEKIT_TRANSPORT {
            return Err(TransportParseError::WrongElement);
        }

        let url = elem.attr(ATTR_URL);
        let room = elem.attr(ATTR_ROOM);
        let identity = elem.attr(ATTR_IDENTITY);
        let token_child = elem
            .children()
            .find(|c| c.name() == TOKEN_NAME && c.ns() == NS_WADDLE_LIVEKIT_TRANSPORT);

        match (url, room, identity, token_child) {
            (None, None, None, None) => Ok(Self::Request),
            (Some(url), Some(room), Some(identity), Some(token)) => {
                let parsed_url = Url::parse(url)
                    .map_err(|_| TransportParseError::InvalidUrl(url.to_string()))?;
                let ws_url = WebsocketUrl::new(parsed_url)
                    .map_err(|e| TransportParseError::InvalidUrl(e.to_string()))?;
                let call_id = CallId::new(room).map_err(TransportParseError::InvalidCallId)?;
                let jid: jid::FullJid = identity
                    .parse()
                    .map_err(|_| TransportParseError::InvalidIdentity(identity.to_string()))?;
                let identity = Identity::from_jid(jid);
                let jwt_text = token.text();
                Ok(Self::Issued(IssuedTransport {
                    url: ws_url,
                    room: call_id,
                    identity,
                    token: Jwt::from_wire(jwt_text),
                }))
            }
            (None, _, _, _) => Err(TransportParseError::MissingAttribute(ATTR_URL)),
            (_, None, _, _) => Err(TransportParseError::MissingAttribute(ATTR_ROOM)),
            (_, _, None, _) => Err(TransportParseError::MissingAttribute(ATTR_IDENTITY)),
            (_, _, _, None) => Err(TransportParseError::MissingToken),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_form_round_trips() {
        let xml = "<transport xmlns='urn:waddle:transports:livekit:0'/>";
        let elem: Element = xml.parse().expect("element parses");
        let parsed = WaddleLiveKitTransport::try_from(&elem).expect("parses");
        assert!(matches!(parsed, WaddleLiveKitTransport::Request));

        let rebuilt = parsed.to_element();
        assert_eq!(rebuilt.name(), TRANSPORT_NAME);
        assert_eq!(rebuilt.ns(), NS_WADDLE_LIVEKIT_TRANSPORT);
        assert!(rebuilt.attrs().iter().next().is_none());
        assert!(rebuilt.children().next().is_none());
    }

    #[test]
    fn issued_form_round_trips() {
        let url = WebsocketUrl::new("wss://livekit.waddle.social".parse().unwrap()).unwrap();
        let room = CallId::new("c1").unwrap();
        let jid: jid::FullJid = "alice@waddle.social/desktop".parse().unwrap();
        let identity = Identity::from_jid(jid);
        let token = Jwt::from_wire("eyJhbGciOi...".to_string());

        let issued = WaddleLiveKitTransport::Issued(IssuedTransport {
            url: url.clone(),
            room: room.clone(),
            identity: identity.clone(),
            token,
        });

        let elem = issued.to_element();
        assert_eq!(elem.attr(ATTR_URL), Some(url.as_str()));
        assert_eq!(elem.attr(ATTR_ROOM), Some(room.as_str()));

        let reparsed = WaddleLiveKitTransport::try_from(&elem).expect("parses");
        match reparsed {
            WaddleLiveKitTransport::Issued(t) => {
                assert_eq!(t.url.as_str(), url.as_str());
                assert_eq!(t.room, room);
                assert_eq!(t.identity, identity);
                assert_eq!(t.token.as_str(), "eyJhbGciOi...");
            }
            WaddleLiveKitTransport::Request => panic!("expected Issued form"),
        }
    }

    #[test]
    fn rejects_partial_attributes() {
        let xml = "<transport xmlns='urn:waddle:transports:livekit:0' url='wss://x/'/>";
        let elem: Element = xml.parse().unwrap();
        let err = WaddleLiveKitTransport::try_from(&elem).unwrap_err();
        assert!(matches!(err, TransportParseError::MissingAttribute(_)));
    }

    #[test]
    fn rejects_wrong_namespace() {
        let xml = "<transport xmlns='urn:xmpp:jingle:transports:ice-udp:1'/>";
        let elem: Element = xml.parse().unwrap();
        let err = WaddleLiveKitTransport::try_from(&elem).unwrap_err();
        assert!(matches!(err, TransportParseError::WrongElement));
    }

    #[test]
    fn rejects_invalid_url_scheme() {
        let xml = r#"<transport xmlns='urn:waddle:transports:livekit:0'
                                url='http://livekit/'
                                room='c1'
                                identity='alice@waddle.social/desktop'>
                       <token xmlns='urn:waddle:transports:livekit:0'>jwt</token>
                     </transport>"#;
        let elem: Element = xml.parse().unwrap();
        let err = WaddleLiveKitTransport::try_from(&elem).unwrap_err();
        assert!(matches!(err, TransportParseError::InvalidUrl(_)));
    }
}
