# SFU Call Fixes — "Waiting for Others to Join" Bug

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the SFU group call system so that multiple participants can join a call, see each other's video, and the "Waiting for others to join" message disappears when someone connects.

**Architecture:** The SFU server invented a non-standard OOB SDP format that stanza.js doesn't speak. The XMPP-native fix: implement proper Jingle XML ↔ SDP conversion on the server using the existing XEP-0167/0176/0320 types, so the SFU speaks standard Jingle. Stanza.js stays untouched. Also fix SID collision (one-line client change), implement the UDP media forwarding loop, and add shared peer storage.

**Tech Stack:** Rust + str0m 0.18 + Kameo actors (server), TypeScript + stanza.js Jingle (unchanged) + Vue 3 (frontend)

---

## Root Cause Analysis

### Bug 1: SDP/Jingle Format Mismatch (CRITICAL — nothing works)

**Client sends:** Standard Jingle XML via stanza.js — `<description xmlns="urn:xmpp:jingle:apps:rtp:1">` with `<payload-type>` children, `<transport xmlns="urn:xmpp:jingle:transports:ice-udp:1">` with `<candidate>` and `<fingerprint>` children. This is correct per XEP-0167/0176/0320.

**Server expects:** Raw SDP text inside a non-standard `<sdp xmlns="urn:xmpp:jingle:apps:oob-sdp:0">` element. This namespace doesn't exist in any XEP.

**Result:** `extract_sdp_from_jingle()` returns `None` for every valid Jingle stanza → server rejects with "Missing SDP offer." Same mismatch in reverse for session-accept.

**Fix:** Replace `extract_sdp_from_jingle()` with `jingle_to_sdp()` that parses standard Jingle XML using existing XEP types and produces an SDP string for str0m. Replace `build_jingle_session_accept()` with `sdp_to_jingle_accept()` that parses str0m's SDP answer and builds standard Jingle XML.

### Bug 2: SID Collision (HIGH — multi-party broken)

`client.ts:398` — `joinMujiCall()` reuses the invite's SID. Server `room_actor.rs:66` — `peers.insert(msg.sid.clone(), peer)` overwrites previous entry.

**Fix:** One line — always generate unique SID in `joinMujiCall()`.

### Bug 3: UDP Forwarding Not Implemented (CRITICAL — no media flows)

`net.rs:37` logs and drops every UDP packet. ICE checks never reach str0m.

**Fix:** Implement full demux/poll loop.

### Bug 4: No Participant Notifications (MEDIUM)

`build_participant_map()` exists but is never called.

---

## File Structure

### Files to Modify

- `server/crates/waddle-xmpp/src/sfu/sdp.rs` — Replace OOB SDP functions with proper Jingle XML ↔ SDP conversion
- `server/crates/waddle-xmpp/src/sfu/service_actor.rs` — Use XEP-0166 Jingle parser, call new conversion functions
- `server/crates/waddle-xmpp/src/sfu/mod.rs` — Add `PeerStore` for shared peer access
- `server/crates/waddle-xmpp/src/sfu/room_actor.rs` — Use `PeerStore` instead of private HashMap
- `server/crates/waddle-xmpp/src/sfu/peer.rs` — Add `room_key` field
- `server/crates/waddle-xmpp/src/sfu/net.rs` — Full UDP demux + str0m polling loop
- `server/crates/waddle-xmpp/src/server.rs` — Pass `PeerStore` to net loop
- `chat/src/lib/xmpp/client.ts` — One-line SID fix in `joinMujiCall()`

---

## Task 1: Implement Jingle XML → SDP Conversion

**Why:** The SFU uses str0m which speaks raw SDP strings. We need to convert the standard Jingle XML (that stanza.js sends) into an SDP string that `SdpOffer::from_sdp_string()` accepts. Uses existing XEP-0166, XEP-0167, XEP-0176, and XEP-0320 parsers.

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/sdp.rs`
- Test: inline `#[cfg(test)]` module in same file

- [ ] **Step 1: Write the failing test**

Add to `sdp.rs` tests:

```rust
#[test]
fn converts_standard_jingle_to_sdp() {
    use crate::xep::xep0166::{JingleContent, ContentCreator, Senders};
    use crate::xep::xep0167::*;
    use crate::xep::xep0176::*;
    use crate::xep::xep0320::*;

    let content = JingleContent {
        creator: ContentCreator::Initiator,
        name: "0".to_owned(),
        senders: Some(Senders::Both),
        description: Some(build_rtp_description_element(
            &RtpDescription::new(MediaType::Audio)
                .with_payload_type(RtpPayloadType {
                    id: 111,
                    name: Some("opus".to_owned()),
                    clockrate: Some(48000),
                    channels: Some(2),
                    parameters: vec![RtpParameter {
                        name: "minptime".to_owned(),
                        value: Some("10".to_owned()),
                    }],
                }),
        )),
        transport: Some(build_ice_udp_transport_element(
            &IceUdpTransport::new()
                .with_fingerprint(
                    DtlsFingerprint::new("sha-256", "AA:BB:CC")
                        .with_setup(FingerprintSetup::Actpass),
                )
                .with_candidate(IceUdpCandidate::new(
                    "1", 1, "udp", 2130706431, "192.168.1.1", 54321, CandidateType::Host,
                )),
        )),
    };

    let sdp = jingle_contents_to_sdp(&[content]).expect("conversion should succeed");
    assert!(sdp.starts_with("v=0\r\n"));
    assert!(sdp.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n"));
    assert!(sdp.contains("a=rtpmap:111 opus/48000/2\r\n"));
    assert!(sdp.contains("a=fmtp:111 minptime=10\r\n"));
    assert!(sdp.contains("a=ice-ufrag:"));
    assert!(sdp.contains("a=fingerprint:sha-256 AA:BB:CC\r\n"));
    assert!(sdp.contains("a=setup:actpass\r\n"));
    assert!(sdp.contains("a=mid:0\r\n"));
    assert!(sdp.contains("a=sendrecv\r\n"));
    assert!(sdp.contains("typ host\r\n"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd server && cargo test -p waddle-xmpp -- sdp::tests::converts_standard_jingle_to_sdp`

