# ADR-007: Durable Objects for Real-Time State

**Status:** Accepted

**Date:** 2025-09-30

## Context

Waddle requires real-time features that need stateful coordination:

- **Chat messages**: Real-time delivery via WebSockets
- **Presence**: Online/offline status, typing indicators
- **Voice channels**: Participant tracking, session coordination
- **Collaborative editing**: Conflict-free concurrent updates (future)
- **Live reactions**: Real-time emoji reactions

Traditional serverless Workers are stateless and ephemeral. We need a mechanism to:

- Maintain WebSocket connections
- Coordinate state across concurrent requests
- Provide strong consistency guarantees
- Scale per-Waddle or per-channel

## Decision

We will use **Cloudflare Durable Objects** as stateful coordinators for all real-time features.

### Durable Object Architecture

```
┌──────────────────────────────────────┐
│  Clients (WebSocket connections)     │
└───────────┬──────────────────────────┘
            │ WSS connect
            ▼
┌──────────────────────────────────────┐
│  Workers (stateless request handler) │
│  - Route to correct DO                │
│  - Upgrade to WebSocket               │
└───────────┬──────────────────────────┘
            │ Service binding
            ▼
┌──────────────────────────────────────┐
│  Durable Objects                      │
│  ┌────────────┐  ┌────────────┐      │
│  │ Waddle DO  │  │ Channel DO │      │
│  │ - Members  │  │ - Active   │      │
│  │ - Presence │  │ - Messages │      │
│  └────────────┘  └────────────┘      │
└──────────────────────────────────────┘
```

### Durable Object Types

#### 1. WaddleDurableObject
Manages Waddle-level state:
- Active members
- Presence tracking
- Broadcast messages

```typescript
export class WaddleDurableObject {
  state: DurableObjectState;
  env: Env;
  sessions: Map<string, WebSocket>; // userId -> WebSocket
  presence: Map<string, PresenceState>;

  constructor(state: DurableObjectState, env: Env) {
    this.state = state;
    this.env = env;
    this.sessions = new Map();
    this.presence = new Map();
  }

  async fetch(request: Request): Promise<Response> {
    // Handle WebSocket upgrade
    if (request.headers.get('Upgrade') === 'websocket') {
      return this.handleWebSocket(request);
    }

    // Handle HTTP requests
    const url = new URL(request.url);
    switch (url.pathname) {
      case '/presence':
        return this.getPresence();
      case '/broadcast':
        return this.broadcast(request);
      default:
        return new Response('Not found', { status: 404 });
    }
  }

  async handleWebSocket(request: Request): Promise<Response> {
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);

    const userId = this.extractUserId(request);
    this.sessions.set(userId, server);

    server.accept();

    // Send current presence state
    server.send(JSON.stringify({
      type: 'presence_state',
      users: Array.from(this.presence.entries()),
    }));

    // Handle messages
    server.addEventListener('message', (event) => {
      this.handleMessage(userId, event.data, server);
    });

    // Handle disconnect
    server.addEventListener('close', () => {
      this.sessions.delete(userId);
      this.updatePresence(userId, 'offline');
    });

    return new Response(null, { status: 101, webSocket: client });
  }

  async handleMessage(userId: string, data: string, ws: WebSocket) {
    const message = JSON.parse(data);

    switch (message.type) {
      case 'presence_update':
        await this.updatePresence(userId, message.status);
        break;

      case 'typing':
        await this.broadcastTyping(userId, message.channelId);
        break;

      default:
        console.warn('Unknown message type:', message.type);
    }
  }

  async updatePresence(userId: string, status: PresenceStatus) {
    this.presence.set(userId, {
      status,
      lastSeen: Date.now(),
    });

    // Broadcast to all connected clients
    const update = {
      type: 'presence_update',
      userId,
      status,
    };

    for (const [id, ws] of this.sessions) {
      if (id !== userId) {
        ws.send(JSON.stringify(update));
      }
    }

    // Persist to storage
    await this.state.storage.put(`presence:${userId}`, {
      status,
      lastSeen: Date.now(),
    });
  }

  async broadcast(message: any) {
    for (const ws of this.sessions.values()) {
      ws.send(JSON.stringify(message));
    }
  }
}
```

