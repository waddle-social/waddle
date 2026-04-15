//! SDP ↔ Jingle conversion helpers.
//!
//! Converts between standard Jingle XML (XEP-0166, XEP-0167, XEP-0176, XEP-0320)
//! and raw SDP strings consumed by str0m.

use minidom::Element;

use crate::xep::xep0166::{
    build_jingle_content_element, parse_jingle_content_element, ContentCreator, JingleContent,
    Senders, NS_JINGLE,
};
use crate::xep::xep0167::{
    build_rtp_description_element, parse_rtp_description_element, MediaType, RtpDescription,
    RtpParameter, RtpPayloadType,
};
use crate::xep::xep0176::{
    build_ice_udp_transport_element, parse_ice_udp_transport_element, CandidateType,
    IceUdpCandidate, IceUdpTransport,
};
use crate::xep::xep0320::{DtlsFingerprint, FingerprintSetup};

const PARTICIPANT_MAP_NS: &str = "urn:waddle:sfu:participant-map:0";

// ---------------------------------------------------------------------------
// Jingle → SDP
// ---------------------------------------------------------------------------

/// Converts parsed Jingle `<content>` elements into a raw SDP string suitable
/// for `str0m::change::SdpOffer::from_sdp_string()`.
pub fn jingle_contents_to_sdp(contents: &[JingleContent]) -> Result<String, String> {
    if contents.is_empty() {
        return Err("No Jingle content elements provided".to_string());
    }

    let mut sdp = String::new();

    // Session-level lines (RFC 4566).
    sdp.push_str("v=0\r\n");
    sdp.push_str("o=- 0 0 IN IP4 0.0.0.0\r\n");
    sdp.push_str("s=-\r\n");
    sdp.push_str("t=0 0\r\n");

    // BUNDLE group containing all content names.
    let mids: Vec<&str> = contents.iter().map(|c| c.name.as_str()).collect();
    sdp.push_str(&format!("a=group:BUNDLE {}\r\n", mids.join(" ")));

    for content in contents {
        // Parse the description element if present.
        let rtp_desc = content
            .description
            .as_ref()
            .and_then(parse_rtp_description_element);

        let media_type = rtp_desc
            .as_ref()
            .map(|d| d.media.as_str().to_owned())
            .unwrap_or_else(|| "application".to_owned());

        // Collect payload type IDs for the m= line.
        let pt_ids: Vec<String> = rtp_desc
            .as_ref()
            .map(|d| d.payload_types.iter().map(|pt| pt.id.to_string()).collect())
            .unwrap_or_default();

        let pt_ids_str = if pt_ids.is_empty() {
            "0".to_owned()
        } else {
            pt_ids.join(" ")
        };

        sdp.push_str(&format!(
            "m={media_type} 9 UDP/TLS/RTP/SAVPF {pt_ids_str}\r\n"
        ));
        sdp.push_str("c=IN IP4 0.0.0.0\r\n");

        // mid
        sdp.push_str(&format!("a=mid:{}\r\n", content.name));

        // Direction from senders attribute.
        let direction = match content.senders {
            Some(Senders::Both) | None => "sendrecv",
            Some(Senders::Initiator) => "sendonly",
            Some(Senders::Responder) => "recvonly",
            Some(Senders::None) => "inactive",
        };
        sdp.push_str(&format!("a={direction}\r\n"));

        // RTP payload descriptions.
        if let Some(ref desc) = rtp_desc {
            for pt in &desc.payload_types {
                if let Some(ref name) = pt.name {
                    let clockrate = pt.clockrate.unwrap_or(8000);
                    let channels_suffix = match pt.channels {
                        Some(ch) if ch > 1 => format!("/{ch}"),
                        _ => String::new(),
                    };
                    sdp.push_str(&format!(
                        "a=rtpmap:{} {}/{}{}\r\n",
                        pt.id, name, clockrate, channels_suffix
                    ));
                }

                if !pt.parameters.is_empty() {
                    let params: Vec<String> = pt
                        .parameters
                        .iter()
                        .map(|p| match &p.value {
                            Some(v) => format!("{}={}", p.name, v),
                            None => p.name.clone(),
                        })
                        .collect();
                    sdp.push_str(&format!("a=fmtp:{} {}\r\n", pt.id, params.join(";")));
                }
            }

            if desc.rtcp_mux {
                sdp.push_str("a=rtcp-mux\r\n");
            }
        }

        // Transport (ICE-UDP + DTLS).
        let transport = content
            .transport
            .as_ref()
            .and_then(parse_ice_udp_transport_element);

        if let Some(ref t) = transport {
            if let Some(ref ufrag) = t.ufrag {
                sdp.push_str(&format!("a=ice-ufrag:{ufrag}\r\n"));
            }
            if let Some(ref pwd) = t.pwd {
                sdp.push_str(&format!("a=ice-pwd:{pwd}\r\n"));
            }

            for fp in &t.fingerprints {
                sdp.push_str(&format!("a=fingerprint:{} {}\r\n", fp.hash, fp.value));
            }
            if let Some(setup) = t.fingerprints.iter().find_map(|fp| fp.setup) {
                sdp.push_str(&format!("a=setup:{}\r\n", setup.as_str()));
            }

            for candidate in &t.candidates {
                sdp.push_str(&format!(
                    "a=candidate:{} {} {} {} {} {} typ {}",
                    candidate.foundation,
                    candidate.component,
                    candidate.protocol,
                    candidate.priority,
                    candidate.ip,
                    candidate.port,
                    candidate.candidate_type.as_str(),
                ));
                if let Some(gen) = candidate.generation {
                    sdp.push_str(&format!(" generation {gen}"));
                }
                sdp.push_str("\r\n");
            }
        }
    }

    Ok(sdp)
}