Expected: FAIL — `jingle_contents_to_sdp` doesn't exist yet.

- [ ] **Step 3: Implement `jingle_contents_to_sdp`**

Replace the contents of `server/crates/waddle-xmpp/src/sfu/sdp.rs` with:

```rust
//! SDP ↔ Jingle conversion — standard XEP-0167/0176/0320 format.
//!
//! Converts between str0m's raw SDP strings and standard Jingle XML
//! using the server's existing XEP type system.

use crate::xep::xep0166::{JingleContent, ContentCreator, Senders};
use crate::xep::xep0167::*;
use crate::xep::xep0176::*;
use crate::xep::xep0320::*;
use minidom::Element;

const JINGLE_NS: &str = "urn:xmpp:jingle:1";
const PARTICIPANT_MAP_NS: &str = "urn:waddle:sfu:participant-map:0";

/// Convert parsed Jingle `<content>` elements into a raw SDP string
/// suitable for `str0m::change::SdpOffer::from_sdp_string()`.
pub fn jingle_contents_to_sdp(contents: &[JingleContent]) -> Result<String, String> {
    if contents.is_empty() {
        return Err("No Jingle content elements".to_string());
    }

    let mut sdp = String::new();

    // Session-level SDP lines
    sdp.push_str("v=0\r\n");
    sdp.push_str("o=- 0 0 IN IP4 0.0.0.0\r\n");
    sdp.push_str("s=-\r\n");
    sdp.push_str("t=0 0\r\n");

    // BUNDLE group — all content names
    let mids: Vec<&str> = contents.iter().map(|c| c.name.as_str()).collect();
    sdp.push_str(&format!("a=group:BUNDLE {}\r\n", mids.join(" ")));
    sdp.push_str("a=msid-semantic:WMS *\r\n");

    for content in contents {
        let desc = content
            .description
            .as_ref()
            .and_then(parse_rtp_description_element)
            .ok_or_else(|| format!("Missing RTP description in content '{}'", content.name))?;

        let transport = content
            .transport
            .as_ref()
            .and_then(parse_ice_udp_transport_element)
            .ok_or_else(|| format!("Missing ICE transport in content '{}'", content.name))?;

        // m= line
        let pt_ids: Vec<String> = desc.payload_types.iter().map(|p| p.id.to_string()).collect();
        let pt_list = if pt_ids.is_empty() {
            "0".to_string()
        } else {
            pt_ids.join(" ")
        };
        sdp.push_str(&format!(
            "m={} 9 UDP/TLS/RTP/SAVPF {}\r\n",
            desc.media.as_str(),
            pt_list
        ));
        sdp.push_str("c=IN IP4 0.0.0.0\r\n");
        sdp.push_str(&format!("a=mid:{}\r\n", content.name));

        // ICE credentials
        if let Some(ref ufrag) = transport.ufrag {
            sdp.push_str(&format!("a=ice-ufrag:{}\r\n", ufrag));
        }
        if let Some(ref pwd) = transport.pwd {
            sdp.push_str(&format!("a=ice-pwd:{}\r\n", pwd));
        }

        // DTLS fingerprint(s)
        for fp in &transport.fingerprints {
            sdp.push_str(&format!("a=fingerprint:{} {}\r\n", fp.hash, fp.value));
            if let Some(setup) = fp.setup {
                sdp.push_str(&format!("a=setup:{}\r\n", setup.as_str()));
            }
        }

        // Direction
        let direction = match content.senders {
            Some(Senders::Both) | None => "sendrecv",
            Some(Senders::Initiator) => "sendonly",
            Some(Senders::Responder) => "recvonly",
            Some(Senders::None) => "inactive",
        };
        sdp.push_str(&format!("a={}\r\n", direction));

        // Codec descriptions
        for pt in &desc.payload_types {
            let name = pt.name.as_deref().unwrap_or("unknown");
            let clockrate = pt.clockrate.unwrap_or(8000);
            match pt.channels {
                Some(ch) if ch > 1 => {
                    sdp.push_str(&format!(
                        "a=rtpmap:{} {}/{}/{}\r\n",
                        pt.id, name, clockrate, ch
                    ));
                }
                _ => {
                    sdp.push_str(&format!("a=rtpmap:{} {}/{}\r\n", pt.id, name, clockrate));
                }
            }

            // fmtp parameters
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

        // ICE candidates
        for c in &transport.candidates {
            sdp.push_str(&format!(
                "a=candidate:{} {} {} {} {} {} typ {}",
                c.foundation,
                c.component,
                c.protocol,
                c.priority,
                c.ip,
                c.port,
                c.candidate_type.as_str()
            ));
            if let Some(gen) = c.generation {
                sdp.push_str(&format!(" generation {}", gen));
            }
            sdp.push_str("\r\n");
        }
    }

    Ok(sdp)
}

/// Gets the `sid` attribute from a Jingle element.
pub fn extract_sid(jingle: &Element) -> Option<&str> {
    jingle.attr("sid")
}

/// Gets the `action` attribute from a Jingle element.
pub fn extract_action(jingle: &Element) -> Option<&str> {
    jingle.attr("action")
}

/// Parse standard Jingle `<content>` children from a `<jingle>` element,
/// then convert to an SDP string for str0m.
///
/// This replaces the old `extract_sdp_from_jingle()` which expected
/// a non-standard OOB SDP format.
pub fn extract_sdp_offer_from_jingle(jingle: &Element) -> Result<String, String> {
    use crate::xep::xep0166::parse_jingle_content_element;

    let contents: Vec<JingleContent> = jingle
        .children()
        .filter(|child| child.name() == "content" && child.ns() == JINGLE_NS)
        .filter_map(parse_jingle_content_element)
        .collect();

    if contents.is_empty() {
        return Err("No valid <content> elements in Jingle stanza".to_string());
    }

    jingle_contents_to_sdp(&contents)
}

/// Parse a raw SDP answer string from str0m into a Jingle `session-accept`
/// element with standard XEP-0167/0176/0320 content.
pub fn build_jingle_session_accept(sid: &str, sdp_answer: &str) -> Result<Element, String> {
    let contents = sdp_to_jingle_contents(sdp_answer, ContentCreator::Responder)?;

    let mut jingle = Element::builder("jingle", JINGLE_NS)
        .attr("action", "session-accept")
        .attr("sid", sid)
        .build();

    for content in &contents {
        jingle.append_child(crate::xep::xep0166::build_jingle_content_element(content));
    }

    Ok(jingle)
}

/// Parse a raw SDP string into Jingle `<content>` structures.
///
/// Walks the SDP line-by-line, accumulating state for each `m=` section,
/// then builds `JingleContent` with `RtpDescription` and `IceUdpTransport`.
pub fn sdp_to_jingle_contents(
    sdp: &str,
    creator: ContentCreator,
) -> Result<Vec<JingleContent>, String> {
    let mut contents: Vec<JingleContent> = Vec::new();
    let mut current: Option<MediaSectionState> = None;

    for line in sdp.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("m=") {
            // Flush previous media section
            if let Some(state) = current.take() {
                contents.push(state.into_jingle_content(creator));
            }
            current = Some(MediaSectionState::from_m_line(rest));
        } else if let Some(ref mut state) = current {
            state.parse_attribute(line);
        }
        // Session-level lines (v=, o=, s=, etc.) are ignored — we only
        // need media-level data for Jingle content elements.
    }

    // Flush last media section
    if let Some(state) = current.take() {
        contents.push(state.into_jingle_content(creator));
    }

    if contents.is_empty() {
        return Err("No media sections found in SDP".to_string());
    }

    Ok(contents)
}

/// Accumulates parsed state for one SDP `m=` section.
struct MediaSectionState {
    media_type: String,
    pt_ids: Vec<u8>,
    mid: Option<String>,
    ufrag: Option<String>,
    pwd: Option<String>,
    fingerprints: Vec<DtlsFingerprint>,
    candidates: Vec<IceUdpCandidate>,
    rtpmaps: Vec<(u8, String, u32, Option<u8>)>, // (id, name, clockrate, channels)
    fmtps: Vec<(u8, String)>,                      // (id, params)
    rtcp_mux: bool,
    direction: Option<Senders>,
    // Track the last fingerprint for associating setup attribute
    pending_setup: Option<FingerprintSetup>,
}

impl MediaSectionState {
    fn from_m_line(rest: &str) -> Self {
        // m=audio 9 UDP/TLS/RTP/SAVPF 111 0 8
        let parts: Vec<&str> = rest.splitn(4, ' ').collect();
        let media_type = parts.first().unwrap_or(&"audio").to_string();
        let pt_ids = parts
            .get(3)
            .unwrap_or(&"")
            .split_whitespace()
            .filter_map(|s| s.parse::<u8>().ok())
            .collect();

        MediaSectionState {
            media_type,
            pt_ids,
            mid: None,
            ufrag: None,
            pwd: None,
            fingerprints: Vec::new(),
            candidates: Vec::new(),
            rtpmaps: Vec::new(),
            fmtps: Vec::new(),
            rtcp_mux: false,
            direction: None,
            pending_setup: None,
        }
    }

    fn parse_attribute(&mut self, line: &str) {
        if let Some(mid) = line.strip_prefix("a=mid:") {
            self.mid = Some(mid.to_string());
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            // a=rtpmap:111 opus/48000/2
            if let Some((id_str, encoding)) = rest.split_once(' ') {
                if let Ok(id) = id_str.parse::<u8>() {
                    let parts: Vec<&str> = encoding.split('/').collect();
                    let name = parts.first().unwrap_or(&"unknown").to_string();
                    let clockrate = parts
                        .get(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(8000);
                    let channels = parts.get(2).and_then(|s| s.parse().ok());
                    self.rtpmaps.push((id, name, clockrate, channels));
                }
            }
        } else if let Some(rest) = line.strip_prefix("a=fmtp:") {
            // a=fmtp:111 minptime=10;useinbandfec=1
            if let Some((id_str, params)) = rest.split_once(' ') {
                if let Ok(id) = id_str.parse::<u8>() {
                    self.fmtps.push((id, params.to_string()));
                }
            }
        } else if line == "a=rtcp-mux" {
            self.rtcp_mux = true;
        } else if let Some(ufrag) = line.strip_prefix("a=ice-ufrag:") {
            self.ufrag = Some(ufrag.to_string());
        } else if let Some(pwd) = line.strip_prefix("a=ice-pwd:") {
            self.pwd = Some(pwd.to_string());
        } else if let Some(rest) = line.strip_prefix("a=fingerprint:") {
            // a=fingerprint:sha-256 AA:BB:CC:DD:...
            if let Some((hash, value)) = rest.split_once(' ') {
                let mut fp = DtlsFingerprint::new(hash, value);
                if let Some(setup) = self.pending_setup.take() {
                    fp.setup = Some(setup);
                }
                self.fingerprints.push(fp);
            }
        } else if let Some(setup_str) = line.strip_prefix("a=setup:") {
            let setup = FingerprintSetup::from_str_attr(setup_str);
            // Apply to the last fingerprint if one exists, otherwise hold
            if let Some(last_fp) = self.fingerprints.last_mut() {
                last_fp.setup = setup;
            } else {
                self.pending_setup = setup;
            }
        } else if let Some(rest) = line.strip_prefix("a=candidate:") {
            if let Some(candidate) = parse_sdp_candidate_line(rest) {
                self.candidates.push(candidate);
            }
        } else if line == "a=sendrecv" {
            self.direction = Some(Senders::Both);
        } else if line == "a=sendonly" {
            self.direction = Some(Senders::Initiator);
        } else if line == "a=recvonly" {
            self.direction = Some(Senders::Responder);
        } else if line == "a=inactive" {
            self.direction = Some(Senders::None);
        }
    }

    fn into_jingle_content(self, creator: ContentCreator) -> JingleContent {
        // Build payload types by merging rtpmap + fmtp data
        let mut payload_types: Vec<RtpPayloadType> = Vec::new();

        for id in &self.pt_ids {
            let rtpmap = self.rtpmaps.iter().find(|(rid, _, _, _)| rid == id);
            let fmtp = self.fmtps.iter().find(|(fid, _)| fid == id);

            let mut pt = RtpPayloadType::new(*id);
            if let Some((_, name, clockrate, channels)) = rtpmap {
                pt.name = Some(name.clone());
                pt.clockrate = Some(*clockrate);
                pt.channels = *channels;
            }
            if let Some((_, params_str)) = fmtp {
                pt.parameters = params_str
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(|param| {
                        if let Some((name, value)) = param.split_once('=') {
                            RtpParameter {
                                name: name.to_string(),
                                value: Some(value.to_string()),
                            }
                        } else {
                            RtpParameter {
                                name: param.to_string(),
                                value: None,
                            }
                        }
                    })
                    .collect();
            }
            payload_types.push(pt);
        }

        let media_type = MediaType::from_str_attr(&self.media_type);
        let mut rtp_desc = RtpDescription::new(media_type);
        rtp_desc.payload_types = payload_types;
        rtp_desc.rtcp_mux = self.rtcp_mux;

        let mut transport = IceUdpTransport {
            ufrag: self.ufrag,
            pwd: self.pwd,
            fingerprints: self.fingerprints,
            candidates: self.candidates,
        };

        // If we have a pending setup but no fingerprints absorbed it, ignore it
        // (shouldn't happen in practice)

        let mid = self.mid.unwrap_or_else(|| "0".to_string());

        JingleContent {
            creator,
            name: mid,
            senders: self.direction,
            description: Some(build_rtp_description_element(&rtp_desc)),
            transport: Some(build_ice_udp_transport_element(&transport)),
        }
    }
}

/// Parse an SDP `a=candidate:` line (after the prefix) into an `IceUdpCandidate`.
///
/// Format: `foundation component protocol priority ip port typ type [generation N]`
fn parse_sdp_candidate_line(line: &str) -> Option<IceUdpCandidate> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 8 {
        return None;
    }
    // parts: [foundation, component, protocol, priority, ip, port, "typ", type, ...]
    if parts.get(6) != Some(&"typ") {
        return None;
    }
    let foundation = parts[0].to_string();
    let component = parts[1].parse::<u16>().ok()?;
    let protocol = parts[2].to_string();
    let priority = parts[3].parse::<u32>().ok()?;
    let ip = parts[4].to_string();
    let port = parts[5].parse::<u16>().ok()?;
    let candidate_type = CandidateType::from_str_attr(parts[7]);

    let mut generation = None;
    // Look for "generation N" pair
    let mut i = 8;
    while i + 1 < parts.len() {
        if parts[i] == "generation" {
            generation = parts[i + 1].parse::<u32>().ok();
            break;
        }
        i += 2; // extension attributes come in pairs
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

/// Builds a `<jingle>` element with `action="session-info"` containing a
/// `<participant-map>` with `<entry msid="..." jid="...">` elements.
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
    use crate::xep::xep0166::{ContentCreator, JingleContent, Senders};
    use crate::xep::xep0167::*;
    use crate::xep::xep0176::*;
    use crate::xep::xep0320::*;

    fn make_audio_content() -> JingleContent {
        JingleContent {
            creator: ContentCreator::Initiator,
            name: "0".to_owned(),
            senders: Some(Senders::Both),
            description: Some(build_rtp_description_element(
                &RtpDescription::new(MediaType::Audio)
                    .with_payload_type(RtpPayloadType {
                        id: 111,
                        name: Some("opus".to_owned()),
                        clockrate: Some(48000),
                        channels: Some(2),
                        parameters: vec![RtpParameter {
                            name: "minptime".to_owned(),
                            value: Some("10".to_owned()),
                        }],
                    }),
            )),
            transport: Some(build_ice_udp_transport_element(
                &IceUdpTransport::new()
                    .with_fingerprint(
                        DtlsFingerprint::new("sha-256", "AA:BB:CC")
                            .with_setup(FingerprintSetup::Actpass),
                    )
                    .with_candidate(IceUdpCandidate::new(
                        "1",
                        1,
                        "udp",
                        2130706431,
                        "192.168.1.1",
                        54321,
                        CandidateType::Host,
                    )),
            )),
        }
    }

    #[test]
    fn converts_standard_jingle_to_sdp() {
        let content = make_audio_content();
        let sdp = jingle_contents_to_sdp(&[content]).expect("conversion should succeed");

        assert!(sdp.starts_with("v=0\r\n"));
        assert!(sdp.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n"));
        assert!(sdp.contains("a=rtpmap:111 opus/48000/2\r\n"));
        assert!(sdp.contains("a=fmtp:111 minptime=10\r\n"));
        assert!(sdp.contains("a=ice-ufrag:"));
        assert!(sdp.contains("a=fingerprint:sha-256 AA:BB:CC\r\n"));
        assert!(sdp.contains("a=setup:actpass\r\n"));
        assert!(sdp.contains("a=mid:0\r\n"));
        assert!(sdp.contains("a=sendrecv\r\n"));
        assert!(sdp.contains("typ host\r\n"));
    }

    #[test]
    fn converts_multi_content_jingle() {
        let audio = make_audio_content();
        let mut video = make_audio_content();
        video.name = "1".to_owned();
        video.description = Some(build_rtp_description_element(
            &RtpDescription::new(MediaType::Video)
                .with_payload_type(RtpPayloadType {
                    id: 96,
                    name: Some("VP8".to_owned()),
                    clockrate: Some(90000),
                    channels: None,
                    parameters: vec![],
                }),
        ));

        let sdp = jingle_contents_to_sdp(&[audio, video]).expect("conversion should succeed");
        assert!(sdp.contains("a=group:BUNDLE 0 1\r\n"));
        assert!(sdp.contains("m=audio 9"));
        assert!(sdp.contains("m=video 9"));
    }

    #[test]
    fn sdp_roundtrip_preserves_structure() {
        let original = make_audio_content();
        let sdp = jingle_contents_to_sdp(&[original]).expect("to sdp");
        let roundtripped =
            sdp_to_jingle_contents(&sdp, ContentCreator::Responder).expect("from sdp");

        assert_eq!(roundtripped.len(), 1);
        let content = &roundtripped[0];
        assert_eq!(content.name, "0");

        let desc = parse_rtp_description_element(content.description.as_ref().unwrap()).unwrap();
        assert_eq!(desc.media, MediaType::Audio);
        assert_eq!(desc.payload_types.len(), 1);
        assert_eq!(desc.payload_types[0].id, 111);
        assert_eq!(desc.payload_types[0].name.as_deref(), Some("opus"));
        assert!(desc.rtcp_mux);

        let transport =
            parse_ice_udp_transport_element(content.transport.as_ref().unwrap()).unwrap();
        assert!(!transport.fingerprints.is_empty());
        assert_eq!(transport.fingerprints[0].hash, "sha-256");
    }

    #[test]
    fn parses_sdp_candidate_line() {
        let candidate = parse_sdp_candidate_line(
            "1 1 udp 2130706431 192.168.1.1 54321 typ host generation 0",
        )
        .expect("should parse");
        assert_eq!(candidate.foundation, "1");
        assert_eq!(candidate.component, 1);
        assert_eq!(candidate.priority, 2130706431);
        assert_eq!(candidate.ip, "192.168.1.1");
        assert_eq!(candidate.port, 54321);
        assert_eq!(candidate.candidate_type, CandidateType::Host);
        assert_eq!(candidate.generation, Some(0));
    }

    #[test]
    fn extract_sdp_from_standard_jingle_element() {
        let jingle_xml = format!(
            "<jingle xmlns='{}' action='session-initiate' sid='test-sid'>\
               <content xmlns='{}' creator='initiator' name='0'>\
                 <description xmlns='{}' media='audio'>\
                   <payload-type xmlns='{}' id='111' name='opus' clockrate='48000' channels='2'/>\
                   <rtcp-mux xmlns='{}'/>\
                 </description>\
                 <transport xmlns='{}' ufrag='abcd' pwd='efgh'>\
                   <fingerprint xmlns='{}' hash='sha-256' setup='actpass'>AA:BB</fingerprint>\
                 </transport>\
               </content>\
             </jingle>",
            JINGLE_NS,
            JINGLE_NS,
            NS_JINGLE_RTP,
            NS_JINGLE_RTP,
            NS_JINGLE_RTP,
            NS_JINGLE_ICE_UDP,
            NS_JINGLE_DTLS,
        );
        let element = jingle_xml.parse::<Element>().expect("valid xml");
        let sdp = extract_sdp_offer_from_jingle(&element).expect("should convert");
        assert!(sdp.contains("m=audio"));
        assert!(sdp.contains("a=ice-ufrag:abcd"));
    }

    #[test]
    fn build_session_accept_produces_valid_jingle() {
        let sdp = "v=0\r\n\
                    o=- 0 0 IN IP4 0.0.0.0\r\n\
                    s=-\r\n\
                    t=0 0\r\n\
                    m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
                    c=IN IP4 0.0.0.0\r\n\
                    a=mid:0\r\n\
                    a=ice-ufrag:serverufrag\r\n\
                    a=ice-pwd:serverpwd\r\n\
                    a=fingerprint:sha-256 11:22:33\r\n\
                    a=setup:active\r\n\
                    a=rtpmap:111 opus/48000/2\r\n\
                    a=sendrecv\r\n\
                    a=rtcp-mux\r\n";

        let jingle = build_jingle_session_accept("test-sid", sdp).expect("should build");
        assert_eq!(jingle.attr("action").unwrap(), "session-accept");
        assert_eq!(jingle.attr("sid").unwrap(), "test-sid");

        // Verify it contains a content element with transport
        let content = jingle
            .children()
            .find(|c| c.name() == "content")
            .expect("should have content");
        let transport = content
            .children()
            .find(|c| c.name() == "transport")
            .expect("should have transport");
        let fp = transport
            .children()
            .find(|c| c.name() == "fingerprint")
            .expect("should have fingerprint");
        assert_eq!(fp.text(), "11:22:33");
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-xmpp -- sfu::sdp::tests`

