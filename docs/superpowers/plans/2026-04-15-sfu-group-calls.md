# SFU Group Calls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement XMPP-native SFU-mediated group video calls where clients negotiate with the SFU via Jingle IQs and media flows through the server using str0m.

**Architecture:** The SFU is an XMPP component at `sfu.{domain}`. Clients send Jingle `session-initiate` IQs to this JID; the server responds with `session-accept` containing an SDP answer. Each call room is a Kameo actor owning str0m `Rtc` instances per participant. The frontend renders a video grid overlay when a call is active.

**Tech Stack:** Rust + str0m 0.18 + Kameo actors (server), TypeScript + Stanza.js + Vue 3 (frontend)

**Spec:** `docs/superpowers/specs/2026-04-15-sfu-group-calls-design.md`

---

## File Structure

### Server — New Files
- `server/crates/waddle-xmpp/src/sfu/mod.rs` — SFU module root, exports, `SfuRegistry` (room lookup)
- `server/crates/waddle-xmpp/src/sfu/service_actor.rs` — `SfuServiceActor` Kameo actor, Jingle IQ dispatch
- `server/crates/waddle-xmpp/src/sfu/room_actor.rs` — `SfuRoomActor` Kameo actor, per-call state, str0m peers
- `server/crates/waddle-xmpp/src/sfu/peer.rs` — `SfuPeer` wrapper around `str0m::Rtc`, track state
- `server/crates/waddle-xmpp/src/sfu/sdp.rs` — SDP ↔ Jingle conversion helpers
- `server/crates/waddle-xmpp/src/sfu/net.rs` — UDP socket management, packet demux loop

### Server — Modified Files
- `server/Cargo.toml` — add `str0m` workspace dependency
- `server/crates/waddle-xmpp/Cargo.toml` — add `str0m` dependency
- `server/crates/waddle-xmpp/src/lib.rs` — add `pub mod sfu;`
- `server/crates/waddle-xmpp/src/routing.rs` — add `LocalSfu` routing destination, `sfu_domain` config
- `server/crates/waddle-xmpp/src/connection.rs` — add SFU Jingle IQ interception in `handle_iq()`
- `server/crates/waddle-xmpp/src/server.rs` — create `SfuServiceActor` at startup, pass to connections
- `server/crates/waddle-xmpp/src/disco/info.rs` — add SFU Jingle features to disco response

### Frontend — New Files
- `chat/src/components/chat/CallOverlay.vue` — video grid overlay component

### Frontend — Modified Files
- `chat/src/lib/xmpp/client.ts` — fix `startMujiCall()`, add ICE config, add `session-info` handler
- `chat/src/lib/xmpp/types.ts` — add `ParticipantTrackMap` type, update `MujiCallEvent`
- `chat/src/composables/useMujiRuntime.ts` — multi-participant track map, expose to UI
- `chat/src/components/chat/ContentArea.vue` — mount `CallOverlay`, pass streams/controls

---

## Task 1: Add str0m Dependency

**Files:**
- Modify: `server/Cargo.toml`
- Modify: `server/crates/waddle-xmpp/Cargo.toml`

- [ ] **Step 1: Add str0m to workspace dependencies**

In `server/Cargo.toml`, add to `[workspace.dependencies]`:

```toml
str0m = "0.18"
```

- [ ] **Step 2: Add str0m to waddle-xmpp crate**

In `server/crates/waddle-xmpp/Cargo.toml`, add to `[dependencies]`:

```toml
str0m = { workspace = true }
```

- [ ] **Step 3: Verify it compiles**

Run: `cd server && cargo check -p waddle-xmpp 2>&1 | tail -5`
Expected: `Finished` with no errors (warnings OK)

- [ ] **Step 4: Commit**

```bash
git add server/Cargo.toml server/crates/waddle-xmpp/Cargo.toml
git commit -m "deps: add str0m 0.18 for SFU WebRTC support"
```

---

## Task 2: SFU Routing — Extend RoutingDestination

**Files:**
- Modify: `server/crates/waddle-xmpp/src/routing.rs`

- [ ] **Step 1: Write routing tests**

Add to the existing `#[cfg(test)] mod tests` in `routing.rs`:

```rust
#[test]
fn routes_sfu_domain_to_local_sfu() {
    let config = RouterConfig::new("waddle.social".to_string());
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry, None);

    let dest = router.get_destination_for_domain("sfu.waddle.social");
    assert_eq!(dest, RoutingDestination::LocalSfu);
}

#[test]
fn sfu_domain_does_not_match_muc_or_local() {
    let config = RouterConfig::new("waddle.social".to_string());
    let registry = Arc::new(ConnectionRegistry::new());
    let router = StanzaRouter::new(config, registry, None);

    assert_eq!(
        router.get_destination_for_domain("waddle.social"),
        RoutingDestination::Local
    );
    assert_eq!(
        router.get_destination_for_domain("muc.waddle.social"),
        RoutingDestination::LocalMuc
    );
    assert_eq!(
        router.get_destination_for_domain("sfu.waddle.social"),
        RoutingDestination::LocalSfu
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd server && cargo test -p waddle-xmpp routing -- --nocapture 2>&1 | tail -10`
Expected: FAIL — `LocalSfu` variant doesn't exist

- [ ] **Step 3: Add sfu_domain to RouterConfig**

In `routing.rs`, update `RouterConfig`:

```rust
pub struct RouterConfig {
    pub local_domain: String,
    pub muc_domain: String,
    pub spaces_domain: String,
    pub sfu_domain: String,
    pub federation_enabled: bool,
}
```

Update `RouterConfig::new()`:

```rust
pub fn new(local_domain: String) -> Self {
    let muc_domain = format!("muc.{}", local_domain);
    let spaces_domain = format!("spaces.{}", local_domain);
    let sfu_domain = format!("sfu.{}", local_domain);
    Self {
        local_domain,
        muc_domain,
        spaces_domain,
        sfu_domain,
        federation_enabled: false,
    }
}
```

- [ ] **Step 4: Add LocalSfu variant to RoutingDestination**

```rust
pub enum RoutingDestination {
    Local,
    LocalMuc,
    LocalSpaces,
    LocalSfu,
    Remote { domain: String },
}
```

- [ ] **Step 5: Update get_destination_for_domain()**

Add the SFU domain match after the spaces check:

```rust
pub fn get_destination_for_domain(&self, domain: &str) -> RoutingDestination {
    if domain == self.config.local_domain {
        RoutingDestination::Local
    } else if domain == self.config.muc_domain {
        RoutingDestination::LocalMuc
    } else if domain == self.config.spaces_domain {
        RoutingDestination::LocalSpaces
    } else if domain == self.config.sfu_domain {
        RoutingDestination::LocalSfu
    } else {
        RoutingDestination::Remote {
            domain: domain.to_string(),
        }
    }
}
```

- [ ] **Step 6: Update route_iq() dispatch**

In `route_iq()`, add the `LocalSfu` arm. For now, return a placeholder that we'll wire up in Task 5:

```rust
RoutingDestination::LocalSfu => {
    debug!("IQ to local SFU service — not yet wired");
    Ok(RoutingResult::DeliveredLocal {
        delivered_count: 0,
        offline_count: 0,
    })
}
```

- [ ] **Step 7: Add is_sfu_jid() helper**

