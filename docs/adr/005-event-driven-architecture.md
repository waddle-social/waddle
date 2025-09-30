# ADR-005: Event-Driven Architecture with Pub/Sub

**Status:** Accepted

**Date:** 2025-09-30

## Context

Waddle's architecture consists of multiple independent feature workers that need to react to events across the system:

- **Chat Worker**: Publishes message.created, message.edited, message.deleted
- **AI Worker**: Subscribes to messages for tagging and conversation grouping
- **Integration Worker**: Publishes external.rss_item, external.github_pr
- **Views Worker**: Subscribes to tag changes to update materialized views
- **Notification Worker**: Subscribes to mentions and DMs

We need an event system that supports:

- Asynchronous, decoupled communication between workers
- At-least-once delivery guarantees
- Event filtering by topic/type
- Scalable event distribution
- Event history and replay for debugging

## Decision

We will implement an **event-driven architecture** using a pub/sub system where:

1. **All domain events are published** to a central event bus
2. **Workers subscribe to events they care about** (declarative subscriptions)
3. **Events are immutable** and contain full context
4. **Event schema is versioned** for backward compatibility

### Architecture

```
┌─────────────────────────────────────────────────┐
│  Event Publishers (Feature Workers)             │
│  - Chat Worker                                   │
│  - Integration Worker                            │
│  - Moderation Worker                             │
└───────────────┬─────────────────────────────────┘
                │ Publish events
                ▼
┌─────────────────────────────────────────────────┐
│  Event Bus                                       │
│  (Cloudflare Queues or alternative)              │
│  - Topic-based routing                           │
│  - Event persistence                             │
│  - Delivery guarantees                           │
└───────┬───────┬───────┬───────┬─────────────────┘
        │       │       │       │
        │       │       │       │ Subscribe
        ▼       ▼       ▼       ▼
    ┌─────┐ ┌─────┐ ┌─────┐ ┌─────┐
    │ AI  │ │Views│ │Notif│ │Anal│  Event Consumers
    └─────┘ └─────┘ └─────┘ └─────┘
```

### Event Schema

```typescript
interface DomainEvent<T = any> {
  id: string;                    // Unique event ID
  type: string;                  // e.g., "message.created"
  version: string;               // Schema version "1.0.0"
  source: string;                // Publishing worker
  waddleId: string;             // Context
  userId?: string;              // Actor
  timestamp: string;            // ISO 8601
  data: T;                      // Event-specific payload
  metadata: {
    correlationId?: string;     // For tracing
    causationId?: string;       // Event that caused this
    [key: string]: any;
  };
}
```

### Event Types

```typescript
// Message Events
interface MessageCreatedEvent extends DomainEvent {
  type: 'message.created';
  data: {
    messageId: string;
    channelId?: string;
    content: string;
    threadId?: string;
    attachments: string[];
  };
}

interface MessageEditedEvent extends DomainEvent {
  type: 'message.edited';
  data: {
    messageId: string;
    oldContent: string;
    newContent: string;
  };
}

// User Events
interface UserJoinedWaddleEvent extends DomainEvent {
  type: 'user.joined_waddle';
  data: {
    waddleId: string;
    userId: string;
    role: string;
  };
}

// Integration Events
interface RssItemPublishedEvent extends DomainEvent {
  type: 'integration.rss_item';
  data: {
    feedUrl: string;
    title: string;
    link: string;
    publishedAt: string;
  };
}

interface GitHubPREvent extends DomainEvent {
  type: 'integration.github_pr';
  data: {
    repository: string;
    prNumber: number;
    action: 'opened' | 'closed' | 'merged';
    title: string;
    url: string;
  };
}

// AI Events
interface ConversationGroupedEvent extends DomainEvent {
  type: 'ai.conversation_grouped';
  data: {
    conversationId: string;
    messageIds: string[];
    tags: string[];
    confidence: number;
  };
}
```

## Consequences

### Positive