Expected: All tests PASS.

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/sdp.rs
git commit -m "feat(sfu): implement standard Jingle XML <-> SDP conversion

Replaces the non-standard OOB SDP format with proper conversion using
XEP-0167 (RTP), XEP-0176 (ICE-UDP), and XEP-0320 (DTLS) types.
jingle_contents_to_sdp() converts incoming Jingle to SDP for str0m.
sdp_to_jingle_contents() converts str0m answers to standard Jingle XML."
```

---

## Task 2: Update SFU Service Actor to Use Standard Jingle Parsing

**Why:** The service actor currently calls the old `extract_sdp_from_jingle()` which expects OOB SDP. We switch it to use `extract_sdp_offer_from_jingle()` which parses standard Jingle XML via the XEP parsers. The response also needs to use the new `build_jingle_session_accept()` which returns a `Result`.

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/service_actor.rs`

- [ ] **Step 1: Update `handle_session_initiate`**

In `service_actor.rs`, replace the SDP extraction call:

```rust
// Before (line 47-53):
let sdp_offer = match sdp::extract_sdp_from_jingle(jingle) {
    Some(sdp) => sdp,
    None => {
        warn!(sid = %sid, "No SDP found in session-initiate Jingle element");
        return JingleIqResponse::Rejection {
            id: iq_id,
            reason: "Missing SDP offer in session-initiate".to_string(),
        };
    }
};

// After:
let sdp_offer = match sdp::extract_sdp_offer_from_jingle(jingle) {
    Ok(sdp) => sdp,
    Err(e) => {
        warn!(sid = %sid, error = %e, "Failed to convert Jingle to SDP");
        return JingleIqResponse::Rejection {
            id: iq_id,
            reason: format!("Invalid Jingle content: {e}"),
        };
    }
};
```