// ---------------------------------------------------------------------------
// SDP → Jingle
// ---------------------------------------------------------------------------

/// Accumulated state for a single SDP media section being parsed.
struct MediaSectionState {
    media_type: Option<String>,
    mid: Option<String>,
    payload_types: Vec<RtpPayloadType>,
    rtcp_mux: bool,
    ufrag: Option<String>,
    pwd: Option<String>,
    fingerprints: Vec<DtlsFingerprint>,
    candidates: Vec<IceUdpCandidate>,
    senders: Option<Senders>,
    pending_setup: Option<FingerprintSetup>,
}

impl MediaSectionState {
    fn new() -> Self {
        Self {
            media_type: None,
            mid: None,
            payload_types: Vec::new(),
            rtcp_mux: false,
            ufrag: None,
            pwd: None,
            fingerprints: Vec::new(),
            candidates: Vec::new(),
            senders: None,
            pending_setup: None,
        }
    }

    /// Parse an `m=` line and reset state for a new media section.
    fn start_media_section(&mut self, m_line: &str) {
        let parts: Vec<&str> = m_line.splitn(4, ' ').collect();
        if !parts.is_empty() {
            self.media_type = Some(parts[0].to_owned());
        }

        // Collect declared payload type IDs.
        if parts.len() >= 4 {
            for id_str in parts[3].split_whitespace() {
                if let Ok(id) = id_str.parse::<u8>() {
                    // Add a placeholder; rtpmap/fmtp lines will fill details.
                    self.payload_types.push(RtpPayloadType::new(id));
                }
            }
        }
    }