#### 2. ChannelDurableObject
Manages channel-specific state:
- Real-time message delivery
- Active participants
- Typing indicators
- Message ordering

```typescript
export class ChannelDurableObject {
  state: DurableObjectState;
  sessions: Map<string, WebSocket>;
  typingUsers: Set<string>;
  messageSequence: number = 0;

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get('Upgrade') === 'websocket') {
      return this.handleWebSocket(request);
    }

    const url = new URL(request.url);
    if (url.pathname === '/message') {
      return this.deliverMessage(request);
    }

    return new Response('Not found', { status: 404 });
  }

  async deliverMessage(request: Request) {
    const message = await request.json();

    // Assign sequence number for ordering
    const sequence = ++this.messageSequence;
    await this.state.storage.put('messageSequence', sequence);

    const envelope = {
      type: 'message',
      sequence,
      ...message,
    };

    // Broadcast to all connected clients
    for (const ws of this.sessions.values()) {
      ws.send(JSON.stringify(envelope));
    }

    return Response.json({ success: true, sequence });
  }

  async handleTypingIndicator(userId: string, isTyping: boolean) {
    if (isTyping) {
      this.typingUsers.add(userId);
      // Auto-clear after 3 seconds
      setTimeout(() => this.typingUsers.delete(userId), 3000);
    } else {
      this.typingUsers.delete(userId);
    }

    // Broadcast typing state
    const update = {
      type: 'typing',
      users: Array.from(this.typingUsers),
    };

    for (const ws of this.sessions.values()) {
      ws.send(JSON.stringify(update));
    }
  }
}
```

#### 3. VoiceChannelDurableObject
Manages voice/video sessions:
- RealTimeKit session coordination
- Participant tracking
- Recording state

```typescript
export class VoiceChannelDurableObject {
  state: DurableObjectState;
  env: Env;
  participants: Map<string, ParticipantState>;
  rtkSession?: RTKSession;

  async initializeRTKSession(channelId: string) {
    const sessionConfig = {
      sessionId: `${this.state.id.toString()}-${channelId}`,
      maxParticipants: 100,
      features: ['recording', 'transcription'],
    };

    this.rtkSession = await createRTKSession(sessionConfig, this.env);
    await this.state.storage.put('rtkSession', this.rtkSession);

    return this.rtkSession;
  }

  async addParticipant(userId: string, metadata: any) {
    if (!this.rtkSession) {
      await this.initializeRTKSession(metadata.channelId);
    }

    this.participants.set(userId, {
      joinedAt: Date.now(),
      audioEnabled: true,
      videoEnabled: false,
      ...metadata,
    });

    // Broadcast updated participant list
    await this.broadcastParticipants();
  }

  async removeParticipant(userId: string) {
    this.participants.delete(userId);
    await this.broadcastParticipants();

    // End session if empty
    if (this.participants.size === 0) {
      await this.endSession();
    }
  }
}
```

## Consequences

### Positive

- **Strong consistency**: Single-threaded execution guarantees
- **Stateful WebSockets**: Long-lived connections without sticky sessions
- **Automatic persistence**: State.storage provides durable storage
- **Geographic routing**: DOs automatically migrate to active users
- **Simple mental model**: One DO per Waddle/channel
- **Built-in coordination**: No distributed locking needed
- **Cost-effective**: Only pay when DO is active

### Negative

- **Cold start latency**: Inactive DOs take ~100ms to wake
- **Single-threaded**: One request at a time per DO
- **Memory limits**: 128MB per DO
- **Request timeout**: 30s CPU time limit
- **Concurrent connection limit**: ~1000 WebSockets per DO
- **No autoscaling**: Must manually shard at scale
- **Regional constraints**: DO runs in single location at a time

### Mitigation Strategies

- **Warm-up requests**: Periodically ping DOs to keep warm
- **Sharding**: Split large Waddles across multiple DOs
- **Connection limits**: Enforce max connections per DO
- **Offload heavy work**: Use Workers or queues for compute
- **Graceful degradation**: Fall back to polling if WebSocket fails
- **DO routing**: Use consistent hashing for predictable routing