And update the session-accept building:

```rust
// Before (line 100):
let accept_element = sdp::build_jingle_session_accept(&sid, &answer_sdp);

// After:
let accept_element = match sdp::build_jingle_session_accept(&sid, &answer_sdp) {
    Ok(elem) => elem,
    Err(e) => {
        warn!(sid = %sid, error = %e, "Failed to build session-accept Jingle");
        return JingleIqResponse::Rejection {
            id: iq_id,
            reason: format!("Failed to build session-accept: {e}"),
        };
    }
};
```

- [ ] **Step 2: Run tests**

Run: `cd server && cargo test -p waddle-xmpp -- sfu`

Expected: All SFU tests pass. If the existing `service_actor` test doesn't test signaling flow, compilation is the key check.

- [ ] **Step 3: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/service_actor.rs
git commit -m "fix(sfu): use standard Jingle parsing instead of OOB SDP extraction"
```

---

## Task 3: Fix SID Collision in Client

**Why:** When User B joins User A's call, `joinMujiCall` reuses the invite's SID. The server's `peers.insert(sid, peer)` overwrites User A's entry. Each participant must have a unique SID. The `RoomKey` is derived from the `waddleId_channelId` prefix, so unique SIDs still map to the same room.

**Files:**
- Modify: `chat/src/lib/xmpp/client.ts`

- [ ] **Step 1: Always generate a unique SID in joinMujiCall**

In `client.ts`, the `joinMujiCall` method (around line 397-403):

```typescript
// Before:
const sid = invite.sid ?? `${w}_${c}_${crypto.randomUUID()}`;
const existing = sid ? this.mujiSessions.get(sid) : undefined;
if (existing && existing.state === "pending") {
  for (const track of localStream.getTracks()) {
    if (!existing.pc.getSenders().some((sender) => sender.track?.id === track.id)) {
      await existing.addTrack(track, localStream);
    }
  }
  await existing.accept();
  return { sid: existing.sid, serviceJid };
}

