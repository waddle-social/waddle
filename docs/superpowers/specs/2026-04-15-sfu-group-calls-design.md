# SFU-Mediated Group Calls via XMPP-Native Jingle Signaling

## Problem

The call system shipped in PR #73 (issue #62) is scaffolding that never completed the media path. `startMujiCall()` broadcasts a call invite but never creates a Jingle session. `joinMujiCall()` creates a Stanza.js MediaSession pointed at a non-existent peer. No video elements exist in the UI. The SFU backends (`embedded_sfu.rs`, `webrtc_rs_sfu.rs`) generate session URLs but have no WebRTC media handling.

Users cannot see or hear each other in calls.

## Goal

Discord/Slack-style group video calls: any channel member can start a call, others join, everyone sees and hears each other via an SFU that forwards media. Scales to 32 participants per call.

## Architecture

### XMPP-Native SFU Component

The SFU is an XMPP component at `sfu.{domain}` — a first-class entity like `muc.{domain}`. It speaks Jingle (XEP-0166) to negotiate WebRTC sessions with clients.

```
Client A ──Jingle IQs (XMPP/WS)──► waddle-server ◄──Jingle IQs── Client B
    │                                     │                            │
    │     WebRTC media (DTLS/SRTP)        │    WebRTC media            │
    └────────────────────────────────────►SFU◄──────────────────────────┘
```

- Signaling (Jingle IQs) flows through the existing XMPP WebSocket connection
- Media (audio/video RTP) flows directly between client and server over UDP/DTLS
- Each active call is a `SfuRoomActor` (Kameo) that owns webrtc-rs peer connections
- `SfuServiceActor` is the top-level component that receives Jingle IQs and dispatches to room actors
- Single binary — no external services

### WebRTC Library

[`str0m`](https://crates.io/crates/str0m) — a sans-IO WebRTC library for Rust. It doesn't own sockets; you drive it with your own event loop. This fits the Kameo actor model: the actor owns the `str0m::Rtc` instance and polls it on ticks. Mature for SFU use cases with a `direct` API for RTP forwarding without transcoding.

## Call Lifecycle

### Starting a Call

1. User A clicks "Start Call" — frontend acquires local media via `getUserMedia()`
2. Frontend sends XEP-0482 call invite to the MUC room (groupchat message) containing the SFU JID and a room token derived from `{waddle_id}_{channel_id}`
3. Frontend sends Jingle `session-initiate` to `sfu.{domain}` with SDP offer containing A's audio/video tracks. The session ID encodes the room key: `{waddle_id}_{channel_id}_{uuid}`
4. SFU router receives IQ → `SfuServiceActor` creates `SfuRoomActor` → creates `str0m::Rtc` peer for A
5. SFU responds with Jingle `session-accept` containing SDP answer
6. ICE candidates exchanged via Jingle `transport-info` IQs
7. WebRTC connects — A's media flows to SFU

### Joining a Call

1. User B sees call invite in chat, clicks "Join"
2. Frontend acquires local media, sends Jingle `session-initiate` to `sfu.{domain}` with the same room key prefix in the session ID
3. `SfuServiceActor` finds existing `SfuRoomActor` → creates new `str0m::Rtc` peer for B
4. SFU sends B's tracks in the `session-accept` to B (including A's existing tracks as receive-only)
5. SFU renegotiates with A via Jingle `content-add` to add B's tracks
6. `peerTrackAdded` fires on both clients — media flows bidirectionally through SFU

### During a Call

- SFU receives RTP from each participant, rewrites SSRC, forwards to all others (no transcoding)
- New participants joining triggers renegotiation with existing participants (`content-add`)
- Participants leaving triggers track removal and renegotiation with remaining participants
- Mute/unmute is client-side only (`track.enabled`) — no SFU involvement

### Ending a Call

- Participant sends Jingle `session-terminate` → SFU removes their peer connection, notifies others
- Last participant leaving → `SfuRoomActor` self-terminates
- ICE failure (unexpected disconnect) → cleanup after timeout (10s)

### Room Identity

Keyed by `{waddle_id}/{channel_id}` — one active call per channel. Multiple channels can have concurrent calls.

## Server Components

### Routing Extension

- Add `LocalSfu` variant to `RoutingDestination` in `routing.rs`
- Add `sfu_domain: String` to `RouterConfig` (defaults to `sfu.{local_domain}`)
- Route IQs with domain `sfu.{domain}` to SFU service actor

### SfuServiceActor

Top-level Kameo actor, owns the registry of active call rooms.

Receives routed Jingle IQs and dispatches:
- `session-initiate` with new room key → spawns `SfuRoomActor`, forwards
- `session-initiate` with existing room key → forwards to existing room actor
- `session-accept`, `transport-info`, `session-terminate` → looks up room by SID, forwards
- Disco info queries → responds with Jingle/Muji feature advertisements

### SfuRoomActor

One per active call. Owns all participant state:

```
SfuRoomActor {
    room_key: String,                              // "{waddle_id}/{channel_id}"
    participants: HashMap<FullJid, Participant>,
}

Participant {
    jid: FullJid,
    jingle_sid: String,
    rtc: str0m::Rtc,                               // WebRTC peer connection
    published_tracks: Vec<TrackInfo>,
}
```