## Alternatives Considered

### Redis/Valkey for State

**Pros:** Mature, feature-rich, widely used
**Cons:** Not Cloudflare-native, latency to external service, cost

**Rejected because:** Conflicts with edge-native architecture.

### Workers KV for State

**Pros:** Simple, cheap, globally replicated
**Cons:** Eventual consistency, no WebSockets, high latency writes

**Rejected because:** Need strong consistency for real-time features.

### Stateful Workers with Hibernation API

**Pros:** Similar to Durable Objects, potentially simpler
**Cons:** Not yet available, immature

**Rejected because:** Durable Objects are production-ready.

### Centralized WebSocket Server

**Pros:** Simple architecture, easier debugging
**Cons:** Single point of failure, doesn't scale, high latency

**Rejected because:** Scalability and reliability concerns.

## Implementation Details

### DO Routing from Worker

```typescript
// chat-worker/index.ts
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    // Route to appropriate Durable Object
    if (url.pathname.startsWith('/ws/waddle/')) {
      const waddleId = url.pathname.split('/')[3];
      const doId = env.WADDLE_DO.idFromName(waddleId);
      const stub = env.WADDLE_DO.get(doId);
      return stub.fetch(request);
    }

    if (url.pathname.startsWith('/ws/channel/')) {
      const channelId = url.pathname.split('/')[3];
      const doId = env.CHANNEL_DO.idFromName(channelId);
      const stub = env.CHANNEL_DO.get(doId);
      return stub.fetch(request);
    }

    return new Response('Not found', { status: 404 });
  },
};
```

### Client-Side WebSocket Connection

```typescript
// client/websocket.ts
export class WaddleWebSocket {
  private ws?: WebSocket;
  private waddleId: string;
  private reconnectAttempts = 0;

  constructor(waddleId: string) {
    this.waddleId = waddleId;
  }

  async connect() {
    const token = await getAuthToken();
    this.ws = new WebSocket(
      `wss://api.waddle.social/ws/waddle/${this.waddleId}`,
      [token]
    );

    this.ws.onopen = () => {
      console.log('WebSocket connected');
      this.reconnectAttempts = 0;
    };

    this.ws.onmessage = (event) => {
      const message = JSON.parse(event.data);
      this.handleMessage(message);
    };

    this.ws.onclose = () => {
      console.log('WebSocket closed');
      this.reconnect();
    };

    this.ws.onerror = (error) => {
      console.error('WebSocket error:', error);
    };
  }

  private reconnect() {
    if (this.reconnectAttempts >= 5) {
      console.error('Max reconnection attempts reached');
      return;
    }

    const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempts), 30000);
    setTimeout(() => {
      this.reconnectAttempts++;
      this.connect();
    }, delay);
  }

  send(type: string, data: any) {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type, ...data }));
    }
  }

  updatePresence(status: PresenceStatus) {
    this.send('presence_update', { status });
  }

  sendTypingIndicator(channelId: string, isTyping: boolean) {
    this.send('typing', { channelId, isTyping });
  }
}
```

## Sharding Strategy

When a Waddle or channel exceeds DO limits:

1. **Split by feature**: Separate DOs for presence, messages, voice
2. **Geographic sharding**: DOs per region for large global communities
3. **Hash-based sharding**: Distribute users across multiple DOs

```typescript
function getShardedDOId(waddleId: string, userId: string, shardCount: number): string {
  const hash = hashString(userId);
  const shardIndex = hash % shardCount;
  return `${waddleId}-shard-${shardIndex}`;
}
```

## Monitoring

Track these metrics:

- Active DO count
- WebSocket connections per DO
- Message latency (send to receive)
- Presence update latency
- DO cold start frequency
- Connection errors and reconnects

## References

- [Cloudflare Durable Objects](https://developers.cloudflare.com/durable-objects/)
- [WebSocket API](https://developers.cloudflare.com/durable-objects/api/websockets/)
- [DO Best Practices](https://developers.cloudflare.com/durable-objects/best-practices/)