    /// Handle all `a=` attribute lines for the current media section.
    fn parse_attribute(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("a=mid:") {
            self.mid = Some(rest.to_owned());
        } else if line == "a=sendrecv" {
            self.senders = Some(Senders::Both);
        } else if line == "a=sendonly" {
            self.senders = Some(Senders::Initiator);
        } else if line == "a=recvonly" {
            self.senders = Some(Senders::Responder);
        } else if line == "a=inactive" {
            self.senders = Some(Senders::None);
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            self.parse_rtpmap(rest);
        } else if let Some(rest) = line.strip_prefix("a=fmtp:") {
            self.parse_fmtp(rest);
        } else if line == "a=rtcp-mux" {
            self.rtcp_mux = true;
        } else if let Some(rest) = line.strip_prefix("a=ice-ufrag:") {
            self.ufrag = Some(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("a=ice-pwd:") {
            self.pwd = Some(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("a=fingerprint:") {
            if let Some((hash, value)) = rest.split_once(' ') {
                self.fingerprints.push(DtlsFingerprint::new(hash, value));
            }
        } else if let Some(rest) = line.strip_prefix("a=setup:") {
            if let Some(setup) = FingerprintSetup::from_str_attr(rest) {
                // a=setup: is a media-level attribute (RFC 8122) that applies to
                // ALL fingerprints in this section. Apply to every fingerprint that
                // doesn't already have a setup value, and store as pending for any
                // fingerprints parsed later in this section.
                for fp in &mut self.fingerprints {
                    if fp.setup.is_none() {
                        fp.setup = Some(setup);
                    }
                }
                self.pending_setup = Some(setup);
            }
        } else if let Some(rest) = line.strip_prefix("a=candidate:") {
            if let Some(c) = parse_sdp_candidate_line(rest) {
                self.candidates.push(c);
            }
        }
    }

    /// Build a `JingleContent` from accumulated state and push it. Resets state.
    fn flush(&mut self, creator: ContentCreator, contents: &mut Vec<JingleContent>) {
        if self.media_type.is_none() {
            return;
        }

        let mid = self
            .mid
            .take()
            .unwrap_or_else(|| format!("{}", contents.len()));

        let media_str = self.media_type.take().unwrap_or_default();
        let media = MediaType::from_str_attr(&media_str);

        let mut desc = RtpDescription::new(media);
        desc.payload_types = std::mem::take(&mut self.payload_types);
        desc.rtcp_mux = self.rtcp_mux;
        self.rtcp_mux = false;

        // Apply any remaining pending_setup to fingerprints that lack one.
        if let Some(setup) = self.pending_setup.take() {
            for fp in &mut self.fingerprints {
                if fp.setup.is_none() {
                    fp.setup = Some(setup);
                }
            }
        }

        let mut transport = IceUdpTransport::new();
        transport.ufrag = self.ufrag.take();
        transport.pwd = self.pwd.take();
        transport.fingerprints = std::mem::take(&mut self.fingerprints);
        transport.candidates = std::mem::take(&mut self.candidates);

        let content = JingleContent {
            creator,
            name: mid,
            senders: self.senders.take(),
            description: Some(build_rtp_description_element(&desc)),
            transport: Some(build_ice_udp_transport_element(&transport)),
        };
        contents.push(content);
    }

    fn parse_rtpmap(&mut self, rest: &str) {
        // Format: id name/clockrate[/channels]
        if let Some((id_str, encoding)) = rest.split_once(' ') {
            if let Ok(id) = id_str.parse::<u8>() {
                let parts: Vec<&str> = encoding.split('/').collect();
                let name = parts.first().map(|s| (*s).to_owned());
                let clockrate = parts.get(1).and_then(|s| s.parse::<u32>().ok());
                let channels = parts.get(2).and_then(|s| s.parse::<u8>().ok());

                // Update the existing placeholder or add new.
                if let Some(pt) = self.payload_types.iter_mut().find(|pt| pt.id == id) {
                    pt.name = name;
                    pt.clockrate = clockrate;
                    pt.channels = channels;
                } else {
                    let mut pt = RtpPayloadType::new(id);
                    pt.name = name;
                    pt.clockrate = clockrate;
                    pt.channels = channels;
                    self.payload_types.push(pt);
                }
            }
        }
    }

    fn parse_fmtp(&mut self, rest: &str) {
        // Format: id param1=val1;param2=val2;...
        if let Some((id_str, params_str)) = rest.split_once(' ') {
            if let Ok(id) = id_str.parse::<u8>() {
                let params: Vec<RtpParameter> = params_str
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(|p| {
                        if let Some((name, value)) = p.split_once('=') {
                            RtpParameter {
                                name: name.to_owned(),
                                value: Some(value.to_owned()),
                            }
                        } else {
                            RtpParameter {
                                name: p.to_owned(),
                                value: None,
                            }
                        }
                    })
                    .collect();

                if let Some(pt) = self.payload_types.iter_mut().find(|pt| pt.id == id) {
                    pt.parameters = params;
                }
            }
        }
    }
}

/// Parses a raw SDP string line-by-line and returns Jingle `<content>` elements.
pub fn sdp_to_jingle_contents(
    sdp: &str,
    creator: ContentCreator,
) -> Result<Vec<JingleContent>, String> {
    let mut contents: Vec<JingleContent> = Vec::new();
    let mut state = MediaSectionState::new();

    for line in sdp.lines() {
        let line = line.trim_end();

        if let Some(rest) = line.strip_prefix("m=") {
            // Flush previous media section.
            state.flush(creator, &mut contents);
            state.start_media_section(rest);
        } else {
            state.parse_attribute(line);
        }
    }

    // Flush last media section.
    state.flush(creator, &mut contents);

    if contents.is_empty() {
        return Err("No media sections found in SDP".to_string());
    }

    Ok(contents)
}

/// Parses the portion after `a=candidate:` into an `IceUdpCandidate`.
///
/// Expected format:
/// `foundation component protocol priority ip port typ type [generation N]`
fn parse_sdp_candidate_line(line: &str) -> Option<IceUdpCandidate> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    // Minimum: foundation component protocol priority ip port "typ" type = 8 tokens.
    if parts.len() < 8 {
        return None;
    }

    let foundation = parts[0].to_owned();
    let component = parts[1].parse::<u16>().ok()?;
    let protocol = parts[2].to_owned();
    let priority = parts[3].parse::<u32>().ok()?;
    let ip = parts[4].to_owned();
    let port = parts[5].parse::<u16>().ok()?;

    // parts[6] should be "typ"
    if parts[6] != "typ" {
        return None;
    }
    let candidate_type = CandidateType::from_str_attr(parts[7]);

    let mut generation: Option<u32> = None;
    // Parse optional trailing key-value pairs.
    let mut i = 8;
    while i + 1 < parts.len() {
        if parts[i] == "generation" {
            generation = parts[i + 1].parse::<u32>().ok();
        }
        i += 2;
    }

    Some(IceUdpCandidate {
        foundation,
        component,
        protocol,
        priority,
        ip,
        port,
        candidate_type,
        generation,
    })
}

// ---------------------------------------------------------------------------
// High-level helpers
// ---------------------------------------------------------------------------

/// Extracts an SDP offer string from a `<jingle>` element by parsing its
/// `<content>` children with standard Jingle/RTP/ICE-UDP types.
pub fn extract_sdp_offer_from_jingle(jingle: &Element) -> Result<String, String> {
    let contents: Vec<JingleContent> = jingle
        .children()
        .filter(|c| c.is("content", NS_JINGLE))
        .filter_map(parse_jingle_content_element)
        .collect();

    if contents.is_empty() {
        return Err("No <content> elements found in Jingle element".to_string());
    }

    jingle_contents_to_sdp(&contents)
}

/// Builds a `<jingle action="session-accept">` element from an SDP answer string.
pub fn build_jingle_session_accept(sid: &str, sdp_answer: &str) -> Result<Element, String> {
    let contents = sdp_to_jingle_contents(sdp_answer, ContentCreator::Responder)?;

    let mut jingle = Element::builder("jingle", NS_JINGLE)
        .attr("action", "session-accept")
        .attr("sid", sid)
        .build();

    for content in &contents {
        jingle.append_child(build_jingle_content_element(content));
    }

    Ok(jingle)
}

/// Gets the `sid` attribute from a Jingle element.
pub fn extract_sid(jingle: &Element) -> Option<&str> {
    jingle.attr("sid")
}

/// Gets the `action` attribute from a Jingle element.
pub fn extract_action(jingle: &Element) -> Option<&str> {
    jingle.attr("action")
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

    Element::builder("jingle", NS_JINGLE)
        .attr("action", "session-info")
        .attr("sid", sid)
        .append(participant_map.build())
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0167::{
        MediaType, RtpDescription, RtpParameter, RtpPayloadType, NS_JINGLE_RTP,
    };
    use crate::xep::xep0176::{CandidateType, IceUdpCandidate, IceUdpTransport, NS_JINGLE_ICE_UDP};
    use crate::xep::xep0320::{DtlsFingerprint, FingerprintSetup};

    /// Helper: build a JingleContent with audio, one payload type, fingerprint, and candidate.
    fn make_audio_content() -> JingleContent {
        let payload = RtpPayloadType {
            id: 111,
            name: Some("opus".to_owned()),
            clockrate: Some(48000),
            channels: Some(2),
            parameters: vec![RtpParameter {
                name: "minptime".to_owned(),
                value: Some("10".to_owned()),
            }],
        };

        let desc = RtpDescription {
            media: MediaType::Audio,
            payload_types: vec![payload],
            rtcp_mux: true,
        };

        let candidate = IceUdpCandidate::new(
            "1",
            1,
            "udp",
            2130706431,
            "192.0.2.1",
            3478,
            CandidateType::Host,
        );

        let transport = IceUdpTransport {
            ufrag: Some("abc".to_owned()),
            pwd: Some("xyz123".to_owned()),
            fingerprints: vec![DtlsFingerprint::new("sha-256", "AA:BB:CC:DD")
                .with_setup(FingerprintSetup::Actpass)],
            candidates: vec![candidate],
        };

        JingleContent {
            creator: ContentCreator::Initiator,
            name: "audio".to_owned(),
            senders: Some(Senders::Both),
            description: Some(build_rtp_description_element(&desc)),
            transport: Some(build_ice_udp_transport_element(&transport)),
        }
    }

    /// Helper: build a JingleContent with video.
    fn make_video_content() -> JingleContent {
        let payload = RtpPayloadType {
            id: 96,
            name: Some("VP8".to_owned()),
            clockrate: Some(90000),
            channels: None,
            parameters: vec![],
        };

        let desc = RtpDescription {
            media: MediaType::Video,
            payload_types: vec![payload],
            rtcp_mux: true,
        };

        let transport = IceUdpTransport {
            ufrag: Some("def".to_owned()),
            pwd: Some("pwd456".to_owned()),
            fingerprints: vec![
                DtlsFingerprint::new("sha-256", "EE:FF:00:11").with_setup(FingerprintSetup::Active)
            ],
            candidates: vec![],
        };

        JingleContent {
            creator: ContentCreator::Initiator,
            name: "video".to_owned(),
            senders: Some(Senders::Both),
            description: Some(build_rtp_description_element(&desc)),
            transport: Some(build_ice_udp_transport_element(&transport)),
        }
    }

    #[test]
    fn converts_standard_jingle_to_sdp() {
        let content = make_audio_content();
        let sdp = jingle_contents_to_sdp(&[content]).expect("should produce SDP");

        assert!(sdp.contains("v=0\r\n"), "missing v= line");
        assert!(
            sdp.contains("o=- 0 0 IN IP4 0.0.0.0\r\n"),
            "missing o= line"
        );
        assert!(sdp.contains("s=-\r\n"), "missing s= line");
        assert!(sdp.contains("t=0 0\r\n"), "missing t= line");
        assert!(sdp.contains("a=group:BUNDLE audio\r\n"), "missing BUNDLE");
        assert!(
            sdp.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n"),
            "missing m= line"
        );
        assert!(sdp.contains("a=mid:audio\r\n"), "missing mid");
        assert!(sdp.contains("a=sendrecv\r\n"), "missing direction");
        assert!(
            sdp.contains("a=rtpmap:111 opus/48000/2\r\n"),
            "missing rtpmap"
        );
        assert!(sdp.contains("a=fmtp:111 minptime=10\r\n"), "missing fmtp");
        assert!(sdp.contains("a=rtcp-mux\r\n"), "missing rtcp-mux");
        assert!(sdp.contains("a=ice-ufrag:abc\r\n"), "missing ice-ufrag");
        assert!(sdp.contains("a=ice-pwd:xyz123\r\n"), "missing ice-pwd");
        assert!(
            sdp.contains("a=fingerprint:sha-256 AA:BB:CC:DD\r\n"),
            "missing fingerprint"
        );
        assert!(sdp.contains("a=setup:actpass\r\n"), "missing setup");
        assert!(
            sdp.contains("a=candidate:1 1 udp 2130706431 192.0.2.1 3478 typ host\r\n"),
            "missing candidate"
        );
    }

    #[test]
    fn converts_multi_content_jingle() {
        let audio = make_audio_content();
        let video = make_video_content();
        let sdp =
            jingle_contents_to_sdp(&[audio, video]).expect("should produce multi-content SDP");

        assert!(
            sdp.contains("a=group:BUNDLE audio video\r\n"),
            "missing BUNDLE group with both mids"
        );
        assert!(sdp.contains("m=audio 9"), "missing audio m= line");
        assert!(sdp.contains("m=video 9"), "missing video m= line");
        assert!(sdp.contains("a=mid:audio\r\n"), "missing audio mid");
        assert!(sdp.contains("a=mid:video\r\n"), "missing video mid");
        assert!(
            sdp.contains("a=rtpmap:96 VP8/90000\r\n"),
            "missing VP8 rtpmap (no channels suffix for 1/None)"
        );
    }

    #[test]
    fn sdp_roundtrip_preserves_structure() {
        let original_audio = make_audio_content();
        let original_video = make_video_content();

        // Jingle → SDP
        let sdp = jingle_contents_to_sdp(&[original_audio.clone(), original_video.clone()])
            .expect("forward conversion");

        // SDP → Jingle
        let roundtripped =
            sdp_to_jingle_contents(&sdp, ContentCreator::Initiator).expect("reverse conversion");

        assert_eq!(roundtripped.len(), 2, "should have 2 content elements");

        // Check audio content.
        let audio = &roundtripped[0];
        assert_eq!(audio.name, "audio");
        let audio_desc = audio
            .description
            .as_ref()
            .and_then(parse_rtp_description_element)
            .expect("audio description");
        assert_eq!(audio_desc.media, MediaType::Audio);
        assert!(audio_desc.rtcp_mux, "audio rtcp_mux");
        assert_eq!(audio_desc.payload_types.len(), 1);
        assert_eq!(audio_desc.payload_types[0].id, 111);
        assert_eq!(audio_desc.payload_types[0].name.as_deref(), Some("opus"));
        assert_eq!(audio_desc.payload_types[0].clockrate, Some(48000));
        assert_eq!(audio_desc.payload_types[0].channels, Some(2));

        let audio_transport = audio
            .transport
            .as_ref()
            .and_then(parse_ice_udp_transport_element)
            .expect("audio transport");
        assert_eq!(audio_transport.ufrag.as_deref(), Some("abc"));
        assert_eq!(audio_transport.fingerprints.len(), 1);
        assert_eq!(audio_transport.fingerprints[0].hash, "sha-256");
        assert_eq!(
            audio_transport.fingerprints[0].setup,
            Some(FingerprintSetup::Actpass)
        );

        // Check video content.
        let video = &roundtripped[1];
        assert_eq!(video.name, "video");
        let video_desc = video
            .description
            .as_ref()
            .and_then(parse_rtp_description_element)
            .expect("video description");
        assert_eq!(video_desc.media, MediaType::Video);
        assert!(video_desc.rtcp_mux, "video rtcp_mux");
        assert_eq!(video_desc.payload_types[0].id, 96);
    }

    #[test]
    fn parses_sdp_candidate_line() {
        let line = "1 1 udp 2130706431 192.0.2.1 3478 typ host generation 0";
        let candidate = parse_sdp_candidate_line(line).expect("should parse candidate");

        assert_eq!(candidate.foundation, "1");
        assert_eq!(candidate.component, 1);
        assert_eq!(candidate.protocol, "udp");
        assert_eq!(candidate.priority, 2130706431);
        assert_eq!(candidate.ip, "192.0.2.1");
        assert_eq!(candidate.port, 3478);
        assert_eq!(candidate.candidate_type, CandidateType::Host);
        assert_eq!(candidate.generation, Some(0));
    }

    #[test]
    fn parses_sdp_candidate_line_without_generation() {
        let line = "2 1 tcp 1015021823 10.0.0.1 9 typ srflx";
        let candidate =
            parse_sdp_candidate_line(line).expect("should parse candidate without generation");
        assert_eq!(candidate.foundation, "2");
        assert_eq!(candidate.candidate_type, CandidateType::Srflx);
        assert_eq!(candidate.generation, None);
    }

    #[test]
    fn extract_sdp_from_standard_jingle_element() {
        let xml = "\
            <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='test-sid'>\
              <content xmlns='urn:xmpp:jingle:1' creator='initiator' name='audio' senders='both'>\
                <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'>\
                  <payload-type id='111' name='opus' clockrate='48000' channels='2'/>\
                  <rtcp-mux/>\
                </description>\
                <transport xmlns='urn:xmpp:jingle:transports:ice-udp:1' ufrag='u1' pwd='p1'>\
                  <fingerprint xmlns='urn:xmpp:jingle:apps:dtls:0' hash='sha-256' setup='actpass'>AB:CD:EF</fingerprint>\
                  <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' \
                    foundation='1' component='1' protocol='udp' priority='2130706431' \
                    ip='192.0.2.1' port='3478' type='host'/>\
                </transport>\
              </content>\
            </jingle>";

        let element = xml.parse::<Element>().expect("valid xml");
        let sdp = extract_sdp_offer_from_jingle(&element).expect("should extract SDP");

        assert!(sdp.contains("v=0\r\n"), "SDP should start with v=0");
        assert!(sdp.contains("m=audio 9"), "SDP should have audio m= line");
        assert!(
            sdp.contains("a=rtpmap:111 opus/48000/2"),
            "SDP should have rtpmap"
        );
        assert!(sdp.contains("a=ice-ufrag:u1"), "SDP should have ice-ufrag");
        assert!(
            sdp.contains("a=fingerprint:sha-256 AB:CD:EF"),
            "SDP should have fingerprint"
        );
        assert!(
            sdp.contains("a=candidate:1 1 udp 2130706431 192.0.2.1 3478 typ host"),
            "SDP should have candidate"
        );
    }

    #[test]
    fn build_session_accept_produces_valid_jingle() {
        let sdp_answer = "\
            v=0\r\n\
            o=- 0 0 IN IP4 0.0.0.0\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE audio\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:audio\r\n\
            a=sendrecv\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=rtcp-mux\r\n\
            a=ice-ufrag:resp-u\r\n\
            a=ice-pwd:resp-p\r\n\
            a=fingerprint:sha-256 11:22:33:44\r\n\
            a=setup:active\r\n";

        let element =
            build_jingle_session_accept("sid-99", sdp_answer).expect("should build accept");

        assert_eq!(element.attr("action"), Some("session-accept"));
        assert_eq!(element.attr("sid"), Some("sid-99"));
        assert_eq!(element.ns(), NS_JINGLE);

        // Should have at least one content child.
        let content_count = element
            .children()
            .filter(|c| c.is("content", NS_JINGLE))
            .count();
        assert_eq!(content_count, 1, "should have 1 content child");

        let content_el = element
            .children()
            .find(|c| c.is("content", NS_JINGLE))
            .expect("content element");
        assert_eq!(content_el.attr("creator"), Some("responder"));
        assert_eq!(content_el.attr("name"), Some("audio"));

        // Verify description child.
        let desc_el = content_el
            .children()
            .find(|c| c.is("description", NS_JINGLE_RTP))
            .expect("description element");
        assert_eq!(desc_el.attr("media"), Some("audio"));

        // Verify transport child.
        let transport_el = content_el
            .children()
            .find(|c| c.is("transport", NS_JINGLE_ICE_UDP))
            .expect("transport element");
        assert_eq!(transport_el.attr("ufrag"), Some("resp-u"));
        assert_eq!(transport_el.attr("pwd"), Some("resp-p"));
    }

    #[test]
    fn extracts_sid_and_action() {
        let element = Element::builder("jingle", NS_JINGLE)
            .attr("action", "session-initiate")
            .attr("sid", "my-session-123")
            .build();
        assert_eq!(extract_sid(&element), Some("my-session-123"));
        assert_eq!(extract_action(&element), Some("session-initiate"));
    }

    #[test]
    fn builds_participant_map() {
        let mappings = vec![
            ("stream-1".to_string(), "alice@waddle.social".to_string()),
            ("stream-2".to_string(), "bob@waddle.social".to_string()),
        ];
        let element = build_participant_map("sid-123", &mappings);
        assert_eq!(element.attr("action"), Some("session-info"));
        assert_eq!(element.attr("sid"), Some("sid-123"));
    }

    #[test]
    fn jingle_contents_to_sdp_rejects_empty_contents() {
        let result = jingle_contents_to_sdp(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn sdp_to_jingle_contents_rejects_empty_sdp() {
        let result = sdp_to_jingle_contents("v=0\r\n", ContentCreator::Initiator);
        assert!(result.is_err());
    }
}