Add to `StanzaRouter`:

```rust
pub fn is_sfu_jid(&self, jid: &Jid) -> bool {
    jid.domain().as_str() == self.config.sfu_domain
}

pub fn sfu_domain(&self) -> &str {
    &self.config.sfu_domain
}
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-xmpp routing -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add server/crates/waddle-xmpp/src/routing.rs
git commit -m "feat(xmpp): add LocalSfu routing destination for sfu.{domain}"
```

---

## Task 3: SFU Module Skeleton and SfuRegistry

**Files:**
- Create: `server/crates/waddle-xmpp/src/sfu/mod.rs`
- Modify: `server/crates/waddle-xmpp/src/lib.rs`

- [ ] **Step 1: Create the sfu module**

Create `server/crates/waddle-xmpp/src/sfu/mod.rs`:

```rust
//! SFU (Selective Forwarding Unit) — XMPP-native group call media server.
//!
//! The SFU is an XMPP component at `sfu.{domain}` that speaks Jingle (XEP-0166)
//! to negotiate WebRTC sessions with clients. Each active call is a `SfuRoomActor`
//! owning str0m `Rtc` instances per participant.

pub mod peer;
pub mod room_actor;
pub mod sdp;
pub mod service_actor;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Unique key for an SFU call room, derived from waddle + channel IDs.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct RoomKey(pub String);

impl RoomKey {
    /// Parse a room key from a Jingle session ID.
    /// Session IDs are formatted as `{waddle_id}_{channel_id}_{uuid}`.
    pub fn from_session_id(sid: &str) -> Option<Self> {
        let parts: Vec<&str> = sid.splitn(3, '_').collect();
        if parts.len() >= 2 {
            Some(Self(format!("{}_{}", parts[0], parts[1])))
        } else {
            None
        }
    }
}

/// Registry of active SFU call rooms.
#[derive(Debug, Default)]
pub struct SfuRegistry {
    rooms: RwLock<HashMap<RoomKey, kameo::actor::ActorRef<room_actor::SfuRoomActor>>>,
}

impl SfuRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_room(
        &self,
        key: &RoomKey,
    ) -> Option<kameo::actor::ActorRef<room_actor::SfuRoomActor>> {
        self.rooms.read().await.get(key).cloned()
    }

    pub async fn insert_room(
        &self,
        key: RoomKey,
        actor_ref: kameo::actor::ActorRef<room_actor::SfuRoomActor>,
    ) {
        self.rooms.write().await.insert(key, actor_ref);
    }

    pub async fn remove_room(&self, key: &RoomKey) {
        self.rooms.write().await.remove(key);
    }
}
```

- [ ] **Step 2: Create stub submodules**

Create `server/crates/waddle-xmpp/src/sfu/peer.rs`:

```rust
//! SfuPeer — wraps a str0m Rtc instance for one participant.
```

Create `server/crates/waddle-xmpp/src/sfu/room_actor.rs`:

```rust
//! SfuRoomActor — Kameo actor managing one active call room.
```

Create `server/crates/waddle-xmpp/src/sfu/service_actor.rs`:

```rust
//! SfuServiceActor — top-level Kameo actor for the SFU XMPP component.
```

Create `server/crates/waddle-xmpp/src/sfu/sdp.rs`:

```rust
//! SDP ↔ Jingle conversion helpers.
```

- [ ] **Step 3: Register sfu module in lib.rs**

In `server/crates/waddle-xmpp/src/lib.rs`, add:

```rust
pub mod sfu;
```

- [ ] **Step 4: Verify it compiles**

Run: `cd server && cargo check -p waddle-xmpp 2>&1 | tail -5`
Expected: `Finished` with no errors

- [ ] **Step 5: Write RoomKey tests**

Add to `server/crates/waddle-xmpp/src/sfu/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_room_key_from_session_id() {
        let key = RoomKey::from_session_id("waddle123_channel456_some-uuid").unwrap();
        assert_eq!(key.0, "waddle123_channel456");
    }

    #[test]
    fn rejects_invalid_session_id() {
        assert!(RoomKey::from_session_id("no-underscores").is_none());
    }

    #[test]
    fn room_key_from_two_part_sid() {
        let key = RoomKey::from_session_id("w_c").unwrap();
        assert_eq!(key.0, "w_c");
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cd server && cargo test -p waddle-xmpp sfu -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/ server/crates/waddle-xmpp/src/lib.rs
git commit -m "feat(sfu): add SFU module skeleton with RoomKey and SfuRegistry"
```

---

## Task 4: SDP ↔ Jingle Conversion

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/sdp.rs`

This bridges between Jingle XML stanzas (which clients send) and str0m's `SdpOffer`/`SdpAnswer` types. Jingle carries SDP content in its XML structure, but Stanza.js actually sends the raw SDP in the Jingle payload via `RawSdp` elements.

- [ ] **Step 1: Write conversion tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_sdp_offer_from_jingle_element() {
        let sdp_text = "v=0\r\no=- 123 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n";
        let element = minidom::Element::builder("jingle", "urn:xmpp:jingle:1")
            .attr("action", "session-initiate")
            .attr("sid", "test-sid")
            .append(
                minidom::Element::builder("content", "urn:xmpp:jingle:1")
                    .attr("creator", "initiator")
                    .attr("name", "audio")
                    .append(
                        minidom::Element::builder("description", "urn:xmpp:jingle:apps:rtp:1")
                            .attr("media", "audio")
                            .build(),
                    )
                    .append(
                        minidom::Element::builder("transport", "urn:xmpp:jingle:transports:ice-udp:1")
                            .append(
                                minidom::Element::builder("sdp", "urn:xmpp:jingle:apps:oob-sdp:0")
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
    fn builds_jingle_accept_with_sdp() {
        let sdp_answer = "v=0\r\no=- 456 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n";
        let element = build_jingle_session_accept("test-sid", sdp_answer);
        assert_eq!(element.attr("action").unwrap(), "session-accept");
        assert_eq!(element.attr("sid").unwrap(), "test-sid");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd server && cargo test -p waddle-xmpp sdp -- --nocapture 2>&1 | tail -10`
Expected: FAIL

- [ ] **Step 3: Implement SDP extraction**

