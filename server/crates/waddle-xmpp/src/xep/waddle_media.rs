//! Waddle media join IQs.
//!
//! This keeps call control on XMPP while returning the media engine session
//! details clients need to join LiveKit.

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

use crate::{MediaSessionInfo, MediaType, XmppError};

pub const NS_WADDLE_MEDIA: &str = "urn:waddle:media:0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaJoinRequest {
    pub waddle_id: String,
    pub channel_id: String,
    pub media_type: MediaType,
}

pub fn is_media_join_request(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Get(elem) | IqType::Set(elem)
            if elem.name() == "media" && elem.ns() == NS_WADDLE_MEDIA
    )
}

pub fn parse_media_join_request(iq: &Iq) -> Result<MediaJoinRequest, XmppError> {
    let elem = match &iq.payload {
        IqType::Get(elem) | IqType::Set(elem)
            if elem.name() == "media" && elem.ns() == NS_WADDLE_MEDIA =>
        {
            elem
        }
        _ => {
            return Err(XmppError::bad_request(Some(
                "missing media join payload".to_string(),
            )))
        }
    };

    let waddle_id = elem
        .attr("waddle")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| XmppError::bad_request(Some("missing media waddle attribute".to_string())))?
        .to_string();
    let channel_id = elem
        .attr("channel")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| XmppError::bad_request(Some("missing media channel attribute".to_string())))?
        .to_string();
    let media_type = elem
        .attr("type")
        .unwrap_or("video")
        .parse::<MediaType>()
        .map_err(|err| XmppError::bad_request(Some(err)))?;

    Ok(MediaJoinRequest {
        waddle_id,
        channel_id,
        media_type,
    })
}

pub fn build_media_session_response(original_iq: &Iq, session: &MediaSessionInfo) -> Iq {
    let elem = Element::builder("media", NS_WADDLE_MEDIA)
        .attr("backend", session.backend.as_str())
        .attr("room", session.room_name.as_str())
        .attr("participant", session.participant_id.as_str())
        .attr("name", session.participant_name.as_str())
        .attr("type", session.media_type.as_str())
        .attr("url", session.server_url.as_str())
        .attr("token", session.token.as_str())
        .attr("expires", session.expires_at.as_str())
        .attr(
            "can-publish",
            if session.can_publish { "true" } else { "false" },
        )
        .attr(
            "can-publish-data",
            if session.can_publish_data {
                "true"
            } else {
                "false"
            },
        )
        .attr(
            "can-subscribe",
            if session.can_subscribe { "true" } else { "false" },
        )
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(elem)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_media_join_request() {
        let iq = Iq {
            from: Some("alice@example.com/resource".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "media-1".to_string(),
            payload: IqType::Get(
                Element::builder("media", NS_WADDLE_MEDIA)
                    .attr("waddle", "penguins")
                    .attr("channel", "general")
                    .attr("type", "video")
                    .build(),
            ),
        };

        let request = parse_media_join_request(&iq).unwrap();
        assert_eq!(request.waddle_id, "penguins");
        assert_eq!(request.channel_id, "general");
        assert_eq!(request.media_type, MediaType::Video);
    }

    #[test]
    fn builds_media_join_response() {
        let iq = Iq {
            from: Some("alice@example.com/resource".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "media-1".to_string(),
            payload: IqType::Get(
                Element::builder("media", NS_WADDLE_MEDIA)
                    .attr("waddle", "penguins")
                    .attr("channel", "general")
                    .build(),
            ),
        };

        let response = build_media_session_response(
            &iq,
            &MediaSessionInfo {
                backend: "livekit".to_string(),
                room_name: "waddle-penguins-general".to_string(),
                participant_id: "user-1".to_string(),
                participant_name: "alice".to_string(),
                media_type: MediaType::Audio,
                server_url: "wss://livekit.example.com".to_string(),
                token: "jwt".to_string(),
                expires_at: "2026-04-16T12:00:00Z".to_string(),
                can_publish: true,
                can_publish_data: true,
                can_subscribe: true,
            },
        );

        match response.payload {
            IqType::Result(Some(elem)) => {
                assert_eq!(elem.name(), "media");
                assert_eq!(elem.ns(), NS_WADDLE_MEDIA);
                assert_eq!(elem.attr("backend"), Some("livekit"));
                assert_eq!(elem.attr("room"), Some("waddle-penguins-general"));
                assert_eq!(elem.attr("type"), Some("audio"));
            }
            other => panic!("expected result IQ, got {other:?}"),
        }
    }
}
