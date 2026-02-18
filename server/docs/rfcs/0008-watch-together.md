# RFC-0008: Watch Together

## Summary

Watch Together enables synchronized media playback in channels, allowing users to watch videos, listen to music, or view content simultaneously.

## Motivation

Social viewing experiences:
- Watch parties for videos/streams
- Listen-along sessions for music
- Synchronized presentations
- Community movie nights

## Detailed Design

### Watch Session

```
WatchSession
├── id: UUID
├── channel_id: UUID
├── host_did: DID
├── media: MediaSource
├── state: PlaybackState
├── participants: DID[]
├── settings: WatchSettings
├── created_at: Timestamp
└── ended_at: Timestamp (optional)
```

### Media Sources

```
MediaSource
├── type: "url" | "youtube" | "twitch" | "attachment"
├── url: URL
├── title: String
├── thumbnail: URL (optional)
├── duration: Duration (optional)
└── embed_data: EmbedMetadata (optional)
```

Supported sources:
- Direct video URLs (MP4, WebM)
- YouTube videos
- Twitch streams/VODs
- Uploaded attachments
- Spotify (audio, with account linking)

### Playback State

```
PlaybackState
├── status: "playing" | "paused" | "buffering" | "ended"
├── position: Duration (current timestamp)
├── playback_rate: Float (1.0 = normal)
├── updated_at: Timestamp
└── updated_by: DID
```

### Synchronization Protocol

1. **Host creates session** with media URL
2. **Participants join** via channel UI
3. **State broadcasts** at regular intervals (1Hz)
4. **Clients sync** to authoritative state
5. **Drift correction** adjusts local playback

**Sync tolerance**: ±2 seconds before forced resync

### Host Controls

Only the host can:
- Play/pause
- Seek to position
- Change playback rate
- Skip to next (if queue exists)
- End session

**Host transfer**: Host can transfer control to another participant.

### Queue System

Optional queue for consecutive media:

```
WatchQueue
├── items: MediaSource[]
├── current_index: Integer
└── repeat_mode: "none" | "one" | "all"
```

### Settings

```
WatchSettings
├── allow_chat_reactions: Boolean
├── sync_tolerance: Duration
├── allow_host_transfer: Boolean
├── end_on_host_leave: Boolean
└── max_participants: Integer
```

### Chat Integration

During watch sessions:
- Reactions overlaid on video (optional)
- Timestamp references in chat ("at 5:23...")
- Auto-generated "Now watching" message

### Permissions

New permissions for channels:
- `start_watch_session`: Can initiate Watch Together
- `join_watch_session`: Can join active sessions

## API Endpoints

```
POST   /channels/:id/watch              Start session
GET    /channels/:id/watch              Get active session
DELETE /channels/:id/watch              End session
POST   /watch/:id/join                  Join session
POST   /watch/:id/leave                 Leave session
PATCH  /watch/:id/state                 Update playback (host only)
POST   /watch/:id/queue                 Add to queue
PATCH  /watch/:id/host                  Transfer host
```

## WebSocket Events

- `watch.started`: New session in channel
- `watch.ended`: Session ended
- `watch.state`: Playback state update
- `watch.participant.joined`: User joined
- `watch.participant.left`: User left
- `watch.queue.updated`: Queue changed

## Client Implementation

Clients should:
1. Use native video players where possible
2. Implement drift correction algorithm
3. Buffer appropriately for network variance
4. Handle source-specific APIs (YouTube IFrame API)

## Limitations

- No DRM content support (technical limitation)
- Geographic restrictions apply per source
- Quality depends on participant bandwidth
- Mobile background playback varies by platform

## Related

- [RFC-0002: Channels](./0002-channels.md)
- [RFC-0009: Screen Sharing](./0009-screen-sharing.md)
- [RFC-0010: Live Streaming](./0010-live-streaming.md)