Responsibilities:
- Process Jingle SDP offers → generate SDP answers via str0m
- Participant joins → renegotiate with all existing participants to add new tracks
- Participant leaves → remove tracks, renegotiate with remaining participants
- Forward incoming RTP packets from each participant to all others (core SFU loop)
- Detect ICE disconnection → cleanup after timeout
- Self-terminate when empty

### Media Forwarding

Simple forwarding — no transcoding, no mixing. When the SFU receives an RTP packet from participant A, it rewrites the SSRC and forwards to B, C, etc. `str0m` handles this with its `direct` API. CPU cost is minimal (packet copying only).

### UDP Socket Management

`str0m` is sans-IO — we own the sockets. One shared UDP socket (or a small pool) bound on the server, multiplexed across all peer connections via ICE credentials. The SFU actor drives the socket with a tokio event loop, dispatching incoming packets to the correct `Rtc` instance based on ICE ufrag.

## Frontend Components

### startMujiCall() Fix

Currently only broadcasts an invite. Must also:
1. Create a Jingle `MediaSession` to the SFU JID via `xmpp.jingle.createMediaSession(sfuJid, sid, localStream)`
2. Call `session.start()` to send `session-initiate`
3. Return `{ sid, serviceJid }` as before

The SFU JID: `sfu.{domain}` derived from `session.jid.split('@')[1]`.

### ICE Server Configuration

Configure Stanza.js `SessionManager` with ICE servers. Hardcode Google STUN initially:
```typescript
xmpp.jingle.config.iceServers = [{ urls: 'stun:stun.l.google.com:19302' }];
```
Later: server advertises ICE servers via Jingle transport elements in `session-accept`.

### Multi-Participant Track Management

With an SFU, all remote tracks arrive on a single peer connection but represent different participants. Track-to-participant mapping via Jingle `session-info` stanzas (custom payload, not a standardized XEP — internal protocol between our SFU and client):

- When the SFU adds tracks for a new participant, it sends a `session-info` IQ containing a mapping of media stream IDs (msid) to participant JIDs
- Frontend maintains `Map<msid, participantJid>`
- When `peerTrackAdded` fires, the track's `stream.id` is looked up in the map to determine which participant it belongs to

`remoteStream` ref changes from `Ref<MediaStream | null>` to `Ref<Map<string, { jid: string; stream: MediaStream }>>` — one entry per remote participant.

### CallOverlay.vue Component

Floating overlay rendered when `mujiPhase !== 'idle'`. Positioned above the message list in `ContentArea`.

Layout:
- Grid of `<video>` elements, one per remote participant
- Local preview as a small picture-in-picture corner element (muted, mirrored CSS transform)
- Controls bar at the bottom: mute mic, toggle camera, end call
- Participant count indicator
- Collapsible to a compact bar when user wants to scroll chat

State sources:
- `localStream` → local `<video srcObject>` (muted)
- `remoteParticipants` map → one `<video srcObject>` per entry
- `micEnabled`, `cameraEnabled` → toggle button states
- `phase` → overlay visibility and status indicators

Video element binding: `ref` callback sets `videoEl.srcObject = stream` when the stream ref changes. Cleanup on unmount stops tracks.

### Renegotiation Handling

When a new participant joins mid-call:
1. SFU sends Jingle `content-add` to existing participants
2. Stanza.js handles the renegotiation internally (updates SDP)
3. `peerTrackAdded` fires for the new tracks
4. SFU sends `session-info` with updated msid→JID mapping
5. Frontend adds the new entry to `remoteParticipants` map
6. Vue reactivity renders a new `<video>` element

When a participant leaves:
1. SFU sends Jingle `content-remove` or `session-info` with updated mapping
2. `peerTrackRemoved` fires
3. Frontend removes the entry from `remoteParticipants` map
4. Vue reactivity removes the `<video>` element

## Testing

### Server-Side (Rust)

Per CLAUDE.md XEP test-suite rule, dedicated test suites for each XEP involved:

- **SFU routing tests**: Jingle IQs to `sfu.{domain}` reach SFU actor. IQs to `muc.{domain}` still route to MUC. Unknown domains route to S2S.
- **SFU Jingle session lifecycle tests**: `session-initiate` creates room actor and returns `session-accept`. Second `session-initiate` with same room key joins existing room. `session-terminate` removes participant. Last participant leaving destroys room.
- **SFU capacity tests**: Room limits, participant limits, session limits.
- **str0m integration tests**: SDP offer/answer round-trip produces valid WebRTC descriptions. ICE candidate exchange. Track forwarding between two peers in same room.
- **Disco tests**: SFU component responds to disco#info with correct Jingle/Muji features.

### Frontend (Bun)

- **useMujiRuntime state machine tests**: Phase transitions for start→dialing→active→idle. Error states. Renegotiation events.
- **Track mapping tests**: `session-info` participant mapping correctly routes tracks to the right entry in the participants map.
- **CallOverlay rendering tests**: Video elements created per participant. Local preview renders muted. Controls toggle state.

### Integration (Manual)

- Two browser tabs: start call in one, join from other, verify bidirectional video
- Three participants: verify all see each other
- Participant leaves mid-call: remaining participants continue
- Network interruption: reconnection or clean error state