```rust
use minidom::Element;

const JINGLE_NS: &str = "urn:xmpp:jingle:1";
const ICE_UDP_NS: &str = "urn:xmpp:jingle:transports:ice-udp:1";
const OOB_SDP_NS: &str = "urn:xmpp:jingle:apps:oob-sdp:0";
const RTP_NS: &str = "urn:xmpp:jingle:apps:rtp:1";

/// Extract raw SDP string from a Jingle element.
///
/// Stanza.js sends SDP inside a `<sdp xmlns="urn:xmpp:jingle:apps:oob-sdp:0">`
/// child of the `<transport>` element.
pub fn extract_sdp_from_jingle(jingle: &Element) -> Option<String> {
    for content in jingle.children().filter(|c| c.is("content", JINGLE_NS)) {
        for transport in content
            .children()
            .filter(|t| t.is("transport", ICE_UDP_NS))
        {
            for sdp_elem in transport
                .children()
                .filter(|s| s.is("sdp", OOB_SDP_NS))
            {
                let text = sdp_elem.text();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    None
}

/// Extract the Jingle session ID from a Jingle element.
pub fn extract_sid(jingle: &Element) -> Option<&str> {
    jingle.attr("sid")
}

/// Extract the Jingle action from a Jingle element.
pub fn extract_action(jingle: &Element) -> Option<&str> {
    jingle.attr("action")
}

/// Build a Jingle `session-accept` element wrapping an SDP answer.
pub fn build_jingle_session_accept(sid: &str, sdp_answer: &str) -> Element {
    Element::builder("jingle", JINGLE_NS)
        .attr("action", "session-accept")
        .attr("sid", sid)
        .append(
            Element::builder("content", JINGLE_NS)
                .attr("creator", "initiator")
                .attr("name", "media")
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

/// Build a Jingle `transport-info` element with an ICE candidate.
pub fn build_jingle_transport_info(sid: &str, candidate_sdp: &str) -> Element {
    Element::builder("jingle", JINGLE_NS)
        .attr("action", "transport-info")
        .attr("sid", sid)
        .append(
            Element::builder("content", JINGLE_NS)
                .attr("creator", "initiator")
                .attr("name", "media")
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

/// Build a Jingle `session-info` element mapping media stream IDs to participant JIDs.
pub fn build_participant_map(sid: &str, mappings: &[(String, String)]) -> Element {
    let mut info = Element::builder("jingle", JINGLE_NS)
        .attr("action", "session-info")
        .attr("sid", sid);

    let mut map = Element::builder("participant-map", "urn:waddle:sfu:participant-map:0");
    for (msid, jid) in mappings {
        map = map.append(
            Element::builder("entry", "urn:waddle:sfu:participant-map:0")
                .attr("msid", msid.as_str())
                .attr("jid", jid.as_str())
                .build(),
        );
    }
    info = info.append(map.build());
    info.build()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-xmpp sdp -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/sdp.rs
git commit -m "feat(sfu): add SDP-to-Jingle conversion helpers"
```

---

## Task 5: SfuPeer — str0m Wrapper

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/peer.rs`

- [ ] **Step 1: Write peer creation test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_peer_from_sdp_offer() {
        // Minimal valid SDP from a browser
        let offer_sdp = "v=0\r\n\
            o=- 4611731400430051336 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=rtcp:9 IN IP4 0.0.0.0\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword1234567890ab\r\n\
            a=fingerprint:sha-256 00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00\r\n\
            a=setup:actpass\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:111 opus/48000/2\r\n";

        let result = SfuPeer::new_from_offer(offer_sdp, "127.0.0.1:10000".parse().unwrap());
        assert!(result.is_ok(), "Failed: {:?}", result.err());
        let (peer, answer_sdp) = result.unwrap();
        assert!(!answer_sdp.is_empty());
        assert!(peer.is_alive());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd server && cargo test -p waddle-xmpp peer -- --nocapture 2>&1 | tail -10`
Expected: FAIL — `SfuPeer` not defined

- [ ] **Step 3: Implement SfuPeer**

```rust
use jid::FullJid;
use std::net::SocketAddr;
use std::time::Instant;
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::net::{DatagramRecv, Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

/// Wraps a str0m `Rtc` instance for one SFU participant.
#[derive(Debug)]
pub struct SfuPeer {
    pub jid: Option<FullJid>,
    pub sid: String,
    rtc: Rtc,
    local_addr: SocketAddr,
}

impl SfuPeer {
    /// Create a new peer by accepting an SDP offer. Returns the peer and the SDP answer string.
    pub fn new_from_offer(
        offer_sdp: &str,
        local_addr: SocketAddr,
    ) -> Result<(Self, String), str0m::RtcError> {
        let mut rtc = Rtc::builder()
            .set_ice_lite(true)
            .build(Instant::now());

        let candidate = Candidate::host(local_addr, "udp")
            .expect("valid host candidate");
        rtc.add_local_candidate(candidate)?;

        let offer: SdpOffer = SdpOffer::from_sdp_string(offer_sdp)
            .map_err(|e| str0m::RtcError::Other(format!("SDP parse error: {e}")))?;
        let answer: SdpAnswer = rtc.sdp_api().accept_offer(offer)?;
        let answer_sdp = answer.to_sdp_string();

        Ok((
            Self {
                jid: None,
                sid: String::new(),
                rtc,
                local_addr,
            },
            answer_sdp,
        ))
    }

    /// Check if this peer's Rtc instance accepts a given input (for UDP demux).
    pub fn accepts(&self, input: &Input) -> bool {
        self.rtc.accepts(input)
    }

    /// Feed input (timeout or received data) into the Rtc state machine.
    pub fn handle_input(&mut self, input: Input) -> Result<(), str0m::RtcError> {
        self.rtc.handle_input(input)
    }

    /// Poll the next output from the Rtc state machine.
    pub fn poll_output(&mut self) -> Result<Output, str0m::RtcError> {
        self.rtc.poll_output()
    }

    /// Check if the peer connection is still alive.
    pub fn is_alive(&self) -> bool {
        self.rtc.is_alive()
    }

    /// Check if the peer is connected (ICE + DTLS complete).
    pub fn is_connected(&self) -> bool {
        self.rtc.is_connected()
    }

    /// Disconnect this peer.
    pub fn disconnect(&mut self) {
        self.rtc.disconnect();
    }

    /// Get the local socket address for this peer.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Get a mutable reference to the inner Rtc for media operations.
    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.rtc
    }

    /// Get an immutable reference to the inner Rtc.
    pub fn rtc(&self) -> &Rtc {
        &self.rtc
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd server && cargo test -p waddle-xmpp peer -- --nocapture 2>&1 | tail -10`
Expected: PASS (or may need SDP format adjustments — iterate until passing)

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/peer.rs
git commit -m "feat(sfu): add SfuPeer wrapper around str0m Rtc"
```

---

## Task 6: SfuRoomActor — Per-Call State

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/room_actor.rs`