// After:
// Each participant gets a unique SID for their own session with the SFU.
// The room is identified by the waddleId_channelId prefix in the SID.
const sid = `${w}_${c}_${crypto.randomUUID()}`;
```

Remove the dead `existing` session check (lines 403-411) — with unique SIDs there can never be an existing session to accept.

- [ ] **Step 2: Commit**

```bash
git add chat/src/lib/xmpp/client.ts
git commit -m "fix(sfu): generate unique SID per participant to prevent peer overwrite"
```

---

## Task 4: Add Shared PeerStore to SfuRegistry

**Why:** The UDP net loop needs direct access to all peers for packet demuxing. Peers are currently private to `SfuRoomActor` Kameo actors. Adding a shared `PeerStore` gives the net loop read/write access without per-packet message-passing overhead.

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/mod.rs`
- Modify: `server/crates/waddle-xmpp/src/sfu/peer.rs`
- Modify: `server/crates/waddle-xmpp/src/sfu/room_actor.rs`

- [ ] **Step 1: Add `room_key` to SfuPeer**

In `peer.rs`, add `use super::RoomKey;` and add the field:

```rust
pub struct SfuPeer {
    pub jid: Option<FullJid>,
    pub sid: String,
    pub room_key: RoomKey,
    rtc: Rtc,
    local_addr: SocketAddr,
}
```

