# RFC-0010: Live Streaming

## Summary

Live streaming enables one-to-many broadcast of video content within Waddle channels, supporting both camera streams and RTMP ingest from streaming software.

## Motivation

Content creators in communities want to:
- Stream gameplay to their Waddle
- Host live events and Q&As
- Broadcast presentations
- Share live content without external platforms

## Detailed Design

### Stream Session

```
LiveStream
├── id: UUID
├── channel_id: UUID
├── streamer_did: DID
├── title: String
├── description: String (optional)
├── thumbnail: URL (optional)
├── source: StreamSource
├── status: StreamStatus
├── settings: StreamSettings
├── viewer_count: Integer
├── started_at: Timestamp
└── ended_at: Timestamp (optional)
```

### Stream Sources

```
StreamSource
├── type: "camera" | "rtmp" | "screen"
├── rtmp_url: URL (for RTMP ingest)
├── rtmp_key: String (for RTMP ingest)
└── resolution: Resolution
```

**RTMP Ingest**: Allows OBS, Streamlabs, etc. to stream.

### Stream Status

```
StreamStatus
├── state: "scheduled" | "starting" | "live" | "ended"
├── health: "excellent" | "good" | "poor" | "disconnected"
├── bitrate: Integer (current kbps)
└── dropped_frames: Integer
```

### Streaming Pipeline

```
[OBS/Browser] → [RTMP Ingest] → [Transcoder] → [HLS/DASH] → [CDN] → [Viewers]
                      ↓
              [WebRTC SFU] → [Low-latency viewers]
```

**Dual delivery**:
- HLS/DASH: Reliable, higher latency (10-30s)
- WebRTC: Low latency (~1-3s), more resource intensive

### Quality Levels

Transcoder produces adaptive bitrate variants:

| Quality | Resolution | Bitrate | Framerate |
|---------|------------|---------|-----------|
| Source  | Native     | Passthrough | Native |
| 1080p   | 1920x1080  | 6000 kbps | 60fps |
| 720p    | 1280x720   | 3000 kbps | 30fps |
| 480p    | 854x480    | 1500 kbps | 30fps |
| 360p    | 640x360    | 800 kbps  | 30fps |

### Stream Settings

```
StreamSettings
├── quality: QualityPreset
├── latency_mode: "normal" | "low" | "ultra_low"
├── dvr_enabled: Boolean (pause/rewind live)
├── chat_overlay: Boolean
├── viewer_count_visible: Boolean
├── subscriber_only: Boolean
└── recording_enabled: Boolean
```

### Chat Integration

Live stream chat features:
- Dedicated chat mode (stream-focused UI)
- Chat replay for VODs
- Highlighted messages (tips, subscriptions)
- Slow mode during high traffic

### Moderation

Stream-specific moderation:
- Chat moderation per [RFC-0013](./0013-moderation.md)
- Stream preview before going live
- Emergency stream termination (admin)
- Viewer ban from stream

### VOD (Video on Demand)

After stream ends:
- Option to save as VOD
- Automatic chapter markers
- Chat replay sync
- Trim/edit before publish

### Permissions

- `go_live`: Can start live stream
- `view_streams`: Can watch streams
- `manage_streams`: Can end others' streams

### Infrastructure Requirements

Self-hosting requires:
- RTMP ingest server (nginx-rtmp, SRS)
- Transcoding (FFmpeg, hardware encoding)
- Media server (HLS/WebRTC)
- Storage for VODs

## API Endpoints

```
POST   /channels/:id/streams         Start stream
GET    /channels/:id/streams         Get active stream
PATCH  /streams/:id                  Update stream info
DELETE /streams/:id                  End stream
GET    /streams/:id/key              Get RTMP key (streamer only)
POST   /streams/:id/key/rotate       Rotate RTMP key
GET    /streams/:id/playback         Get playback URLs
GET    /streams/:id/stats            Get stream stats
POST   /streams/:id/clips            Create clip
GET    /streams/:id/vod              Get VOD after stream
```

## WebSocket Events

- `stream.started`: Stream went live
- `stream.updated`: Stream metadata changed
- `stream.ended`: Stream finished
- `stream.health`: Health status update
- `stream.viewer_count`: Viewer count changed

## Scaling Considerations

- CDN for HLS distribution
- Multiple RTMP ingest points
- Horizontal transcoder scaling
- WebRTC SFU clustering

## Related

- [RFC-0008: Watch Together](./0008-watch-together.md)
- [RFC-0009: Screen Sharing](./0009-screen-sharing.md)
- [RFC-0013: Moderation](./0013-moderation.md)
