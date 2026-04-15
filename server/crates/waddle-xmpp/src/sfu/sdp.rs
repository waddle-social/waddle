//! SDP ↔ Jingle conversion helpers.

use minidom::Element;

const JINGLE_NS: &str = "urn:xmpp:jingle:1";
const ICE_UDP_NS: &str = "urn:xmpp:jingle:transports:ice-udp:1";
const OOB_SDP_NS: &str = "urn:xmpp:jingle:apps:oob-sdp:0";
const RTP_NS: &str = "urn:xmpp:jingle:apps:rtp:1";
const PARTICIPANT_MAP_NS: &str = "urn:waddle:sfu:participant-map:0";

/// Walks `<content>` → `<transport>` → `<sdp>` children to find raw SDP text.
pub fn extract_sdp_from_jingle(jingle: &Element) -> Option<String> {
    for content in jingle.children().filter(|c| c.is("content", JINGLE_NS)) {
        for transport in content.children().filter(|c| c.is("transport", ICE_UDP_NS)) {
            for sdp_el in transport.children().filter(|c| c.is("sdp", OOB_SDP_NS)) {
                let text = sdp_el.text();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    None
}

/// Gets the `sid` attribute from a Jingle element.
pub fn extract_sid(jingle: &Element) -> Option<&str> {
    jingle.attr("sid")
}

/// Gets the `action` attribute from a Jingle element.
pub fn extract_action(jingle: &Element) -> Option<&str> {
    jingle.attr("action")
}

/// Builds a Jingle element with `action="session-accept"`, wrapping the SDP answer
/// inside `<content><transport><sdp>`.
pub fn build_jingle_session_accept(sid: &str, sdp_answer: &str) -> Element {
    Element::builder("jingle", JINGLE_NS)
        .attr("action", "session-accept")
        .attr("sid", sid)
        .append(
            Element::builder("content", JINGLE_NS)
                .attr("creator", "responder")
                .attr("name", "audio")
                .append(
                    Element::builder("description", RTP_NS)
                        .attr("media", "audio")
                        .build(),
                )
                .append(
                    Element::builder("transport", ICE_UDP_NS)
                        .append(
                            Element::builder("sdp", OOB_SDP_NS)
                                .append(minidom::Node::Text(sdp_answer.to_string()))
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Builds a Jingle element with `action="transport-info"` containing the candidate SDP
/// inside `<content><transport><candidate>`.
pub fn build_jingle_transport_info(sid: &str, candidate_sdp: &str) -> Element {
    Element::builder("jingle", JINGLE_NS)
        .attr("action", "transport-info")
        .attr("sid", sid)
        .append(
            Element::builder("content", JINGLE_NS)
                .attr("creator", "responder")
                .attr("name", "audio")
                .append(
                    Element::builder("transport", ICE_UDP_NS)
                        .append(
                            Element::builder("candidate", ICE_UDP_NS)
                                .append(minidom::Node::Text(candidate_sdp.to_string()))
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Builds a Jingle element with `action="session-info"` containing a
/// `<participant-map xmlns="urn:waddle:sfu:participant-map:0">` with
/// `<entry msid="..." jid="...">` elements.
pub fn build_participant_map(sid: &str, mappings: &[(String, String)]) -> Element {
    let mut participant_map = Element::builder("participant-map", PARTICIPANT_MAP_NS);

    for (msid, jid) in mappings {
        participant_map = participant_map.append(
            Element::builder("entry", PARTICIPANT_MAP_NS)
                .attr("msid", msid.as_str())
                .attr("jid", jid.as_str())
                .build(),
        );
    }

    Element::builder("jingle", JINGLE_NS)
        .attr("action", "session-info")
        .attr("sid", sid)
        .append(participant_map.build())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_sdp_offer_from_jingle_element() {
        let sdp_text = "v=0\r\no=- 123 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n";
        let element = minidom::Element::builder("jingle", JINGLE_NS)
            .attr("action", "session-initiate")
            .attr("sid", "test-sid")
            .append(
                minidom::Element::builder("content", JINGLE_NS)
                    .attr("creator", "initiator")
                    .attr("name", "audio")
                    .append(
                        minidom::Element::builder("description", RTP_NS)
                            .attr("media", "audio")
                            .build(),
                    )
                    .append(
                        minidom::Element::builder("transport", ICE_UDP_NS)
                            .append(
                                minidom::Element::builder("sdp", OOB_SDP_NS)
                                    .append(minidom::Node::Text(sdp_text.to_string()))
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();

        let result = extract_sdp_from_jingle(&element);
        assert!(result.is_some());
        assert!(result.unwrap().contains("v=0"));
    }

    #[test]
    fn extracts_sid_and_action() {
        let element = minidom::Element::builder("jingle", JINGLE_NS)
            .attr("action", "session-initiate")
            .attr("sid", "my-session-123")
            .build();
        assert_eq!(extract_sid(&element), Some("my-session-123"));
        assert_eq!(extract_action(&element), Some("session-initiate"));
    }

    #[test]
    fn builds_jingle_accept_with_sdp() {
        let sdp_answer = "v=0\r\no=- 456 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n";
        let element = build_jingle_session_accept("test-sid", sdp_answer);
        assert_eq!(element.attr("action").unwrap(), "session-accept");
        assert_eq!(element.attr("sid").unwrap(), "test-sid");
        // Verify SDP is inside the element tree
        let sdp = extract_sdp_from_jingle(&element);
        assert!(sdp.is_some());
        assert!(sdp.unwrap().contains("v=0"));
    }

    #[test]
    fn builds_participant_map() {
        let mappings = vec![
            ("stream-1".to_string(), "alice@waddle.social".to_string()),
            ("stream-2".to_string(), "bob@waddle.social".to_string()),
        ];
        let element = build_participant_map("sid-123", &mappings);
        assert_eq!(element.attr("action").unwrap(), "session-info");
        assert_eq!(element.attr("sid").unwrap(), "sid-123");
    }
}