- **Decoupling**: Workers don't need to know about each other
- **Scalability**: Events processed asynchronously, don't block requests
- **Extensibility**: Add new subscribers without modifying publishers
- **Resilience**: Failed processing can retry without data loss
- **Audit trail**: Complete event history for debugging and compliance
- **Temporal decoupling**: Publishers and subscribers operate independently
- **Load leveling**: Event queue smooths traffic spikes

### Negative

- **Eventual consistency**: State updates aren't immediate
- **Complexity**: Distributed system debugging is harder
- **Ordering challenges**: Events may arrive out of order
- **Duplicate processing**: At-least-once delivery means duplicates possible
- **Event schema management**: Breaking changes require careful versioning
- **Operational overhead**: Additional infrastructure to monitor

### Mitigation Strategies

- **Idempotency**: All event handlers must be idempotent
- **Event versioning**: Support multiple schema versions simultaneously
- **Dead letter queues**: Capture failed events for investigation
- **Monitoring**: Track event processing lag and failure rates
- **Correlation IDs**: Trace events across system for debugging
- **Schema registry**: Central registry for event type definitions

## Alternatives Considered

### Synchronous Service Calls

**Pros:** Simple, immediate consistency, easy to debug
**Cons:** Tight coupling, cascading failures, poor scalability

**Rejected because:** Violates our independence and scalability goals.

### Webhooks

**Pros:** Simple HTTP callbacks, widely understood
**Cons:** No delivery guarantees, complex retry logic, firewall issues

**Rejected because:** Insufficient reliability for critical events.

### Database Polling (Outbox Pattern)

**Pros:** Guaranteed delivery, leverages existing DB
**Cons:** Inefficient, adds DB load, delayed processing

**Rejected because:** Performance concerns at scale.

### Kafka/RabbitMQ

**Pros:** Mature, feature-rich, proven at scale
**Cons:** Not Cloudflare-native, operational complexity, cost

**Rejected because:** Conflicts with edge-native architecture.

## Implementation Details

### Event Publisher (Chat Worker)

```typescript
// chat-worker/events.ts
import { publishEvent } from '@waddle/events';

export async function createMessage(
  content: string,
  userId: string,
  waddleId: string,
  channelId: string,
  env: Env
) {
  // 1. Persist message to DB
  const messageId = crypto.randomUUID();
  const waddleDb = await getWaddleDb(waddleId, env);

  await waddleDb.prepare(`
    INSERT INTO messages (id, channel_id, user_id, content, created_at)
    VALUES (?, ?, ?, ?, ?)
  `).bind(messageId, channelId, userId, content, new Date().toISOString()).run();

  // 2. Publish event
  await publishEvent(env.EVENT_BUS, {
    id: crypto.randomUUID(),
    type: 'message.created',
    version: '1.0.0',
    source: 'chat-worker',
    waddleId,
    userId,
    timestamp: new Date().toISOString(),
    data: {
      messageId,
      channelId,
      content,
      threadId: null,
      attachments: [],
    },
    metadata: {
      correlationId: env.REQUEST_ID,
    },
  });

  return { messageId };
}
```

### Event Subscriber (AI Worker)