Update `new_from_offer` to accept `room_key: RoomKey` and store it. Update the test constructors to pass `room_key: RoomKey("test".to_string())`.

- [ ] **Step 2: Add PeerStore to mod.rs**

In `mod.rs`, add after the `SfuRegistry` struct:

```rust
use peer::SfuPeer;

/// Shared store of all active SFU peers, keyed by Jingle SID.
/// Accessible by both signaling actors and the UDP net loop.
#[derive(Debug, Default)]
pub struct PeerStore {
    peers: RwLock<HashMap<String, SfuPeer>>,
}

impl PeerStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, sid: String, peer: SfuPeer) {
        self.peers.write().await.insert(sid, peer);
    }

    pub async fn remove(&self, sid: &str) -> Option<SfuPeer> {
        self.peers.write().await.remove(sid)
    }

    /// Mutable access to all peers — used by the net loop.
    pub fn peers(&self) -> &RwLock<HashMap<String, SfuPeer>> {
        &self.peers
    }

    pub async fn peer_count_in_room(&self, room_key: &RoomKey) -> usize {
        self.peers
            .read()
            .await
            .values()
            .filter(|peer| peer.room_key == *room_key)
            .count()
    }
}
```

Add `peer_store: Arc<PeerStore>` to `SfuRegistry`:

```rust
#[derive(Debug, Default)]
pub struct SfuRegistry {
    rooms: RwLock<HashMap<RoomKey, kameo::actor::ActorRef<room_actor::SfuRoomActor>>>,
    pub peer_store: Arc<PeerStore>,
}

impl SfuRegistry {
    pub fn new() -> Self {
        Self {
            rooms: RwLock::new(HashMap::new()),
            peer_store: Arc::new(PeerStore::new()),
        }
    }
    // ... existing methods unchanged ...
}
```

- [ ] **Step 3: Update room_actor to use PeerStore**

Replace the private `peers: HashMap<String, SfuPeer>` with `peer_store: Arc<PeerStore>`:

```rust
#[derive(Actor)]
pub struct SfuRoomActor {
    pub(crate) room_key: RoomKey,
    pub(crate) peer_store: Arc<PeerStore>,
    pub(crate) local_addr: SocketAddr,
}

impl SfuRoomActor {
    pub fn new(room_key: RoomKey, local_addr: SocketAddr, peer_store: Arc<PeerStore>) -> Self {
        Self { room_key, peer_store, local_addr }
    }
}
```

Update `AddParticipant` handler to use `self.peer_store.insert()` and `RemoveParticipant` to use `self.peer_store.remove()`. Update `GetParticipantCount` to use `self.peer_store.peer_count_in_room()`.

- [ ] **Step 4: Update service_actor.rs room creation**

```rust
// Before:
let actor_ref = kameo::spawn(SfuRoomActor::new(room_key.clone(), self.udp_addr));

// After:
let actor_ref = kameo::spawn(SfuRoomActor::new(
    room_key.clone(),
    self.udp_addr,
    Arc::clone(&self.registry.peer_store),
));
```

- [ ] **Step 5: Run tests**

Run: `cd server && cargo test -p waddle-xmpp -- sfu`

Fix compilation errors. Update test constructors for new signatures.

- [ ] **Step 6: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/
git commit -m "refactor(sfu): add shared PeerStore for net loop access to all peers"
```

---

## Task 5: Implement UDP Media Forwarding Loop

**Why:** The net loop currently drops all received packets. Without forwarding them to str0m `Rtc` instances, ICE checks fail and no media flows.

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/net.rs`
- Modify: `server/crates/waddle-xmpp/src/server.rs`

- [ ] **Step 1: Implement the forwarding loop**

Replace the contents of `net.rs`:

```rust
//! SFU UDP socket management and media forwarding loop.

use super::PeerStore;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::{Event, Input, Output};
use tokio::net::UdpSocket;
use tracing::{debug, info, trace, warn};

/// Spawn the SFU UDP event loop as a background tokio task.
///
/// This loop:
/// 1. Reads incoming UDP packets from the shared socket
/// 2. Demuxes them to the correct SfuPeer (via `accepts()`)
/// 3. Polls each peer for outputs (outgoing packets, events)
pub async fn spawn_sfu_net_loop(
    udp_addr: SocketAddr,
    peer_store: Arc<PeerStore>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), std::io::Error> {
    let socket = UdpSocket::bind(udp_addr).await?;
    info!(addr = %udp_addr, "SFU UDP socket bound");

    let mut buf = vec![0u8; 2000];
    let mut poll_interval = tokio::time::interval(Duration::from_millis(20));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("SFU net loop shutting down");
                break;
            }
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, src)) => {
                        trace!(src = %src, len = len, "SFU UDP packet received");
                        let now = Instant::now();

                        let mut peers = peer_store.peers().write().await;
                        for peer in peers.values_mut() {
                            let input = Input::Receive(
                                now,
                                str0m::net::Receive {
                                    source: src,
                                    destination: udp_addr,
                                    contents: (&buf[..len]).try_into()
                                        .expect("valid datagram"),
                                },
                            );
                            if peer.accepts(&input) {
                                if let Err(e) = peer.handle_input(input) {
                                    warn!(sid = %peer.sid, error = %e, "input error");
                                }
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "SFU UDP recv error");
                    }
                }
            }
            _ = poll_interval.tick() => {
                let now = Instant::now();
                let mut peers = peer_store.peers().write().await;
                let sids: Vec<String> = peers.keys().cloned().collect();

                for sid in &sids {
                    let peer = match peers.get_mut(sid) {
                        Some(p) => p,
                        None => continue,
                    };

                    if let Err(e) = peer.handle_input(Input::Timeout(now)) {
                        warn!(sid = %sid, error = %e, "timeout input error");
                        continue;
                    }

                    loop {
                        match peer.poll_output() {
                            Ok(Output::Transmit(transmit)) => {
                                if let Err(e) = socket.try_send_to(
                                    &transmit.contents,
                                    transmit.destination,
                                ) {
                                    trace!(error = %e, "UDP send error");
                                }
                            }
                            Ok(Output::Event(event)) => {
                                match &event {
                                    Event::Connected => {
                                        info!(sid = %sid, "Peer connected");
                                    }
                                    Event::IceConnectionStateChange(state) => {
                                        debug!(sid = %sid, state = ?state, "ICE state");
                                    }
                                    _ => {
                                        trace!(sid = %sid, event = ?event, "str0m event");
                                    }
                                }
                            }
                            Ok(Output::Timeout(_)) => break,
                            Err(e) => {
                                warn!(sid = %sid, error = %e, "poll_output error");
                                break;
                            }
                        }
                    }
                }

                // Remove dead peers
                peers.retain(|sid, peer| {
                    if !peer.is_alive() {
                        info!(sid = %sid, "Removing dead peer");
                        false
                    } else {
                        true
                    }
                });
            }
        }
    }

    Ok(())
}
```

**Note on `Input::Receive` construction:** The exact API depends on str0m 0.18. The `str0m::net::Receive` struct and its `contents` field type may vary. Check `str0m` docs for the correct `DatagramRecv` construction. If `(&buf[..len]).try_into()` doesn't compile, use `str0m::net::DatagramRecv::new(buf[..len].to_vec())` or similar.

- [ ] **Step 2: Update server.rs to pass PeerStore**

In `server.rs`, update the net loop spawn:

```rust
// Before:
let sfu_registry_clone = Arc::clone(&sfu_registry);
// ...
crate::sfu::net::spawn_sfu_net_loop(sfu_udp_addr_clone, sfu_registry_clone, sfu_shutdown)

// After:
let sfu_peer_store = Arc::clone(&sfu_registry.peer_store);
// ...
crate::sfu::net::spawn_sfu_net_loop(sfu_udp_addr_clone, sfu_peer_store, sfu_shutdown)
```

- [ ] **Step 3: Run compilation check**

Run: `cd server && cargo check -p waddle-xmpp`

Fix any str0m API differences. The key types to verify:
- `str0m::net::Receive` struct fields
- `str0m::Input::Receive` constructor
- `str0m::Output` variants

- [ ] **Step 4: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/net.rs server/crates/waddle-xmpp/src/server.rs
git commit -m "feat(sfu): implement UDP packet demux and str0m polling loop

Receives UDP packets, routes to correct str0m Rtc via accepts(),
polls peers for outgoing transmits. Enables ICE/DTLS handshake."
```

---

## Task 6: Integration Verification

**Files:** All from Tasks 1-5.

- [ ] **Step 1: Verify server compiles cleanly**

Run: `cd server && cargo build -p waddle-xmpp 2>&1 | head -30`

- [ ] **Step 2: Run all server tests**

Run: `cd server && cargo test -p waddle-xmpp 2>&1 | tail -30`

- [ ] **Step 3: Verify frontend compiles**

Run: `cd chat && bun run build 2>&1 | head -30`

- [ ] **Step 4: Run frontend tests**

Run: `cd chat && bun test 2>&1 | tail -20`

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix(sfu): resolve compilation issues from Jingle conversion refactor"
```

---

## Known Limitations (Post-Fix)

1. **Media forwarding between peers** — The net loop establishes ICE/DTLS connections per peer, but actual RTP forwarding between peers in the same room (reading `Event::RtpPacket` from one peer and writing to others) requires additional str0m API usage. Without this, peers connect to the SFU but see black frames instead of each other's video. This is the next step.

2. **Missing Jingle attributes** — The XEP-0167 parser doesn't support `<rtcp-fb>`, `<rtp-hdrext>`, `<source>`, or `<ssrc-group>`. Stanza.js sends these, but they'll be dropped during Jingle→SDP conversion. str0m should tolerate the minimal SDP, but codec negotiation may be sub-optimal.

3. **Participant map notifications** — `build_participant_map()` is ready but not called. After media forwarding works, the SFU should send `session-info` IQs so clients know who joined.

4. **ICE candidate trickling** — `transport-info` handler is a no-op. Vanilla ICE (all candidates in initial SDP) works for now.
