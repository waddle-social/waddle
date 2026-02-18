# RFC-0009: Screen Sharing

## Summary

Screen sharing allows users to broadcast their screen, window, or application to channel members in real-time.

## Motivation

Use cases:
- Technical support and debugging
- Presentations and demos
- Collaborative work sessions
- Gaming with friends

## Detailed Design

### Screen Share Session

```
ScreenShareSession
├── id: UUID
├── channel_id: UUID
├── broadcaster_did: DID
├── stream_id: String
├── source: ShareSource
├── settings: ShareSettings
├── viewers: DID[]
├── started_at: Timestamp
└── ended_at: Timestamp (optional)
```

### Share Sources

```
ShareSource
├── type: "screen" | "window" | "tab"
├── name: String (display/window name)
├── resolution: Resolution
└── has_audio: Boolean
```

### Streaming Protocol

**WebRTC-based streaming**:

1. **Broadcaster** captures screen via browser/native API
2. **SFU (Selective Forwarding Unit)** receives stream
3. **Viewers** connect to SFU for stream
4. **Adaptive bitrate** based on viewer bandwidth

**Media Server Options**:
- Janus Gateway (self-hosted)
- mediasoup (Node.js-based)
- LiveKit (commercial/self-hosted)

### Quality Settings

```
ShareSettings
├── max_resolution: "720p" | "1080p" | "4k"
├── max_framerate: Integer (15, 30, 60)
├── bitrate: Integer (kbps)
├── audio_enabled: Boolean
├── optimize_for: "quality" | "motion"
└── cursor_visible: Boolean
```

**Presets**:
- **Document**: 1080p, 5fps, high quality
- **General**: 1080p, 30fps, balanced
- **Gaming**: 1080p, 60fps, motion optimized

### Remote Control (Optional)

Allow viewers to request control:

```
RemoteControlRequest
├── session_id: UUID
├── requester_did: DID
├── status: "pending" | "granted" | "denied"
└── permissions: ControlPermissions

ControlPermissions
├── mouse: Boolean
├── keyboard: Boolean
└── clipboard: Boolean
```

Requires explicit grant from broadcaster.

### Permissions

Channel permissions:
- `share_screen`: Can start screen share
- `view_screen_share`: Can view active shares

### Bandwidth Management

- Simulcast: Multiple quality layers
- SVC: Scalable Video Coding for adaptation
- Viewer quality selection
- Auto-quality based on connection

### Recording (Optional)

Screen shares can be recorded if:
- Waddle admin enables recording
- Broadcaster consents
- Viewers are notified

## API Endpoints

```
POST   /channels/:id/screenshare        Start sharing
DELETE /channels/:id/screenshare        Stop sharing
GET    /channels/:id/screenshare        Get active session
POST   /screenshare/:id/view            Start viewing
DELETE /screenshare/:id/view            Stop viewing
POST   /screenshare/:id/control/request Request control
POST   /screenshare/:id/control/grant   Grant control
POST   /screenshare/:id/control/revoke  Revoke control
```

## WebSocket Events

- `screenshare.started`: Share session began
- `screenshare.ended`: Share session ended
- `screenshare.viewer.joined`: Viewer connected
- `screenshare.viewer.left`: Viewer disconnected
- `screenshare.control.requested`: Control requested
- `screenshare.control.granted`: Control given
- `screenshare.quality.changed`: Quality adjusted

## WebRTC Signaling

Screen sharing uses standard WebRTC signaling:

```json
{
  "type": "screenshare.signal",
  "session_id": "...",
  "signal": {
    "type": "offer" | "answer" | "ice-candidate",
    "sdp": "...",
    "candidate": "..."
  }
}
```

## Client Requirements

- Browser: `getDisplayMedia()` API
- Desktop: Native screen capture (Electron/Tauri)
- Mobile: Limited support (iOS/Android restrictions)

## Limitations

- Browser tab audio capture varies
- Some platforms restrict capture
- DRM content blocked by browsers
- High bandwidth for quality streams

## Related

- [RFC-0008: Watch Together](./0008-watch-together.md)
- [RFC-0010: Live Streaming](./0010-live-streaming.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