```typescript
// ai-worker/index.ts
export default {
  async queue(batch: MessageBatch<DomainEvent>, env: Env) {
    for (const message of batch.messages) {
      const event = message.body;

      try {
        // Route to appropriate handler
        switch (event.type) {
          case 'message.created':
            await handleMessageCreated(event as MessageCreatedEvent, env);
            break;

          case 'message.edited':
            await handleMessageEdited(event as MessageEditedEvent, env);
            break;

          default:
            console.warn(`Unhandled event type: ${event.type}`);
        }

        // Acknowledge successful processing
        message.ack();
      } catch (error) {
        console.error(`Failed to process event ${event.id}:`, error);
        // Message will be retried
        message.retry();
      }
    }
  },
};

async function handleMessageCreated(event: MessageCreatedEvent, env: Env) {
  // Analyze message for tags
  const tags = await analyzeMessageForTags(event.data.content, env);

  // Store tags in Waddle DB
  const waddleDb = await getWaddleDb(event.waddleId, env);
  for (const tag of tags) {
    await waddleDb.prepare(`
      INSERT INTO message_tags (message_id, tag, confidence)
      VALUES (?, ?, ?)
    `).bind(event.data.messageId, tag.name, tag.confidence).run();
  }

  // Attempt conversation grouping
  const conversation = await findOrCreateConversation(
    event.data.messageId,
    tags,
    event.waddleId,
    env
  );

  // Publish conversation grouped event
  await publishEvent(env.EVENT_BUS, {
    id: crypto.randomUUID(),
    type: 'ai.conversation_grouped',
    version: '1.0.0',
    source: 'ai-worker',
    waddleId: event.waddleId,
    timestamp: new Date().toISOString(),
    data: {
      conversationId: conversation.id,
      messageIds: conversation.messageIds,
      tags: tags.map(t => t.name),
      confidence: Math.min(...tags.map(t => t.confidence)),
    },
    metadata: {
      correlationId: event.metadata.correlationId,
      causationId: event.id,
    },
  });
}
```

### Event Bus Abstraction

```typescript
// shared/events/publisher.ts
export async function publishEvent(
  eventBus: Queue<DomainEvent>,
  event: DomainEvent
): Promise<void> {
  // Validate event schema
  validateEvent(event);

  // Publish to queue
  await eventBus.send(event);

  // Optionally: Store in event log for history
  // await storeEventInLog(event, env);
}

function validateEvent(event: DomainEvent): void {
  if (!event.id || !event.type || !event.version || !event.timestamp) {
    throw new Error('Invalid event: missing required fields');
  }

  // Validate event type is registered
  if (!EVENT_REGISTRY.has(event.type)) {
    throw new Error(`Unknown event type: ${event.type}`);
  }

  // Validate against schema
  const schema = EVENT_REGISTRY.get(event.type);
  if (!schema.validate(event.data)) {
    throw new Error(`Invalid event data for type ${event.type}`);
  }
}
```

### Subscription Configuration

```typescript
// wrangler.toml for ai-worker
[[queues.consumers]]
queue = "waddle-events"
max_batch_size = 10
max_batch_timeout = 5
max_retries = 3
dead_letter_queue = "waddle-events-dlq"

# Event filtering (if supported)
[queues.consumers.filters]
types = ["message.created", "message.edited", "message.deleted"]
```

## Event Pub/Sub Technology Decision

**Primary Option: Cloudflare Queues**
- Native integration with Workers
- Automatic scaling
- Simple pricing
- At-least-once delivery

**Alternative: Custom Event Bus**
- D1 as event log
- Workers poll for new events
- More control, more complexity

**Decision:** Start with **Cloudflare Queues**, evaluate alternatives if limitations arise.

## Monitoring & Observability

Track these metrics:

- Events published per second by type
- Event processing lag (time from publish to consume)
- Failed event processing rate
- Dead letter queue size
- Event size distribution
- Subscription processing time (p50, p95, p99)

## Migration Strategy

### Phase 1: Add Events, Keep Sync
- Publish events alongside synchronous calls
- No subscribers yet
- Validate event publishing works

### Phase 2: First Subscribers
- AI worker subscribes to message events
- Keep existing sync calls as backup
- Monitor consistency

### Phase 3: Remove Sync Calls
- Migrate all features to events
- Remove direct service calls
- Full event-driven

### Phase 4: Event Sourcing
- Consider event sourcing for critical entities
- Event replay for recovery
- CQRS patterns

## References

- [Cloudflare Queues](https://developers.cloudflare.com/queues/)
- [Event-Driven Architecture Patterns](https://martinfowler.com/articles/201701-event-driven.html)
- [Domain Events Pattern](https://learn.microsoft.com/en-us/dotnet/architecture/microservices/microservice-ddd-cqrs-patterns/domain-events-design-implementation)