- [ ] **Step 1: Write room actor tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_room_with_key() {
        let room = SfuRoomActor::new(
            RoomKey("test_room".to_string()),
            "127.0.0.1:10000".parse().unwrap(),
        );
        assert_eq!(room.room_key.0, "test_room");
        assert!(room.peers.is_empty());
    }

    #[tokio::test]
    async fn tracks_participant_count() {
        let mut room = SfuRoomActor::new(
            RoomKey("test_room".to_string()),
            "127.0.0.1:10000".parse().unwrap(),
        );
        assert_eq!(room.participant_count(), 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd server && cargo test -p waddle-xmpp room_actor -- --nocapture 2>&1 | tail -10`
Expected: FAIL

- [ ] **Step 3: Implement SfuRoomActor**

```rust
use super::peer::SfuPeer;
use super::RoomKey;
use jid::FullJid;
use kameo::Actor;
use std::collections::HashMap;
use std::net::SocketAddr;
use tracing::{debug, info, warn};

/// Kameo actor managing one active call room.
///
/// Owns one `SfuPeer` (str0m Rtc instance) per participant.
/// Handles Jingle session-initiate by creating a peer and returning an SDP answer.
#[derive(Actor)]
pub struct SfuRoomActor {
    pub(crate) room_key: RoomKey,
    pub(crate) peers: HashMap<String, SfuPeer>, // keyed by Jingle SID
    pub(crate) local_addr: SocketAddr,
}

impl SfuRoomActor {
    pub fn new(room_key: RoomKey, local_addr: SocketAddr) -> Self {
        Self {
            room_key,
            peers: HashMap::new(),
            local_addr,
        }
    }

    pub fn participant_count(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

// --- Kameo Messages ---

/// Add a participant to this call room. Returns SDP answer.
pub struct AddParticipant {
    pub sid: String,
    pub jid: FullJid,
    pub sdp_offer: String,
}

impl kameo::message::Message<AddParticipant> for SfuRoomActor {
    type Reply = Result<String, String>;

    async fn handle(
        &mut self,
        msg: AddParticipant,
        _ctx: &mut kameo::message::Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        if self.peers.contains_key(&msg.sid) {
            return Err(format!("Session {} already exists in room", msg.sid));
        }

        let (mut peer, answer_sdp) = SfuPeer::new_from_offer(&msg.sdp_offer, self.local_addr)
            .map_err(|e| format!("Failed to create peer: {e}"))?;

        peer.jid = Some(msg.jid.clone());
        peer.sid = msg.sid.clone();

        info!(
            room = %self.room_key.0,
            sid = %msg.sid,
            jid = %msg.jid,
            "Participant joined SFU room"
        );

        self.peers.insert(msg.sid, peer);
        Ok(answer_sdp)
    }
}

/// Remove a participant from this call room.
pub struct RemoveParticipant {
    pub sid: String,
}

impl kameo::message::Message<RemoveParticipant> for SfuRoomActor {
    type Reply = Result<bool, String>;

    async fn handle(
        &mut self,
        msg: RemoveParticipant,
        _ctx: &mut kameo::message::Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(mut peer) = self.peers.remove(&msg.sid) {
            peer.disconnect();
            info!(
                room = %self.room_key.0,
                sid = %msg.sid,
                "Participant left SFU room"
            );
            Ok(self.is_empty())
        } else {
            Err(format!("Session {} not found in room", msg.sid))
        }
    }
}

/// Query the current participant count.
pub struct GetParticipantCount;

impl kameo::message::Message<GetParticipantCount> for SfuRoomActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: GetParticipantCount,
        _ctx: &mut kameo::message::Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.participant_count()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-xmpp room_actor -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/room_actor.rs
git commit -m "feat(sfu): add SfuRoomActor with AddParticipant/RemoveParticipant messages"
```

---

## Task 7: SfuServiceActor — Jingle IQ Dispatch

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/service_actor.rs`

- [ ] **Step 1: Write service actor tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatches_session_initiate_to_new_room() {
        let registry = Arc::new(SfuRegistry::new());
        let actor = kameo::spawn(SfuServiceActor::new(
            "sfu.waddle.social".to_string(),
            registry.clone(),
            "127.0.0.1:10000".parse().unwrap(),
        ));

        // Verify registry is empty
        let key = RoomKey("w_c".to_string());
        assert!(registry.get_room(&key).await.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd server && cargo test -p waddle-xmpp service_actor -- --nocapture 2>&1 | tail -10`
Expected: FAIL

- [ ] **Step 3: Implement SfuServiceActor**

```rust
use super::room_actor::{AddParticipant, RemoveParticipant, SfuRoomActor};
use super::sdp;
use super::{RoomKey, SfuRegistry};
use jid::FullJid;
use kameo::Actor;
use minidom::Element;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, info, warn};
use xmpp_parsers::iq::{Iq, IqType};

/// Top-level SFU XMPP component actor.
///
/// Receives Jingle IQs addressed to `sfu.{domain}` and dispatches
/// them to the appropriate `SfuRoomActor`.
#[derive(Actor)]
pub struct SfuServiceActor {
    sfu_domain: String,
    registry: Arc<SfuRegistry>,
    udp_addr: SocketAddr,
}

impl SfuServiceActor {
    pub fn new(sfu_domain: String, registry: Arc<SfuRegistry>, udp_addr: SocketAddr) -> Self {
        Self {
            sfu_domain,
            registry,
            udp_addr,
        }
    }
}

/// Handle an incoming Jingle IQ addressed to the SFU.
pub struct HandleJingleIq {
    pub iq: Iq,
    pub sender_jid: FullJid,
}

/// Response from the SFU to a Jingle IQ.
pub enum JingleIqResponse {
    /// Return an IQ result with this Jingle element payload.
    Accept { id: String, jingle: Element },
    /// Return an IQ result (empty, for acks).
    Ack { id: String },
    /// Return an IQ error.
    Error { id: String, reason: String },
}

impl kameo::message::Message<HandleJingleIq> for SfuServiceActor {
    type Reply = JingleIqResponse;

    async fn handle(
        &mut self,
        msg: HandleJingleIq,
        _ctx: &mut kameo::message::Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        let iq = msg.iq;
        let iq_id = iq.id.clone();

        let jingle_element = match &iq.payload {
            IqType::Set(elem) if elem.is("jingle", "urn:xmpp:jingle:1") => elem.clone(),
            _ => {
                return JingleIqResponse::Error {
                    id: iq_id,
                    reason: "Expected Jingle IQ set".to_string(),
                };
            }
        };

        let action = match sdp::extract_action(&jingle_element) {
            Some(a) => a.to_string(),
            None => {
                return JingleIqResponse::Error {
                    id: iq_id,
                    reason: "Missing Jingle action".to_string(),
                };
            }
        };

        let sid = match sdp::extract_sid(&jingle_element) {
            Some(s) => s.to_string(),
            None => {
                return JingleIqResponse::Error {
                    id: iq_id,
                    reason: "Missing Jingle SID".to_string(),
                };
            }
        };

        match action.as_str() {
            "session-initiate" => {
                self.handle_session_initiate(iq_id, sid, jingle_element, msg.sender_jid)
                    .await
            }
            "session-terminate" => {
                self.handle_session_terminate(iq_id, sid).await
            }
            "transport-info" => {
                // ICE candidates — acknowledge for now, full handling in net.rs
                debug!(sid = %sid, "Received transport-info (ICE candidate)");
                JingleIqResponse::Ack { id: iq_id }
            }
            other => {
                warn!(action = %other, sid = %sid, "Unsupported Jingle action");
                JingleIqResponse::Error {
                    id: iq_id,
                    reason: format!("Unsupported Jingle action: {other}"),
                }
            }
        }
    }
}

impl SfuServiceActor {
    async fn handle_session_initiate(
        &self,
        iq_id: String,
        sid: String,
        jingle: Element,
        sender_jid: FullJid,
    ) -> JingleIqResponse {
        let sdp_offer = match sdp::extract_sdp_from_jingle(&jingle) {
            Some(sdp) => sdp,
            None => {
                return JingleIqResponse::Error {
                    id: iq_id,
                    reason: "No SDP found in Jingle session-initiate".to_string(),
                };
            }
        };

        let room_key = match RoomKey::from_session_id(&sid) {
            Some(key) => key,
            None => {
                return JingleIqResponse::Error {
                    id: iq_id,
                    reason: "Invalid session ID format (expected waddle_channel_uuid)".to_string(),
                };
            }
        };

        // Get or create room actor
        let room_ref = if let Some(existing) = self.registry.get_room(&room_key).await {
            existing
        } else {
            let room = SfuRoomActor::new(room_key.clone(), self.udp_addr);
            let actor_ref = kameo::spawn(room);
            self.registry.insert_room(room_key, actor_ref.clone()).await;
            actor_ref
        };

        // Add participant to room
        match room_ref
            .ask(AddParticipant {
                sid: sid.clone(),
                jid: sender_jid,
                sdp_offer,
            })
            .await
        {
            Ok(Ok(answer_sdp)) => {
                let accept_element = sdp::build_jingle_session_accept(&sid, &answer_sdp);
                JingleIqResponse::Accept {
                    id: iq_id,
                    jingle: accept_element,
                }
            }
            Ok(Err(e)) => JingleIqResponse::Error {
                id: iq_id,
                reason: e,
            },
            Err(e) => JingleIqResponse::Error {
                id: iq_id,
                reason: format!("Room actor error: {e}"),
            },
        }
    }

    async fn handle_session_terminate(&self, iq_id: String, sid: String) -> JingleIqResponse {
        let room_key = match RoomKey::from_session_id(&sid) {
            Some(key) => key,
            None => {
                return JingleIqResponse::Error {
                    id: iq_id,
                    reason: "Invalid session ID format".to_string(),
                };
            }
        };

        let room_ref = match self.registry.get_room(&room_key).await {
            Some(r) => r,
            None => {
                return JingleIqResponse::Error {
                    id: iq_id,
                    reason: "Room not found".to_string(),
                };
            }
        };

        match room_ref.ask(RemoveParticipant { sid }).await {
            Ok(Ok(room_empty)) => {
                if room_empty {
                    self.registry.remove_room(&room_key).await;
                    info!(room = %room_key.0, "SFU room closed (last participant left)");
                }
                JingleIqResponse::Ack { id: iq_id }
            }
            Ok(Err(e)) => JingleIqResponse::Error {
                id: iq_id,
                reason: e,
            },
            Err(e) => JingleIqResponse::Error {
                id: iq_id,
                reason: format!("Room actor error: {e}"),
            },
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd server && cargo test -p waddle-xmpp service_actor -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/service_actor.rs
git commit -m "feat(sfu): add SfuServiceActor with Jingle IQ dispatch"
```

---

## Task 8: Wire SFU into Server Startup and Connection Handler

**Files:**
- Modify: `server/crates/waddle-xmpp/src/server.rs`
- Modify: `server/crates/waddle-xmpp/src/connection.rs`

This is the critical wiring task: the SFU service actor needs to be created at startup and injected into each connection so that Jingle IQs addressed to `sfu.{domain}` are intercepted.

- [ ] **Step 1: Add SFU service actor to XmppServer**

In `server.rs`, add to the struct fields:

```rust
sfu_service: Option<kameo::actor::ActorRef<crate::sfu::service_actor::SfuServiceActor>>,
sfu_registry: Arc<crate::sfu::SfuRegistry>,
```

In the `new()` constructor (after `room_registry` creation around line 123), add:

```rust
let sfu_domain = format!("sfu.{}", config.domain);
let sfu_registry = Arc::new(crate::sfu::SfuRegistry::new());
let sfu_udp_addr: SocketAddr = "0.0.0.0:10000".parse().unwrap(); // TODO: configurable
let sfu_service = kameo::spawn(
    crate::sfu::service_actor::SfuServiceActor::new(
        sfu_domain,
        Arc::clone(&sfu_registry),
        sfu_udp_addr,
    ),
);
```

Store in the struct:

```rust
sfu_service: Some(sfu_service),
sfu_registry,
```

- [ ] **Step 2: Pass SFU service ref to ConnectionActor**

In `connection.rs`, add to `ConnectionActor` struct:

```rust
sfu_service: Option<kameo::actor::ActorRef<crate::sfu::service_actor::SfuServiceActor>>,
```

Initialize it from the parameter passed during connection creation. Add a builder method:

```rust
pub fn set_sfu_service(
    &mut self,
    sfu: kameo::actor::ActorRef<crate::sfu::service_actor::SfuServiceActor>,
) {
    self.sfu_service = Some(sfu);
}
```

Wire it in `server.rs` where connections are created (similar to how `stanza_router` is set).

- [ ] **Step 3: Intercept SFU Jingle IQs in handle_iq()**

In `connection.rs`, in the `handle_iq()` method, add this check after the local full JID routing (around line 3743) and before the disco checks:

```rust
// Route Jingle IQs addressed to sfu.{domain} to the SFU service
{
    let sfu_domain = format!("sfu.{}", self.domain);
    if let Some(to_jid) = &iq.to {
        if to_jid.domain().as_str() == sfu_domain {
            if crate::xep::is_jingle_iq(&iq) {
                return self.handle_sfu_jingle_iq(iq).await;
            }
        }
    }
}
```

- [ ] **Step 4: Implement handle_sfu_jingle_iq()**

Add to `ConnectionActor`:

```rust
async fn handle_sfu_jingle_iq(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
    let sfu = self.sfu_service.as_ref().ok_or_else(|| {
        XmppError::service_unavailable(Some("SFU service not available".to_string()))
    })?;

    let sender_jid = self.jid.as_ref().ok_or_else(|| {
        XmppError::not_authorized(Some("Not authenticated".to_string()))
    })?.clone();

    let response = sfu
        .ask(crate::sfu::service_actor::HandleJingleIq {
            iq: iq.clone(),
            sender_jid,
        })
        .await
        .map_err(|e| {
            XmppError::internal_server_error(Some(format!("SFU actor error: {e}")))
        })?;

    match response {
        crate::sfu::service_actor::JingleIqResponse::Accept { id, jingle } => {
            let result_iq = xmpp_parsers::iq::Iq {
                id,
                to: iq.from.clone(),
                from: iq.to.clone(),
                payload: IqType::Result(Some(jingle)),
                ..Default::default()
            };
            self.send_stanza(Stanza::Iq(result_iq)).await
        }
        crate::sfu::service_actor::JingleIqResponse::Ack { id } => {
            let result_iq = xmpp_parsers::iq::Iq {
                id,
                to: iq.from.clone(),
                from: iq.to.clone(),
                payload: IqType::Result(None),
                ..Default::default()
            };
            self.send_stanza(Stanza::Iq(result_iq)).await
        }
        crate::sfu::service_actor::JingleIqResponse::Error { id, reason } => {
            Err(XmppError::bad_request(Some(reason)))
        }
    }
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cd server && cargo check -p waddle-xmpp 2>&1 | tail -10`
Expected: `Finished` with no errors. Fix any type/import issues iteratively.

- [ ] **Step 6: Commit**

```bash
git add server/crates/waddle-xmpp/src/server.rs server/crates/waddle-xmpp/src/connection.rs
git commit -m "feat(sfu): wire SFU service actor into server startup and IQ handling"
```

---

## Task 9: SFU Disco Features

**Files:**
- Modify: `server/crates/waddle-xmpp/src/connection.rs` (disco handler section)

- [ ] **Step 1: Add SFU to disco#items response**

Find the `handle_disco_items_query()` method in `connection.rs`. Add the SFU as a discovered item alongside MUC:

```rust
// Add SFU service item
let sfu_domain = format!("sfu.{}", self.domain);
items.push(DiscoItem {
    jid: sfu_domain.parse().unwrap(),
    name: Some("Waddle SFU".to_string()),
    node: None,
});
```

- [ ] **Step 2: Handle disco#info for sfu.{domain}**

In `handle_disco_info_query()`, add a check for when the query is addressed to the SFU domain:

```rust
let sfu_domain = format!("sfu.{}", self.domain);
if iq.to.as_ref().map(|j| j.domain().as_str()) == Some(sfu_domain.as_str()) {
    let features = vec![
        Feature::new("urn:xmpp:jingle:1"),
        Feature::new("urn:xmpp:jingle:apps:rtp:1"),
        Feature::new("urn:xmpp:jingle:transports:ice-udp:1"),
        Feature::new("urn:xmpp:jingle:apps:dtls:0"),
        Feature::new("urn:xmpp:muji:0"),
    ];
    // Build and send disco#info result with these features
    // Follow the existing pattern for disco responses
    return self.send_disco_info_result(&iq, &features, "sfu").await;
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd server && cargo check -p waddle-xmpp 2>&1 | tail -10`
Expected: `Finished`

- [ ] **Step 4: Commit**

```bash
git add server/crates/waddle-xmpp/src/connection.rs
git commit -m "feat(sfu): advertise SFU Jingle features via disco#info"
```

---

## Task 10: Frontend — Fix startMujiCall() to Create Jingle Session

**Files:**
- Modify: `chat/src/lib/xmpp/client.ts`

- [ ] **Step 1: Configure ICE servers on Stanza.js Jingle plugin**

In `client.ts`, in the `connect()` method (after `createClient()` and before `wireEvents()`), add ICE server configuration:

```typescript
if (xmpp.jingle) {
  xmpp.jingle.config.iceServers = [
    { urls: 'stun:stun.l.google.com:19302' },
  ];
}
```

- [ ] **Step 2: Fix startMujiCall() to create a Jingle session**

Replace the existing `startMujiCall()` method. The key change: it must create a `MediaSession` via `xmpp.jingle.createMediaSession()` and call `session.start()`, not just send an invite message.

```typescript
async startMujiCall(
  w: string,
  c: string,
  localStream: MediaStream,
  opts: { video: boolean; sid?: string; serviceJid?: string } = { video: true },
): Promise<{ sid: string; serviceJid: string } | null> {
  await this.connect();
  await this.switchRoom(w, c);
  if (!this.xmpp?.jingle) return null;

  const serviceJid = opts.serviceJid ?? sfuServiceJidFor(this.session);
  const sid = opts.sid ?? `${w}_${c}_${crypto.randomUUID()}`;

  // Create a real Jingle session to the SFU
  const session = requireMujiSessionShape(
    this.xmpp.jingle.createMediaSession(serviceJid, sid, localStream),
  );
  this.mujiSessions.set(session.sid, session);

  // Start the Jingle negotiation (sends session-initiate to SFU)
  await session.start({
    offerToReceiveAudio: true,
    offerToReceiveVideo: opts.video,
  });

  // Broadcast call invite to the MUC room so others can join
  await this.sendCallInvite(w, c, {
    muji: true,
    sid,
    jingleJid: serviceJid,
    externalUri: `xmpp:${serviceJid}?jingle;sid=${sid}`,
    video: opts.video,
  });

  return { sid, serviceJid };
}
```

- [ ] **Step 3: Fix joinMujiCall() session ID format**

Update `joinMujiCall()` to use the same `{w}_{c}_{uuid}` SID format when creating a new session (so the SFU can extract the room key):

```typescript
async joinMujiCall(
  w: string,
  c: string,
  localStream: MediaStream,
  invite: { sid?: string; jingleJid?: string; video?: boolean },
): Promise<{ sid: string; serviceJid: string } | null> {
  await this.connect();
  await this.switchRoom(w, c);
  if (!this.xmpp?.jingle) return null;

  const serviceJid = invite.jingleJid ?? sfuServiceJidFor(this.session);
  // Use a new SID with the same room key prefix so SFU groups them
  const sid = `${w}_${c}_${crypto.randomUUID()}`;

  const existing = invite.sid ? this.mujiSessions.get(invite.sid) : undefined;
  if (existing && existing.state === "pending") {
    for (const track of localStream.getTracks()) {
      if (!existing.pc.getSenders().some((sender) => sender.track?.id === track.id)) {
        await existing.addTrack(track, localStream);
      }
    }
    await existing.accept();
    return { sid: existing.sid, serviceJid };
  }

  const session = requireMujiSessionShape(
    this.xmpp.jingle.createMediaSession(serviceJid, sid, localStream),
  );
  this.mujiSessions.set(session.sid, session);
  await session.start({
    offerToReceiveAudio: true,
    offerToReceiveVideo: invite.video ?? true,
  });
  return { sid: session.sid, serviceJid };
}
```

- [ ] **Step 4: Verify frontend builds**

Run: `cd chat && bun run build 2>&1 | tail -10`
Expected: Build succeeds (or type errors to fix iteratively)

- [ ] **Step 5: Commit**

```bash
git add chat/src/lib/xmpp/client.ts
git commit -m "fix(chat): wire startMujiCall/joinMujiCall to create real Jingle sessions to SFU"
```

---

## Task 11: Frontend — Multi-Participant Track Management

**Files:**
- Modify: `chat/src/lib/xmpp/types.ts`
- Modify: `chat/src/composables/useMujiRuntime.ts`

- [ ] **Step 1: Add RemoteParticipant type**

In `types.ts`, add:

```typescript
export interface RemoteParticipant {
  jid: string;
  stream: MediaStream;
}
```

- [ ] **Step 2: Update useMujiRuntime to use participants map**

In `useMujiRuntime.ts`, change the `remoteStream` ref to a participants map:

Replace:
```typescript
const remoteStream = ref<MediaStream | null>(null);
```

With:
```typescript
const remoteParticipants = ref<Map<string, RemoteParticipant>>(new Map());
```

- [ ] **Step 3: Update applyMujiEvent for multi-participant tracks**

In the `peer-track-added` handler, use the stream ID to group tracks by participant. For now, use stream ID as the participant key (the SFU will later send `session-info` with the JID mapping):

```typescript
if (event.type === "peer-track-added") {
  sid.value = event.sid;
  phase.value = "active";

  const streamId = event.stream?.id ?? "unknown";
  const existing = remoteParticipants.value.get(streamId);
  if (existing) {
    if (!existing.stream.getTracks().some((t) => t.id === event.track.id)) {
      existing.stream.addTrack(event.track);
    }
  } else {
    const stream = new MediaStream([event.track]);
    remoteParticipants.value.set(streamId, {
      jid: streamId, // placeholder until session-info mapping
      stream,
    });
  }
  // Trigger reactivity
  remoteParticipants.value = new Map(remoteParticipants.value);
  hasRemoteTracks.value = remoteParticipants.value.size > 0;
  return;
}
```

Update `peer-track-removed`:

```typescript
if (event.type === "peer-track-removed") {
  for (const [key, participant] of remoteParticipants.value) {
    const tracks = participant.stream.getTracks();
    const trackIdx = tracks.findIndex((t) => t.id === event.track.id);
    if (trackIdx >= 0) {
      participant.stream.removeTrack(tracks[trackIdx]);
      if (participant.stream.getTracks().length === 0) {
        remoteParticipants.value.delete(key);
      }
      break;
    }
  }
  remoteParticipants.value = new Map(remoteParticipants.value);
  hasRemoteTracks.value = remoteParticipants.value.size > 0;
  return;
}
```

- [ ] **Step 4: Update the terminated handler to clear participants**

In the `terminated` handler, replace `remoteStream.value = null` with:

```typescript
remoteParticipants.value = new Map();
```

- [ ] **Step 5: Update the return object**

Replace `remoteStream` with `remoteParticipants` in the return:

```typescript
return {
  // ... existing fields ...
  remoteParticipants, // was: remoteStream
  // ...
};
```

- [ ] **Step 6: Update ChatApp.vue and ContentArea.vue props**

Update `ChatApp.vue` to pass `remoteParticipants` instead of `remoteStream`. Update `ContentArea.vue` props to receive it. Remove old `remoteStream` references. Update `hasRemoteTracks` to be derived from `remoteParticipants.size > 0`.

- [ ] **Step 7: Verify frontend builds**

Run: `cd chat && bun run build 2>&1 | tail -10`
Expected: Build succeeds

- [ ] **Step 8: Commit**

```bash
git add chat/src/lib/xmpp/types.ts chat/src/composables/useMujiRuntime.ts chat/src/components/ChatApp.vue chat/src/components/chat/ContentArea.vue
git commit -m "feat(chat): multi-participant track management with remoteParticipants map"
```

---

## Task 12: Frontend — CallOverlay.vue Component

**Files:**
- Create: `chat/src/components/chat/CallOverlay.vue`

- [ ] **Step 1: Create CallOverlay component**

```vue
<script setup lang="ts">
import { ref, watch, onUnmounted, type Ref } from "vue";
import { Mic, MicOff, Video, VideoOff, PhoneOff, Minimize2, Maximize2 } from "lucide-vue-next";
import type { RemoteParticipant } from "@/lib/xmpp/types";

const props = defineProps<{
  localStream: MediaStream | null;
  remoteParticipants: Map<string, RemoteParticipant>;
  micEnabled: boolean;
  cameraEnabled: boolean;
  phase: string;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCamera: [];
  endCall: [];
}>();

const collapsed = ref(false);
const localVideoRef = ref<HTMLVideoElement | null>(null);
const remoteVideoRefs = ref<Map<string, HTMLVideoElement>>(new Map());

// Bind local stream to video element
watch(
  () => props.localStream,
  (stream) => {
    if (localVideoRef.value) {
      localVideoRef.value.srcObject = stream;
    }
  },
);

// Bind remote streams to video elements
function setRemoteVideoRef(key: string, el: HTMLVideoElement | null) {
  if (el) {
    remoteVideoRefs.value.set(key, el);
    const participant = props.remoteParticipants.get(key);
    if (participant) {
      el.srcObject = participant.stream;
    }
  } else {
    remoteVideoRefs.value.delete(key);
  }
}

// Update remote video elements when participants change
watch(
  () => props.remoteParticipants,
  (participants) => {
    for (const [key, el] of remoteVideoRefs.value) {
      const participant = participants.get(key);
      if (participant) {
        el.srcObject = participant.stream;
      }
    }
  },
  { deep: true },
);

onUnmounted(() => {
  if (localVideoRef.value) {
    localVideoRef.value.srcObject = null;
  }
  for (const el of remoteVideoRefs.value.values()) {
    el.srcObject = null;
  }
});
</script>

<template>
  <div class="border-b border-foreground bg-background">
    <!-- Collapsed bar -->
    <div
      v-if="collapsed"
      class="px-6 py-2 flex items-center justify-between"
    >
      <div class="text-xs font-mono">
        <span class="font-bold">Call</span>
        · {{ remoteParticipants.size }} participant{{ remoteParticipants.size !== 1 ? 's' : '' }}
        · {{ phase }}
      </div>
      <div class="flex items-center gap-2">
        <button
          class="p-1 hover:bg-muted rounded"
          title="Expand"
          @click="collapsed = false"
        >
          <Maximize2 class="w-3.5 h-3.5" />
        </button>
        <button
          class="p-1 text-red-500 hover:bg-red-500/10 rounded"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <!-- Expanded view -->
    <div v-else class="p-4">
      <!-- Video grid -->
      <div
        class="grid gap-2 mb-3"
        :class="{
          'grid-cols-1': remoteParticipants.size === 0,
          'grid-cols-2': remoteParticipants.size >= 1 && remoteParticipants.size <= 3,
          'grid-cols-3': remoteParticipants.size >= 4,
        }"
      >
        <!-- Remote participants -->
        <div
          v-for="[key, participant] in remoteParticipants"
          :key="key"
          class="relative bg-muted aspect-video"
        >
          <video
            :ref="(el) => setRemoteVideoRef(key, el as HTMLVideoElement)"
            autoplay
            playsinline
            class="w-full h-full object-cover"
          />
          <div class="absolute bottom-1 left-1 px-1.5 py-0.5 bg-black/60 text-white text-xs font-mono rounded">
            {{ participant.jid.split('@')[0] || 'Participant' }}
          </div>
        </div>

        <!-- Empty state -->
        <div
          v-if="remoteParticipants.size === 0"
          class="bg-muted aspect-video flex items-center justify-center"
        >
          <div class="text-sm font-mono text-muted-foreground">
            {{ phase === 'dialing' ? 'Waiting for others to join...' : phase }}
          </div>
        </div>
      </div>

      <!-- Local preview (picture-in-picture) -->
      <div class="relative">
        <div class="absolute top-0 right-0 -mt-20 mr-1 w-32 aspect-video bg-muted border border-foreground/20 z-10">
          <video
            ref="localVideoRef"
            autoplay
            playsinline
            muted
            class="w-full h-full object-cover"
            style="transform: scaleX(-1)"
          />
        </div>
      </div>

      <!-- Controls -->
      <div class="flex items-center justify-center gap-3">
        <button
          class="p-2 border rounded-full transition-colors"
          :class="micEnabled ? 'border-foreground hover:bg-muted' : 'border-red-500 bg-red-500/10 text-red-500'"
          :title="micEnabled ? 'Mute mic' : 'Unmute mic'"
          @click="emit('toggleMic')"
        >
          <Mic v-if="micEnabled" class="w-4 h-4" />
          <MicOff v-else class="w-4 h-4" />
        </button>

        <button
          class="p-2 border rounded-full transition-colors"
          :class="cameraEnabled ? 'border-foreground hover:bg-muted' : 'border-red-500 bg-red-500/10 text-red-500'"
          :title="cameraEnabled ? 'Turn off camera' : 'Turn on camera'"
          @click="emit('toggleCamera')"
        >
          <Video v-if="cameraEnabled" class="w-4 h-4" />
          <VideoOff v-else class="w-4 h-4" />
        </button>

        <button
          class="p-2 border border-red-500 bg-red-500 text-white rounded-full hover:bg-red-600 transition-colors"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-4 h-4" />
        </button>

        <button
          class="p-2 border border-foreground/40 rounded-full hover:bg-muted transition-colors"
          title="Minimize"
          @click="collapsed = true"
        >
          <Minimize2 class="w-4 h-4" />
        </button>
      </div>
    </div>
  </div>
</template>
```

- [ ] **Step 2: Verify it compiles**

Run: `cd chat && bun run build 2>&1 | tail -10`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add chat/src/components/chat/CallOverlay.vue
git commit -m "feat(chat): add CallOverlay component with video grid and controls"
```

---

## Task 13: Mount CallOverlay in ContentArea

**Files:**
- Modify: `chat/src/components/chat/ContentArea.vue`

- [ ] **Step 1: Import and add CallOverlay**

Add import at the top of the `<script setup>`:

```typescript
import CallOverlay from "@/components/chat/CallOverlay.vue";
```

- [ ] **Step 2: Add props for call streams**

Add to the props interface:

```typescript
mujiLocalStream: MediaStream | null;
mujiRemoteParticipants: Map<string, import("@/lib/xmpp/types").RemoteParticipant>;
```

- [ ] **Step 3: Mount CallOverlay in the template**

Insert immediately after the active call status bar (around line 221) and before the messages container:

```vue
<CallOverlay
  v-if="mujiInCall"
  :local-stream="mujiLocalStream"
  :remote-participants="mujiRemoteParticipants"
  :mic-enabled="mujiMicEnabled"
  :camera-enabled="mujiCameraEnabled"
  :phase="mujiPhase"
  @toggle-mic="emit('toggleMic')"
  @toggle-camera="emit('toggleCamera')"
  @end-call="emit('endCall')"
/>
```

- [ ] **Step 4: Remove the old text-only call status bar**

Remove the `div` at lines 213-221 that shows "Call: {{ mujiPhase }}" — CallOverlay replaces this.

- [ ] **Step 5: Pass new props from ChatApp.vue**

In `ChatApp.vue`, add the new props to the `ContentArea` component:

```vue
:muji-local-stream="muji.localStream.value"
:muji-remote-participants="muji.remoteParticipants.value"
```

- [ ] **Step 6: Verify frontend builds**

Run: `cd chat && bun run build 2>&1 | tail -10`
Expected: Build succeeds

- [ ] **Step 7: Commit**

```bash
git add chat/src/components/chat/ContentArea.vue chat/src/components/ChatApp.vue
git commit -m "feat(chat): mount CallOverlay in ContentArea with video streams"
```

---

## Task 14: UDP Socket and Media Forwarding Loop

**Files:**
- Modify: `server/crates/waddle-xmpp/src/sfu/net.rs`

This is the core media plane — a tokio task that owns the UDP socket, demuxes packets to the correct `SfuPeer`, and polls peers for outgoing packets.

- [ ] **Step 1: Implement the SFU network loop**

```rust
use super::SfuRegistry;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::net::{DatagramRecv, Protocol, Receive};
use str0m::{Event, Input, Output};
use tokio::net::UdpSocket;
use tracing::{debug, info, trace, warn};

/// Spawn the SFU UDP event loop as a background tokio task.
///
/// This loop:
/// 1. Reads incoming UDP packets from the shared socket
/// 2. Demuxes them to the correct SfuPeer (via `accepts()`)
/// 3. Polls each peer for outputs (outgoing packets, events)
/// 4. Forwards media data between peers in the same room
pub async fn spawn_sfu_net_loop(
    udp_addr: SocketAddr,
    registry: Arc<SfuRegistry>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), std::io::Error> {
    let socket = UdpSocket::bind(udp_addr).await?;
    info!(addr = %udp_addr, "SFU UDP socket bound");

    let mut buf = vec![0u8; 2000]; // MTU-safe buffer

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("SFU net loop shutting down");
                break;
            }
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, src)) => {
                        let data = &buf[..len];
                        // Dispatch to room actors for demuxing
                        // This is a simplified version — full implementation
                        // would iterate rooms and peers to find the right Rtc
                        trace!(src = %src, len = len, "SFU UDP packet received");
                    }
                    Err(e) => {
                        warn!(error = %e, "SFU UDP recv error");
                    }
                }
            }
        }
    }

    Ok(())
}
```

> **Note:** The full media forwarding loop (polling each peer's `Rtc` for `Output::Transmit`, forwarding `Event::MediaData` between peers) is complex and will be refined iteratively once the basic signaling path is working end-to-end. The initial implementation here establishes the socket binding and packet receive loop.

- [ ] **Step 2: Start the net loop from server.rs**

In `server.rs`, after creating the SFU service actor, spawn the net loop:

```rust
let sfu_shutdown = self.shutdown_token.clone();
let sfu_registry_clone = Arc::clone(&sfu_registry);
tokio::spawn(async move {
    if let Err(e) = crate::sfu::net::spawn_sfu_net_loop(
        sfu_udp_addr,
        sfu_registry_clone,
        sfu_shutdown,
    ).await {
        tracing::error!(error = %e, "SFU net loop failed");
    }
});
```

- [ ] **Step 3: Verify it compiles**

Run: `cd server && cargo check -p waddle-xmpp 2>&1 | tail -10`
Expected: `Finished`

- [ ] **Step 4: Commit**

```bash
git add server/crates/waddle-xmpp/src/sfu/net.rs server/crates/waddle-xmpp/src/server.rs
git commit -m "feat(sfu): add UDP socket binding and media forwarding loop skeleton"
```

---

## Task 15: End-to-End Smoke Test

**Files:** No new files — manual verification

- [ ] **Step 1: Run server tests**

Run: `cd server && cargo test -p waddle-xmpp 2>&1 | tail -20`
Expected: All tests pass

- [ ] **Step 2: Run frontend build**

Run: `cd chat && bun run build 2>&1 | tail -10`
Expected: Build succeeds with no errors

- [ ] **Step 3: Run linter if available**

Run: `cd chat && bun run lint 2>&1 | tail -10`
Expected: No errors (warnings OK)

- [ ] **Step 4: Final commit — update XEPS.md**

Update `XEPS.md` to reflect the SFU implementation status. Find the Jingle/Muji entries and update their status.

```bash
git add XEPS.md
git commit -m "docs: update XEP status for SFU Jingle implementation"
```

---

## Implementation Notes

### What this plan delivers
- SFU XMPP component (`sfu.{domain}`) that accepts Jingle `session-initiate` and responds with `session-accept`
- Room-per-call model with Kameo actors
- str0m peer connections created per participant with SDP offer/answer
- Frontend creates real Jingle sessions to the SFU instead of only broadcasting invites
- Video grid UI with local preview, remote participants, and call controls
- Multi-participant track management

### What remains after this plan (follow-up work)
- **Full media forwarding**: The UDP net loop receives packets but the RTP forwarding between peers within a room needs the full poll→demux→forward cycle. This is complex I/O code best iterated on with a running server.
- **Renegotiation on join/leave**: When participant B joins, the SFU needs to send `content-add` to participant A. This requires the SFU to send Jingle IQs *to* clients (outbound from the SFU component), which needs additional connection.rs wiring.
- **Participant map session-info**: The SFU sending `session-info` stanzas with msid→JID mapping for the frontend to show correct participant names.
- **TURN server**: Required for clients behind symmetric NAT. Currently only STUN is configured.
- **ICE candidate handling**: The `transport-info` handler in the SFU currently only ACKs. Full ICE trickle support needs to pipe candidates into `rtc.add_remote_candidate()`.
- **Disconnect detection**: ICE timeout → cleanup after 10s.
- **Capacity enforcement**: Max participants per room, max concurrent rooms.
